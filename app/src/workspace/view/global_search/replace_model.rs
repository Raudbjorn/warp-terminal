//! Runs a replace across the files of a project for global search, and takes the last one back.
//!
//! Files that are not open are rewritten in the background. Files open in an editor are edited
//! through their buffer on the main thread, so that the editor shows the change and whatever it
//! holds unsaved is kept.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use file_write_guard::WriteGuard;
use futures::FutureExt as _;
use futures::future::{Either, Shared};
use instant::Instant;
use project_replace::apply::{
    AppliedBufferEdits, DEFAULT_MAX_FILE_BYTES, FileTarget, OpenBuffer, Progress, ReplaceError,
    ReplaceOutcome, ReplaceReport, ReplaceRequest, replace_in_files,
};
use project_replace::engine::SearchSpec;
use project_replace::journal::{Journal, JournalLimits, TransactionDescription};
use project_replace::replacement::ReplaceSpec;
use project_replace::report::{RefusalReason, RefusedFile, SkipReason, SkippedFile};
use project_replace::scope::ScopeGlobs;
use project_replace::undo::{UndoOutcome, UndoReport, undo_latest};
use warp_editor::content::diff::text_diff;
use warp_files::{FileModel, SaveFuture};
use warp_util::file::FileId;
use warp_util::file::FileSaveError;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use crate::code::global_buffer_model::{BufferTextEditError, GlobalBufferModel};

/// Directory, below the app's state directory, that holds the copies kept for undoing.
const JOURNAL_DIRECTORY: &str = "project_replace_journal";

/// How often the progress of a run is reported while it goes through its files.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// One deadline for the entire batch; pending I/O remains quarantined after the UI stops waiting.
const SAVE_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const SAVE_CANCEL_POLL: Duration = Duration::from_millis(100);
type ObservedSave = Shared<SaveFuture>;

/// Set while a replace or an undo runs anywhere in the app. Two of them at once could each
/// change a file the other is about to check.
static RUN_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

struct RunPermit(&'static AtomicBool);

impl RunPermit {
    fn acquire(lock: &'static AtomicBool) -> Option<Arc<Self>> {
        lock.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Arc::new(Self(lock)))
    }
}

impl Drop for RunPermit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// What to replace, with what, and where.
pub struct ReplaceJob {
    pub search: SearchSpec,
    pub replace: ReplaceSpec,
    /// The local folders that were searched.
    pub roots: Vec<PathBuf>,
    pub scope: ScopeGlobs,
    pub files: Vec<FileTarget>,
}

pub enum ProjectReplaceEvent {
    /// A run started, got further, or was asked to stop.
    Progress,
    Finished(ReplaceReport),
    UndoFinished {
        /// What the replace that was taken back had replaced.
        description: TransactionDescription,
        report: UndoReport,
    },
    /// No replace is left to take back.
    NothingToUndo,
    /// The run could not start or stopped before changing anything.
    Failed(String),
}

enum RunState {
    Idle,
    Replacing {
        progress: Option<Progress>,
        cancel: Arc<AtomicBool>,
    },
    Undoing {
        progress: Option<Progress>,
        cancel: Arc<AtomicBool>,
    },
}

/// The edits that turn the text of an editor back into what it was before a replace.
type RestoringEdits = Vec<(Range<usize>, String)>;

/// A file that was open in an editor when a run started.
struct OpenFile {
    file_id: FileId,
    has_unsaved_changes: bool,
}

pub struct ProjectReplace {
    state: RunState,
    permit: Option<Arc<RunPermit>>,
}

impl Entity for ProjectReplace {
    type Event = ProjectReplaceEvent;
}

impl ProjectReplace {
    pub fn new() -> Self {
        Self {
            state: RunState::Idle,
            permit: None,
        }
    }

    pub fn is_running(&self) -> bool {
        match self.state {
            RunState::Idle => false,
            RunState::Replacing { .. } | RunState::Undoing { .. } => true,
        }
    }

    /// Whether the current run takes a replace back rather than making one.
    pub fn is_undoing(&self) -> bool {
        match self.state {
            RunState::Idle | RunState::Replacing { .. } => false,
            RunState::Undoing { .. } => true,
        }
    }

    /// How far the current run got, once it reported anything.
    pub fn progress(&self) -> Option<Progress> {
        match &self.state {
            RunState::Idle => None,
            RunState::Replacing { progress, .. } | RunState::Undoing { progress, .. } => *progress,
        }
    }

