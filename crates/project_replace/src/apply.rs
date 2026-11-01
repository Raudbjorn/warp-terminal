//! Replacing across the files of a project.
//!
//! Files that are open in an editor are not written here: the edits for them are handed back so
//! that the editor applies them to its own text, which may hold unsaved changes.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;

use crate::disk::{read_rewritable, write_atomically};
use crate::encoding::{DecodeError, decode, encode};
use crate::engine::{FindError, MatchFinder, SearchSpec};
use crate::journal::{
    EntryKind, Journal, RecordError, TransactionDescription, TransactionWriter, sha256_hex,
};
use crate::plan::{FileExpectation, TextEdit, apply_edits, plan_edits};
use crate::replacement::ReplaceSpec;
use crate::report::{ReplaceCounts, ReplacedFile, SkipReason, SkippedFile};
use crate::scope::{Scope, ScopeError, ScopeGlobs};

/// The byte order mark an editor keeps at the start of its text.
const BYTE_ORDER_MARK: char = '\u{feff}';

/// Files larger than this are left alone unless the request says otherwise.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// A file to replace in, with the matches the search showed for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileTarget {
    pub path: PathBuf,
    pub expectation: FileExpectation,
}

/// A file that is open in an editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenBuffer {
    pub path: PathBuf,
    /// The editor's text, with `\n` line endings.
    pub text: String,
    pub has_unsaved_changes: bool,
}

#[derive(Clone, Debug)]
pub struct ReplaceRequest {
    pub search: SearchSpec,
    pub replace: ReplaceSpec,
    /// The folders that were searched. Files outside them are never touched.
    pub roots: Vec<PathBuf>,
    pub scope: ScopeGlobs,
    pub files: Vec<FileTarget>,
    pub open_buffers: Vec<OpenBuffer>,
    pub max_file_bytes: u64,
}

/// Edits for a file that is open in an editor, for the editor to apply to its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferEdits {
    /// The path the file was found under by the search.
    pub path: PathBuf,
    /// The path of the [`OpenBuffer`] the edits are for.
    pub buffer_path: PathBuf,
    /// Byte ranges of the editor's text, in order and apart.
    pub edits: Vec<TextEdit>,
    journal_index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
}

#[derive(Debug, Error)]
pub enum ReplaceError {
    #[error(transparent)]
    Pattern(#[from] FindError),
    #[error(transparent)]
    Scope(#[from] ScopeError),
    #[error("the undo journal could not be written: {0}")]
    Journal(#[from] std::io::Error),
}

/// What a replace did to the files on disk and what is left for the editors to do.
#[derive(Debug)]
pub struct ReplaceOutcome {
    /// Files rewritten on disk.
    pub replaced: Vec<ReplacedFile>,
    pub skipped: Vec<SkippedFile>,
    /// Edits still to be applied by the editors holding those files. Report how each went with
    /// [`ReplaceOutcome::finish`].
    pub buffer_edits: Vec<BufferEdits>,
    /// Number of files the replace did not get to because it was cancelled.
    pub not_attempted: usize,
    writer: TransactionWriter,
}

/// A file an editor applied its edits to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedBufferEdits {
    pub path: PathBuf,
    /// The editor's text before the edits.
    pub text_before: String,
    /// The editor's text after the edits.
    pub text_after: String,
    pub occurrences: usize,
}

/// The whole of a finished replace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplaceReport {
    pub replaced: Vec<ReplacedFile>,
    pub skipped: Vec<SkippedFile>,
    pub not_attempted: usize,
    /// Whether the replace can be undone, which it can as soon as it changed anything.
    pub can_undo: bool,
}

impl ReplaceReport {
    pub fn counts(&self) -> ReplaceCounts {
        ReplaceCounts {
            occurrences: self.replaced.iter().map(|file| file.occurrences).sum(),
            files: self.replaced.len(),
        }
    }