    /// Stops the current run before its next file. The files a replace already changed stay
    /// changed and can be undone, and the files an undo did not get to can be undone later.
    pub fn cancel(&mut self, ctx: &mut ModelContext<Self>) {
        match &self.state {
            RunState::Idle => {}
            RunState::Replacing { cancel, .. } | RunState::Undoing { cancel, .. } => {
                cancel.store(true, Ordering::Relaxed);
                ctx.emit(ProjectReplaceEvent::Progress);
            }
        }
    }

    pub fn start(&mut self, job: ReplaceJob, ctx: &mut ModelContext<Self>) {
        let Some(journal) = journal() else {
            ctx.emit(ProjectReplaceEvent::Failed(
                "There is nowhere to keep the copies needed to undo a replace.".to_string(),
            ));
            return;
        };
        let Some(permit) = RunPermit::acquire(&RUN_IN_PROGRESS) else {
            ctx.emit(ProjectReplaceEvent::Failed(
                "Another replace is still running.".to_string(),
            ));
            return;
        };
        self.permit = Some(permit);

        let paths = job.files.iter().map(|target| target.path.clone()).collect();
        let (open_buffers, open_files) = open_buffers_for_paths(&paths, ctx);
        let request = ReplaceRequest {
            search: job.search,
            replace: job.replace,
            roots: job.roots,
            scope: job.scope,
            files: job.files,
            open_buffers,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let (progress_tx, progress_rx) = async_channel::bounded(1);
        ctx.spawn_stream_local(progress_rx, Self::handle_progress, |_, _| {});
        let permit = self.permit.clone();
        ctx.spawn(
            {
                let cancel = cancel.clone();
                async move {
                    let _permit = permit;
                    let report_progress = throttled(progress_tx);
                    let result = replace_in_files(&request, &journal, &report_progress, &cancel);
                    (request, result)
                }
            },
            move |me, (request, result), ctx| {
                me.handle_files_replaced(request, result, open_files, ctx);
            },
        );

        self.state = RunState::Replacing {
            progress: None,
            cancel,
        };
        ctx.emit(ProjectReplaceEvent::Progress);
    }

    fn cancellation(&self) -> Arc<AtomicBool> {
        match &self.state {
            RunState::Replacing { cancel, .. } | RunState::Undoing { cancel, .. } => cancel.clone(),
            RunState::Idle => Arc::new(AtomicBool::new(true)),
        }
    }

    fn handle_progress(&mut self, latest: Progress, ctx: &mut ModelContext<Self>) {
        match &mut self.state {
            RunState::Idle => {}
            RunState::Replacing { progress, .. } | RunState::Undoing { progress, .. } => {
                *progress = Some(latest);
                ctx.emit(ProjectReplaceEvent::Progress);
            }
        }
    }

    /// Applies the edits of the files that are open in an editor, now that the files that are not
    /// have been rewritten, then closes the journal of the run.
    fn handle_files_replaced(
        &mut self,
        request: ReplaceRequest,
        result: Result<ReplaceOutcome, ReplaceError>,
        open_files: HashMap<PathBuf, OpenFile>,
        ctx: &mut ModelContext<Self>,
    ) {
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(err) => {
                self.finish_run();
                ctx.emit(ProjectReplaceEvent::Failed(err.to_string()));
                return;
            }
        };

        let deadline = Instant::now() + SAVE_WAIT_TIMEOUT;
        let cancel = self.cancellation();
        let transaction = outcome.transaction_directory();
        let mut pending = Vec::new();
        let mut skipped = Vec::new();
        for buffer_edits in &outcome.buffer_edits {
            let open_buffer = request
                .open_buffers
                .iter()
                .find(|buffer| buffer.path == buffer_edits.buffer_path);
            let (Some(open_buffer), Some(open_file)) =
                (open_buffer, open_files.get(&buffer_edits.buffer_path))
            else {
                continue;
            };

            let edits = buffer_edits
                .edits
                .iter()
                .map(|edit| (edit.range.clone(), edit.replacement.clone()))
                .collect();
            // A buffer that was in step with its file is saved, so that it stays in step. One
            // with unsaved changes keeps them unsaved, along with the replacement.
            let guard = match reserve_save(
                open_file.file_id,
                !open_file.has_unsaved_changes,
                &transaction,
                &cancel,
                ctx,
            ) {
                Ok(guard) => guard,
                Err(error) => {
                    skipped.push(SkippedFile {
                        path: buffer_edits.path.clone(),
                        reason: SkipReason::WriteFailed(error),
                    });
                    continue;
                }
            };
            let edited = GlobalBufferModel::handle(ctx).update(ctx, |model, ctx| {
                model.apply_text_edits(
                    open_file.file_id,
                    &open_buffer.text,
                    edits,
                    guard.as_ref(),
                    ctx,
                )
            });
            match edited {
                Ok((text_after, completion)) => pending.push((
                    AppliedBufferEdits {
                        path: buffer_edits.path.clone(),
                        text_before: open_buffer.text.clone(),
                        text_after,
                        occurrences: buffer_edits.edits.len(),
                        save_error: None,
                    },
                    observe_save(completion, guard.as_ref(), ctx),
                    guard,
                )),
                Err(
                    BufferTextEditError::NotLoaded
                    | BufferTextEditError::TextChanged
                    | BufferTextEditError::PendingWrite,
                ) => {
                    if let Some(guard) = guard {
                        guard.cancel_before_dispatch();
                    }
                    skipped.push(SkippedFile {
                        path: buffer_edits.path.clone(),
                        reason: SkipReason::EditedWhileReplacing,
                    });
                }
            }
        }

        let permit = self.permit.clone();
        ctx.spawn(
            async move {
                let _permit = permit;
                let mut applied = Vec::new();
                let mut guards = Vec::new();
                for (mut edits, completion, guard) in pending {
                    edits.save_error = await_save(completion, deadline, &cancel).await.err();
                    applied.push(edits);
                    guards.extend(guard);
                }
                let report = outcome.finish(applied, skipped);
                for guard in guards {
                    guard.journal_finished();
                }
                report
            },
            |me, report, ctx| {
                me.finish_run();
                ctx.emit(ProjectReplaceEvent::Finished(report));
            },
        );
    }

    /// Takes back the most recent replace that still has something to take back.
    pub fn undo(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(journal) = journal() else {
            ctx.emit(ProjectReplaceEvent::NothingToUndo);
            return;
        };
        let Some(permit) = RunPermit::acquire(&RUN_IN_PROGRESS) else {
            ctx.emit(ProjectReplaceEvent::Failed(
                "Another replace is still running.".to_string(),
            ));
            return;
        };
        self.permit = Some(permit);

        let cancel = Arc::new(AtomicBool::new(false));
        self.state = RunState::Undoing {
            progress: None,
            cancel: cancel.clone(),
        };
        ctx.emit(ProjectReplaceEvent::Progress);

        // The journal says which files the replace touched, and with that, which editors have
        // to be looked at before anything is restored.
        let permit = self.permit.clone();
        ctx.spawn(
            {
                let journal = journal.clone();
                async move {
                    let _permit = permit;
                    journal.latest().map(|transaction| {
                        transaction.map(|transaction| {
                            transaction
                                .entries
                                .iter()
                                .map(|entry| entry.path.clone())
                                .collect::<HashSet<PathBuf>>()
                        })
                    })
                }
            },
            move |me, paths: io::Result<Option<HashSet<PathBuf>>>, ctx| {
                me.handle_undo_planned(journal, paths, cancel, ctx);
            },
        );
    }

    fn handle_undo_planned(
        &mut self,
        journal: Journal,
        paths: io::Result<Option<HashSet<PathBuf>>>,
        cancel: Arc<AtomicBool>,
        ctx: &mut ModelContext<Self>,
    ) {
        let paths = match paths {
            Ok(Some(paths)) => paths,
            Ok(None) => {
                self.finish_run();
                ctx.emit(ProjectReplaceEvent::NothingToUndo);
                return;
            }
            Err(err) => {
                self.finish_run();
                ctx.emit(ProjectReplaceEvent::Failed(err.to_string()));
                return;
            }
        };
        let (open_buffers, open_files) = open_buffers_for_paths(&paths, ctx);

        let (progress_tx, progress_rx) = async_channel::bounded(1);
        ctx.spawn_stream_local(progress_rx, Self::handle_progress, |_, _| {});
        let permit = self.permit.clone();
        ctx.spawn(
            async move {
                let _permit = permit;
                let outcome = {
                    let report_progress = throttled(progress_tx);
                    undo_latest(&journal, &open_buffers, &report_progress, &cancel)
                };

                let texts: Vec<(String, String)> = match &outcome {
                    Ok(Some(outcome)) => outcome
                        .buffer_restores
                        .iter()
                        .map(|restore| {
                            (restore.current_text.clone(), restore.restored_text.clone())
                        })
                        .collect(),
                    Ok(None) | Err(_) => Vec::new(),
                };
                let mut restoring_edits: Vec<RestoringEdits> = Vec::new();
                for (current_text, restored_text) in texts {
                    restoring_edits.push(text_diff(&current_text, &restored_text).await.edits);
                }
                (outcome, restoring_edits)
            },
            move |me, (outcome, restoring_edits), ctx| {
                me.handle_files_restored(outcome, restoring_edits, open_files, ctx);
            },
        );
    }