    pub fn was_cancelled(&self) -> bool {
        self.not_attempted > 0
    }
}

impl ReplaceOutcome {
    /// Records what the editors did with [`ReplaceOutcome::buffer_edits`] and closes the undo
    /// journal of the replace. `skipped` are the files whose editor could not apply them.
    pub fn finish(
        mut self,
        applied: Vec<AppliedBufferEdits>,
        skipped: Vec<SkippedFile>,
    ) -> ReplaceReport {
        self.skipped.extend(skipped);
        for planned in &self.buffer_edits {
            if let Some(applied) = applied.iter().find(|applied| applied.path == planned.path) {
                self.replaced.push(ReplacedFile {
                    path: applied.path.clone(),
                    occurrences: applied.occurrences,
                });
            } else if let Err(err) = self.writer.retract(planned.journal_index) {
                log::warn!("could not retract an unapplied buffer replacement: {err}");
            }
        }

        let can_undo = match self.writer.finish() {
            Ok(id) => id.is_some(),
            Err(err) => {
                log::warn!("could not close the replace journal: {err}");
                false
            }
        };
        ReplaceReport {
            replaced: self.replaced,
            skipped: self.skipped,
            not_attempted: self.not_attempted,
            can_undo,
        }
    }
}

/// Replaces in every file of `request` that still has the matches the search showed.
///
/// Each file is copied to `journal` before it is changed, so that stopping at any point, through
/// `cancel` or otherwise, leaves every file either untouched or changed and undoable.
pub fn replace_in_files(
    request: &ReplaceRequest,
    journal: &Journal,
    progress: &dyn Fn(Progress),
    cancel: &AtomicBool,
) -> Result<ReplaceOutcome, ReplaceError> {
    let finder = MatchFinder::new(&request.search, Some(&request.replace))?;
    let scope = Scope::parse(&request.scope)?;
    let open_buffers = OpenBuffers::new(&request.open_buffers);
    let mut writer = journal.begin(TransactionDescription {
        pattern: request.search.pattern.clone(),
        replacement: request.replace.replacement.clone(),
    })?;

    let mut outcome_replaced = Vec::new();
    let mut skipped = Vec::new();
    let mut buffer_edits = Vec::new();
    let mut not_attempted = 0;
    let total = request.files.len();

    for (done, target) in request.files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            not_attempted = total - done;
            break;
        }

        let result = if !in_scope(&request.roots, &scope, &target.path) {
            Err(SkipReason::OutOfScope)
        } else if let Some(buffer) = open_buffers.for_path(&target.path) {
            plan_buffer_edits(&finder, target, buffer, &mut writer).map(|edits| {
                if let Some(edits) = edits {
                    buffer_edits.push(edits);
                }
            })
        } else {
            replace_on_disk(&finder, target, request.max_file_bytes, &mut writer).map(
                |occurrences| {
                    if occurrences > 0 {
                        outcome_replaced.push(ReplacedFile {
                            path: target.path.clone(),
                            occurrences,
                        });
                    }
                },
            )
        };
        if let Err(reason) = result {
            skipped.push(SkippedFile {
                path: target.path.clone(),
                reason,
            });
        }
        progress(Progress {
            done: done + 1,
            total,
        });
    }

    Ok(ReplaceOutcome {
        replaced: outcome_replaced,
        skipped,
        buffer_edits,
        not_attempted,
        writer,
    })
}

fn in_scope(roots: &[PathBuf], scope: &Scope, path: &Path) -> bool {
    roots
        .iter()
        .any(|root| path.starts_with(root) && scope.includes(root, path))
}

/// Looks open editors up by the file they hold, whichever way the path to it is spelled.
struct OpenBuffers<'a> {
    by_canonical_path: HashMap<PathBuf, &'a OpenBuffer>,
}

impl<'a> OpenBuffers<'a> {
    fn new(buffers: &'a [OpenBuffer]) -> Self {
        Self {
            by_canonical_path: buffers
                .iter()
                .map(|buffer| (canonical(&buffer.path), buffer))
                .collect(),
        }
    }