    /// Puts back the text of the files that are open in an editor, now that the files that are
    /// not have been restored, then drops what was undone from the journal.
    fn handle_files_restored(
        &mut self,
        outcome: io::Result<Option<UndoOutcome>>,
        restoring_edits: Vec<RestoringEdits>,
        open_files: HashMap<PathBuf, OpenFile>,
        ctx: &mut ModelContext<Self>,
    ) {
        let outcome = match outcome {
            Ok(Some(outcome)) => outcome,
            Ok(None) => {
                self.finish_run();
                ctx.emit(ProjectReplaceEvent::NothingToUndo);
                return;
            }
            Err(err) => {
                self.finish_run();
                ctx.emit(ProjectReplaceEvent::Failed(err.to_string()));
                return;
            }
        };

        let deadline = Instant::now() + SAVE_WAIT_TIMEOUT;
        let cancel = self.cancellation();
        let transaction = outcome.transaction_directory();
        let mut pending = Vec::new();
        let mut refused = Vec::new();
        for (restore, edits) in outcome.buffer_restores.iter().zip(restoring_edits) {
            let Some(open_file) = open_files.get(&restore.buffer_path) else {
                continue;
            };
            let guard =
                match reserve_save(open_file.file_id, restore.save, &transaction, &cancel, ctx) {
                    Ok(guard) => guard,
                    Err(error) => {
                        refused.push(RefusedFile {
                            path: restore.buffer_path.clone(),
                            reason: RefusalReason::WriteFailed(error),
                        });
                        continue;
                    }
                };
            // As when replacing: an editor in step with its file stays in step with it.
            let edited = GlobalBufferModel::handle(ctx).update(ctx, |model, ctx| {
                model.apply_text_edits(
                    open_file.file_id,
                    &restore.current_text,
                    edits,
                    guard.as_ref(),
                    ctx,
                )
            });
            match edited {
                Ok((_, completion)) => pending.push((
                    restore.buffer_path.clone(),
                    observe_save(completion, guard.as_ref(), ctx),
                    guard,
                )),
                Err(
                    BufferTextEditError::NotLoaded
                    | BufferTextEditError::TextChanged
                    | BufferTextEditError::PendingWrite,
                ) => {
                    if let Some(guard) = guard {
                        guard.cancel_before_dispatch();
                    }
                    refused.push(RefusedFile {
                        path: restore.buffer_path.clone(),
                        reason: RefusalReason::EditedSinceReplace,
                    });
                }
            }
        }

        let description = outcome.description.clone();
        let permit = self.permit.clone();
        ctx.spawn(
            async move {
                let _permit = permit;
                let mut restored = Vec::new();
                let mut guards = Vec::new();
                for (path, completion, guard) in pending {
                    guards.extend(guard);
                    match await_save(completion, deadline, &cancel).await {
                        Ok(()) => restored.push(path),
                        Err(error) => refused.push(RefusedFile {
                            path,
                            reason: RefusalReason::WriteFailed(format!(
                                "buffer restored but remains unsaved: {error}"
                            )),
                        }),
                    }
                }
                let report = outcome.finish(&restored, refused);
                for guard in guards {
                    guard.journal_finished();
                }
                report
            },
            move |me, report, ctx| {
                me.finish_run();
                ctx.emit(ProjectReplaceEvent::UndoFinished {
                    description,
                    report,
                });
            },
        );
    }

    fn finish_run(&mut self) {
        self.state = RunState::Idle;
        self.permit = None;
    }
}

impl Default for ProjectReplace {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ProjectReplace {
    /// Keep the permit in the worker until it stops, even if its view disappears.
    fn drop(&mut self) {
        match &self.state {
            RunState::Idle => {}
            RunState::Replacing { cancel, .. } | RunState::Undoing { cancel, .. } => {
                cancel.store(true, Ordering::Relaxed);
            }
        }
    }
}

fn reserve_save(
    file_id: FileId,
    save: bool,
    transaction: &std::path::Path,
    cancel: &AtomicBool,
    ctx: &AppContext,
) -> Result<Option<WriteGuard>, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled before changing the buffer".to_owned());
    }
    let path = FileModel::as_ref(ctx)
        .file_path(file_id)
        .ok_or("the file is no longer open")?;
    if save {
        WriteGuard::acquire(path, transaction.to_path_buf())
            .map(Some)
            .map_err(str::to_owned)
    } else if file_write_guard::is_quarantined(&path) {
        Err("a previous save for this file is still pending".to_owned())
    } else {
        Ok(None)
    }
}

fn observe_save(
    completion: SaveFuture,
    guard: Option<&WriteGuard>,
    ctx: &AppContext,
) -> ObservedSave {
    let observed = completion.shared();
    if let Some(guard) = guard {
        let guard = guard.clone();
        let monitor = observed.clone();
        // Backing I/O cannot be cancelled; this bounded monitor must outlive the initiating model.
        ctx.background_executor()
            .spawn_boxed(Box::pin(monitor_save(monitor, guard)))
            .detach();
    }
    observed
}

async fn monitor_save(completion: ObservedSave, guard: WriteGuard) {
    match completion.await {
        Err(error) if matches!(error.as_ref(), FileSaveError::CompletionLost) => {
            log::warn!(
                "save completion lost; retaining file quarantine and undo journal until restart"
            );
        }
        _ => guard.write_finished(),
    }
}