    fn for_path(&self, path: &Path) -> Option<&'a OpenBuffer> {
        if self.by_canonical_path.is_empty() {
            return None;
        }
        self.by_canonical_path.get(&canonical(path)).copied()
    }
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Replaces in an unopened file. Returns the number of occurrences replaced.
fn replace_on_disk(
    finder: &MatchFinder,
    target: &FileTarget,
    max_file_bytes: u64,
    writer: &mut TransactionWriter,
) -> Result<usize, SkipReason> {
    let (bytes, stamp) = read_rewritable(&target.path, max_file_bytes)?;
    let decoded = decode(&bytes).map_err(|err| match err {
        DecodeError::Binary => SkipReason::Binary,
        DecodeError::UnsupportedEncoding => SkipReason::UnsupportedEncoding,
    })?;
    let found = finder
        .find(&decoded.text)
        .map_err(|err| SkipReason::SearchFailed(err.to_string()))?;
    let edits = plan_edits(&found, &target.expectation, false)
        .map_err(|_| SkipReason::ChangedSinceSearch)?;
    if edits.is_empty() {
        return Ok(0);
    }

    let new_text = apply_edits(&decoded.text, &edits);
    // A replacement equal to what it replaces leaves nothing to write, or to undo.
    if new_text == decoded.text {
        return Ok(0);
    }
    let new_bytes = encode(&new_text, decoded.encoding);
    if !stamp.still_matches(&target.path) {
        return Err(SkipReason::ChangedDuringReplace);
    }

    let index = writer
        .record(
            &target.path,
            EntryKind::Disk,
            &bytes,
            sha256_hex(&new_bytes),
            edits.len(),
        )
        .map_err(|err| match err {
            RecordError::Full => SkipReason::JournalFull,
            RecordError::Io(err) => SkipReason::WriteFailed(err.to_string()),
        })?;
    // Writing the journal can take time; check again immediately before replacing the file.
    if !stamp.still_matches(&target.path) {
        if let Err(err) = writer.retract(index) {
            log::warn!("could not retract a stale replacement: {err}");
        }
        return Err(SkipReason::ChangedDuringReplace);
    }
    if let Err(err) = write_atomically(&target.path, &new_bytes, stamp.metadata()) {
        if let Err(retract_err) = writer.retract(index) {
            log::warn!("could not take back a replace journal entry: {retract_err}");
        }
        return Err(SkipReason::WriteFailed(err.to_string()));
    }
    Ok(edits.len())
}

/// Plans the edits for a file that is open in an editor, setting room aside in the journal for
/// the copy of the editor's text. Returns `None` when there is nothing to replace.
fn plan_buffer_edits(
    finder: &MatchFinder,
    target: &FileTarget,
    buffer: &OpenBuffer,
    writer: &mut TransactionWriter,
) -> Result<Option<BufferEdits>, SkipReason> {
    // The search does not count the byte order mark as part of the first line.
    let searched = buffer
        .text
        .strip_prefix(BYTE_ORDER_MARK)
        .unwrap_or(&buffer.text);
    let offset = buffer.text.len() - searched.len();
    if searched.contains('\0') {
        return Err(SkipReason::Binary);
    }

    let found = finder
        .find(searched)
        .map_err(|err| SkipReason::SearchFailed(err.to_string()))?;
    let mut edits = plan_edits(&found, &target.expectation, true)
        .map_err(|_| SkipReason::ChangedSinceSearch)?;
    if edits.is_empty() || apply_edits(searched, &edits) == searched {
        return Ok(None);
    }
    for edit in &mut edits {
        edit.range = edit.range.start + offset..edit.range.end + offset;
    }

    let journal_index = writer
        .record(
            &target.path,
            EntryKind::Buffer,
            buffer.text.as_bytes(),
            sha256_hex(apply_edits(&buffer.text, &edits).as_bytes()),
            edits.len(),
        )
        .map_err(|err| match err {
            RecordError::Full => SkipReason::JournalFull,
            RecordError::Io(err) => SkipReason::WriteFailed(err.to_string()),
        })?;
    Ok(Some(BufferEdits {
        path: target.path.clone(),
        buffer_path: buffer.path.clone(),
        edits,
        journal_index,
    }))
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