async fn await_save(
    completion: ObservedSave,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<(), String> {
    loop {
        if let Some(result) = completion.clone().now_or_never() {
            return result.map_err(|error| error.to_string());
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(
                "stopped waiting; the pending save and its undo journal remain protected"
                    .to_owned(),
            );
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(
                "timed out waiting; the pending save and its undo journal remain protected"
                    .to_owned(),
            );
        }
        match futures::future::select(
            completion.clone(),
            Box::pin(async_io::Timer::after(remaining.min(SAVE_CANCEL_POLL))),
        )
        .await
        {
            Either::Left((result, _)) => return result.map_err(|error| error.to_string()),
            Either::Right(_) => {}
        }
    }
}

/// Passes on the progress of a run often enough to follow it, without a report per file.
fn throttled(progress_tx: async_channel::Sender<Progress>) -> impl Fn(Progress) {
    let last_report = Cell::new(None::<Instant>);
    move |progress| {
        let is_due = last_report
            .get()
            .is_none_or(|last| last.elapsed() >= PROGRESS_INTERVAL);
        if is_due || progress.done == progress.total {
            last_report.set(Some(Instant::now()));
            let _ = progress_tx.try_send(progress);
        }
    }
}

fn journal() -> Option<Journal> {
    let state_directory = warp_core::paths::state_dir();
    // Without a state directory the journal would land wherever the app happens to run from.
    if state_directory.as_os_str().is_empty() {
        return None;
    }
    Some(Journal::new(
        state_directory.join(JOURNAL_DIRECTORY),
        JournalLimits::default(),
    ))
}

/// Snapshot only target buffers, including files opened through a differently named symlink.
fn open_buffers_for_paths(
    paths: &HashSet<PathBuf>,
    ctx: &AppContext,
) -> (Vec<OpenBuffer>, HashMap<PathBuf, OpenFile>) {
    let paths: HashSet<PathBuf> = paths
        .iter()
        .map(|path| std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()))
        .collect();
    let mut open_buffers = Vec::new();
    let mut open_files = HashMap::new();
    for loaded in GlobalBufferModel::as_ref(ctx).loaded_local_buffers(ctx) {
        let canonical = std::fs::canonicalize(&loaded.path).unwrap_or_else(|_| loaded.path.clone());
        if !paths.contains(&canonical) {
            continue;
        }
        open_buffers.push(OpenBuffer {
            path: loaded.path.clone(),
            text: loaded.buffer.as_ref(ctx).text().into_string(),
            has_unsaved_changes: loaded.has_unsaved_changes,
        });
        open_files.insert(
            loaded.path,
            OpenFile {
                file_id: loaded.file_id,
                has_unsaved_changes: loaded.has_unsaved_changes,
            },
        );
    }
    (open_buffers, open_files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_the_model_keeps_the_run_exclusive_until_the_worker_finishes() {
        static LOCK: AtomicBool = AtomicBool::new(false);
        let model_permit = RunPermit::acquire(&LOCK).unwrap();
        let worker_permit = model_permit.clone();
        drop(model_permit);
        assert!(RunPermit::acquire(&LOCK).is_none());
        drop(worker_permit);
        assert!(RunPermit::acquire(&LOCK).is_some());
    }
    #[test]
    fn timeout_and_cancel_release_the_run_but_not_a_late_writes_quarantine() {
        for cancelled in [false, true] {
            static LOCK: AtomicBool = AtomicBool::new(false);
            let path = PathBuf::from(format!("pending-save-{cancelled}"));
            let transaction = PathBuf::from(format!("journal-save-{cancelled}"));
            let guard = WriteGuard::acquire(path.clone(), transaction.clone()).unwrap();
            let (tx, rx) = futures::channel::oneshot::channel();
            let completion: SaveFuture = Box::pin(async move {
                rx.await
                    .unwrap_or_else(|_| Err(Arc::new(FileSaveError::CompletionLost)))
            });
            let observed = completion.shared();
            let monitor = monitor_save(observed.clone(), guard.clone());
            let permit = RunPermit::acquire(&LOCK).unwrap();
            let cancel = AtomicBool::new(cancelled);
            let deadline = if cancelled {
                Instant::now() + SAVE_WAIT_TIMEOUT
            } else {
                Instant::now() + Duration::from_millis(5)
            };
            assert!(
                futures::executor::block_on(await_save(observed.clone(), deadline, &cancel))
                    .is_err()
            );
            assert!(
                await_save(observed, deadline, &cancel)
                    .now_or_never()
                    .is_some(),
                "later files reuse the expired/cancelled batch deadline"
            );
            // The phase retains the failed/pending journal entries before releasing its run permit.
            guard.journal_finished();
            drop(permit);
            assert!(RunPermit::acquire(&LOCK).is_some());
            assert!(file_write_guard::is_quarantined(&path));
            assert!(file_write_guard::transaction_is_pinned(&transaction));
            let unrelated = WriteGuard::acquire(
                PathBuf::from(format!("unrelated-{cancelled}")),
                PathBuf::from("unrelated-journal"),
            )
            .unwrap();
            unrelated.cancel_before_dispatch();
            tx.send(Ok(())).unwrap();
            futures::executor::block_on(monitor);
            assert!(!file_write_guard::is_quarantined(&path));
            assert!(!file_write_guard::transaction_is_pinned(&transaction));
        }
    }

    #[test]
    fn a_lost_completion_keeps_the_path_and_journal_quarantined() {
        let path = PathBuf::from("lost-save");
        let transaction = PathBuf::from("lost-save-journal");
        let guard = WriteGuard::acquire(path.clone(), transaction.clone()).unwrap();
        let completion: SaveFuture =
            Box::pin(async { Err(Arc::new(FileSaveError::CompletionLost)) });
        guard.journal_finished();
        futures::executor::block_on(monitor_save(completion.shared(), guard.clone()));
        assert!(file_write_guard::is_quarantined(&path));
        assert!(file_write_guard::transaction_is_pinned(&transaction));
        // Test cleanup only: production never marks unknown I/O finished.
        guard.write_finished();
    }
    #[test]
    fn cancellation_wakes_a_wait_that_is_already_pending() {
        let completion: SaveFuture = Box::pin(futures::future::pending());
        let cancel = AtomicBool::new(false);
        let (result, ()) = futures::executor::block_on(async {
            futures::join!(
                await_save(
                    completion.shared(),
                    Instant::now() + SAVE_WAIT_TIMEOUT,
                    &cancel
                ),
                async {
                    async_io::Timer::after(Duration::from_millis(5)).await;
                    cancel.store(true, Ordering::Relaxed);
                }
            )
        });
        assert!(result.unwrap_err().contains("stopped waiting"));
    }
}
