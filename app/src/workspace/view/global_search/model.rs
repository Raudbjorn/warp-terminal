use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use futures::StreamExt as _;
use instant::Instant;
use num_traits::SaturatingSub;
use project_replace::engine::SearchSpec;
use project_replace::scope::{Scope, ScopeGlobs};
use remote_server::HostId;
use remote_server::manager::{HostRequestError, RemoteServerManager, RipgrepSearchParams};
use remote_server::proto::RipgrepSearchSuccess;
use remote_server::protocol::RequestId;
use string_offset::ByteOffset;
use warp_errors::report_error;
use warp_ripgrep::search::{Match as RipgrepMatch, Submatch};
use warp_util::local_or_remote_path::LocalOrRemotePath;
use warp_util::remote_path::RemotePath;
use warp_util::standardized_path::StandardizedPath;
use warpui::r#async::SpawnedFutureHandle;
use warpui::{Entity, ModelContext, ModelSpawner, SingletonEntity};

use crate::workspace::view::global_search::name_match::NameMatcher;
use crate::workspace::view::global_search::name_search::{resolve_roots, search_names};
use crate::workspace::view::global_search::view::GlobalSearchEvent;
use crate::workspace::view::global_search::{GlobalSearchMatch, SearchConfig, SearchOptions};

const START_BATCH_AFTER_COUNT: usize = 50;
const MAX_BATCH_SIZE: usize = 512;
const MAX_BATCH_AGE_MS: u64 = 4000;

/// Client-requested cap on remote matches per host. The daemon clamps this
/// to its own server-side cap; both bound the single-frame response size.
const REMOTE_MAX_MATCH_COUNT: u32 = 5_000;

/// Aggregate state for one logical search across all of its sources
/// (one local ripgrep run plus one remote request per searched host).
struct ActiveSearch {
    search_id: u32,
    remaining_sources: usize,
    completed_sources: usize,
    local_source_failed: bool,
    remote_source_failures: usize,
    total_match_count: usize,
    /// True when any remote source hit the server-side match cap.
    capped: bool,
}

#[derive(Clone, Copy)]
enum SearchSource {
    Local,
    Remote,
}

/// Result of one search source (the local ripgrep run, or one remote
/// host's request) that ran to completion.
struct SourceResult {
    match_count: usize,
    capped: bool,
}

/// The background task matching file and folder names for one search.
struct NameSearchTask {
    search_id: u32,
    handle: SpawnedFutureHandle,
    /// Stops the task between entries, since it never yields to the executor while it runs.
    cancelled: Arc<AtomicBool>,
}

pub struct GlobalSearch {
    /// Spawned local/remote search tasks for the current search.
    search_handles: Vec<SpawnedFutureHandle>,
    /// Name matching for the current search. It runs apart from the content sources, so neither
    /// kind of result waits for the other.
    name_search: Option<NameSearchTask>,
    /// Request ids of remote searches started for the current search, so
    /// they can be aborted daemon-side when the query changes. May contain
    /// ids of already-resolved requests; aborting those is a no-op.
    in_flight_remote_requests: Vec<RequestId>,
    /// Aggregate completion state for the current search.
    active_search: Option<ActiveSearch>,
    // track the search ID so that we only show results for the current search
    next_search_id: u32,
}

impl Entity for GlobalSearch {
    type Event = GlobalSearchEvent;
}

async fn flush_batch(
    spawner: &ModelSpawner<GlobalSearch>,
    search_id: u32,
    batch: &mut Vec<GlobalSearchMatch>,
) {
    if batch.is_empty() {
        return;
    }

    let items = std::mem::take(batch);

    let _ = spawner
        .spawn(move |_me, ctx| {
            ctx.emit(GlobalSearchEvent::ProgressBatch { search_id, items });
        })
        .await;
}

impl GlobalSearch {
    pub fn new() -> Self {
        GlobalSearch {
            search_handles: Vec::new(),
            name_search: None,
            in_flight_remote_requests: Vec::new(),
            active_search: None,
            next_search_id: 1,
        }
    }

    pub fn abort_search(&mut self, ctx: &mut ModelContext<Self>) {
        self.abort_content_search(ctx);
        self.abort_name_search();
    }

    fn abort_name_search(&mut self) {
        if let Some(task) = self.name_search.take() {
            task.cancelled.store(true, Ordering::Relaxed);
            task.handle.abort();
        }
    }

    /// Stops searching file contents, leaving name matching for the current search running.
    pub fn abort_content_search(&mut self, ctx: &mut ModelContext<Self>) {
        for handle in self.search_handles.drain(..) {
            handle.abort();
        }
        self.active_search = None;

        // Cancel in-flight remote searches daemon-side as well: queries
        // change on every debounced edit, so without this the daemon piles
        // up wasted ripgrep runs.
        let request_ids = std::mem::take(&mut self.in_flight_remote_requests);
        if !request_ids.is_empty() {
            RemoteServerManager::handle(ctx).update(ctx, |manager, _| {
                for request_id in &request_ids {
                    manager.abort_host_request(request_id);
                }
            });
        }
    }

    pub fn run_search(
        &mut self,
        pattern: String,
        roots: Vec<LocalOrRemotePath>,
        search_config: SearchConfig,
        options: SearchOptions,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.search_handles.is_empty() {
            log::info!("GlobalSearch: aborting previous search");
        }
        self.abort_search(ctx);

        let search_id = self.next_search_id;
        self.next_search_id += 1;

        let scope = match Scope::parse(&options.scope) {
            Ok(scope) => Arc::new(scope),
            Err(err) => {
                ctx.emit(GlobalSearchEvent::Started {
                    search_id,
                    remote_host_count: 0,
                    name_search_started: false,
                });
                ctx.emit(GlobalSearchEvent::Failed {
                    search_id,
                    error: err.to_string(),
                });
                return;
            }
        };

        let name_search_started = self.spawn_name_search(
            search_id,
            &pattern,
            &roots,
            &search_config,
            scope.clone(),
            ctx,
        );

        let spec = SearchSpec {
            pattern,
            use_regex: search_config.use_regex,
            case_sensitive: search_config.use_case_sensitivity,
            whole_word: options.whole_word,
        };
        let effective_pattern = spec.effective_pattern();
        let ignore_case = spec.ignore_case();
        let multiline = spec.is_multiline();

        // Split roots into the local filesystem source and one remote
        // source per host.
        let mut local_roots: Vec<PathBuf> = Vec::new();
        let mut remote_roots: HashMap<HostId, Vec<StandardizedPath>> = HashMap::new();
        for root in roots {
            match root {
                LocalOrRemotePath::Local(path) => local_roots.push(path),
                LocalOrRemotePath::Remote(remote) => {
                    remote_roots
                        .entry(remote.host_id)
                        .or_default()
                        .push(remote.path);
                }
            }
        }

        let remote_host_count = remote_roots.len();
        ctx.emit(GlobalSearchEvent::Started {
            search_id,
            remote_host_count,
            name_search_started,
        });
        let source_count = usize::from(!local_roots.is_empty()) + remote_roots.len();
        if source_count == 0 {
            ctx.emit(GlobalSearchEvent::Completed {
                search_id,
                total_match_count: 0,
                capped: false,
                local_source_failed: false,
                remote_source_failures: 0,
            });
            return;
        }

        self.active_search = Some(ActiveSearch {
            search_id,
            remaining_sources: source_count,
            completed_sources: 0,
            local_source_failed: false,
            remote_source_failures: 0,
            total_match_count: 0,
            capped: false,
        });

        if !local_roots.is_empty() {
            self.spawn_local_search(
                search_id,
                effective_pattern.clone(),
                local_roots,
                ignore_case,
                multiline,
                options.scope.clone(),
                ctx,
            );
        }

        for (host_id, paths) in remote_roots {
            let params = RipgrepSearchParams {
                pattern: effective_pattern.clone(),
                roots: paths,
                ignore_case,
                multiline,
                max_matches: REMOTE_MAX_MATCH_COUNT,
            };
            self.spawn_remote_search(search_id, host_id, params, scope.clone(), ctx);
        }
    }

    /// Starts matching file and folder names below `roots`. Returns whether a task was started,
    /// in which case a [`GlobalSearchEvent::NameMatches`] follows unless the search is aborted.
    fn spawn_name_search(
        &mut self,
        search_id: u32,
        pattern: &str,
        roots: &[LocalOrRemotePath],
        search_config: &SearchConfig,
        scope: Arc<Scope>,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(matcher) = NameMatcher::new(pattern, search_config) else {
            return false;
        };
        let roots = resolve_roots(roots, ctx);
        if roots.is_empty() {
            return false;
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        let handle = ctx.spawn(
            {
                let cancelled = cancelled.clone();
                async move { search_names(&roots, &matcher, &scope, &cancelled) }
            },
            move |me, results, ctx| {
                if me
                    .name_search
                    .as_ref()
                    .is_none_or(|task| task.search_id != search_id)
                {
                    return;
                }
                me.name_search = None;
                if let Some(results) = results {
                    ctx.emit(GlobalSearchEvent::NameMatches { search_id, results });
                }
            },
        );
        self.name_search = Some(NameSearchTask {
            search_id,
            handle,
            cancelled,
        });
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_local_search(
        &mut self,
        search_id: u32,
        pattern: String,
        roots: Vec<PathBuf>,
        ignore_case: bool,
        multiline: bool,
        scope: ScopeGlobs,
        ctx: &mut ModelContext<Self>,
    ) {
        let spawner = ctx.spawner();
        self.spawn_source(
            search_id,
            SearchSource::Local,
            async move {
                let result = Self::run_warp_ripgrep_cli(
                    search_id,
                    pattern,
                    roots,
                    ignore_case,
                    multiline,
                    scope,
                    spawner,
                )
                .await;
                match result {
                    Ok(match_count) => Some(SourceResult {
                        match_count,
                        capped: false,
                    }),
                    Err(err) => {
                        report_error!(
                            err.context("GlobalSearch: warp_ripgrep CLI search failed or aborted")
                        );
                        None
                    }
                }
            },
            ctx,
        );
    }

    fn spawn_remote_search(
        &mut self,
        search_id: u32,
        host_id: HostId,
        params: RipgrepSearchParams,
        scope: Arc<Scope>,
        ctx: &mut ModelContext<Self>,
    ) {
        let roots = params.roots.clone();
        let pending = RemoteServerManager::handle(ctx).update(ctx, |manager, _| {
            manager.start_ripgrep_search(&host_id, params)
        });
        self.in_flight_remote_requests
            .push(pending.request_id().clone());
        let spawner = ctx.spawner();
        self.spawn_source(
            search_id,
            SearchSource::Remote,
            async move {
                match pending.result().await {
                    Ok(mut success) => {
                        let capped = success.capped;
                        // The host searches whole folders, so the scope is applied to what it
                        // found.
                        if !scope.is_unrestricted() {
                            success.matches.retain(|found| {
                                let path = Path::new(&found.file_path);
                                roots.iter().any(|root| {
                                    let root = Path::new(root.as_str());
                                    path.starts_with(root) && scope.includes(root, path)
                                })
                            });
                        }
                        let mut items = Self::remote_matches_to_global(&host_id, success);
                        let match_count = items.len();
                        flush_batch(&spawner, search_id, &mut items).await;
                        Some(SourceResult {
                            match_count,
                            capped,
                        })
                    }
                    // An abort is initiated by a newer search (or a reset),
                    // which already replaced the aggregate state; the stale
                    // search-id guard drops this outcome regardless.
                    Err(HostRequestError::Aborted) => None,
                    Err(err) => {
                        log::warn!("GlobalSearch: remote search failed for host {host_id}: {err}");
                        None
                    }
                }
            },
            ctx,
        );
    }

    /// Spawns one search source (the local ripgrep run, or one remote
    /// host's request) and routes its outcome into the shared completion
    /// accounting. Sources emit their matches via `Progress`/`ProgressBatch`
    /// while running and log their own failures.
    fn spawn_source(
        &mut self,
        search_id: u32,
        source_kind: SearchSource,
        source: impl Future<Output = Option<SourceResult>> + Send + 'static,
        ctx: &mut ModelContext<Self>,
    ) {
        let task = ctx.spawn(source, move |me, outcome, ctx| {
            me.handle_source_completed(search_id, source_kind, outcome, ctx);
        });
        self.search_handles.push(task);
    }

    /// Converts a remote search response into per-submatch result rows,
    /// attaching the originating host to each match location.
    fn remote_matches_to_global(
        host_id: &HostId,
        success: RipgrepSearchSuccess,
    ) -> Vec<GlobalSearchMatch> {
        success
            .matches
            .into_iter()
            .filter_map(|m| {
                let path = match StandardizedPath::try_new(&m.file_path) {
                    Ok(path) => path,
                    Err(err) => {
                        log::warn!("GlobalSearch: dropping remote match with invalid path: {err}");
                        return None;
                    }
                };
                let submatches = m
                    .submatches
                    .into_iter()
                    .map(|s| Submatch {
                        byte_start: ByteOffset::from(s.byte_start as usize),
                        byte_end: ByteOffset::from(s.byte_end as usize),
                    })
                    .collect();
                Some(GlobalSearchMatch {
                    location: LocalOrRemotePath::Remote(RemotePath::new(host_id.clone(), path)),
                    line_number: m.line_number,
                    column_num: None,
                    line_text: m.line_text,
                    submatches,
                })
            })
            .flat_map(Self::expand_submatches)
            .collect()
    }

    /// Records the completion of one search source (`None` when the source
    /// failed; the source already logged the failure). When all sources have
    /// finished, emits `Completed` (or `Failed` when every source failed).
    fn handle_source_completed(
        &mut self,
        search_id: u32,
        source_kind: SearchSource,
        outcome: Option<SourceResult>,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(active) = self.active_search.as_mut() else {
            return;
        };
        if active.search_id != search_id {
            return;
        }

        match outcome {
            Some(SourceResult {
                match_count,
                capped,
            }) => {
                active.completed_sources += 1;
                active.total_match_count += match_count;
                active.capped |= capped;
            }
            None => match source_kind {
                SearchSource::Local => active.local_source_failed = true,
                SearchSource::Remote => active.remote_source_failures += 1,
            },
        }

        active.remaining_sources = active.remaining_sources.saturating_sub(1);
        if active.remaining_sources > 0 {
            return;
        }

        let active = self
            .active_search
            .take()
            .expect("active search was checked above");
        if active.completed_sources == 0 {
            ctx.emit(GlobalSearchEvent::Failed {
                search_id,
                error: "Global search failed.".to_string(),
            });
        } else {
            ctx.emit(GlobalSearchEvent::Completed {
                search_id,
                total_match_count: active.total_match_count,
                capped: active.capped,
                local_source_failed: active.local_source_failed,
                remote_source_failures: active.remote_source_failures,
            });
        }
    }

    async fn run_warp_ripgrep_cli(
        search_id: u32,
        pattern: String,
        roots: Vec<PathBuf>,
        ignore_case: bool,
        multiline: bool,
        scope: ScopeGlobs,
        spawner: ModelSpawner<GlobalSearch>,
    ) -> Result<usize> {
        let roots_display: Vec<_> = roots.iter().map(|r| r.display().to_string()).collect();
        log::info!(
            "GlobalSearch: starting warp_ripgrep CLI search with pattern={pattern}, roots={:?}",
            roots_display
        );

        let patterns = &[pattern];
        let stream = warp_ripgrep::search::search_streaming(
            patterns,
            &roots,
            ignore_case,
            multiline,
            &scope,
        )?;
        futures::pin_mut!(stream);

        let mut total_match_count: usize = 0;
        let mut num_unbatched_emitted: usize = 0;
        let mut batch: Vec<GlobalSearchMatch> = Vec::new();
        let mut last_batch_flush_at = Instant::now();

        while let Some(raw_match) = stream.next().await {
            // Expand each submatch into its own result row (matching
            // the old per-submatch behavior). Each row gets the line
            // text trimmed up to that particular submatch.
            for per_submatch in Self::expand_submatches(Self::local_match_to_global(raw_match)) {
                total_match_count += 1;

                if num_unbatched_emitted < START_BATCH_AFTER_COUNT {
                    num_unbatched_emitted += 1;

                    let _ = spawner
                        .spawn(move |_me, ctx| {
                            ctx.emit(GlobalSearchEvent::Progress {
                                search_id,
                                result: per_submatch,
                            });
                        })
                        .await;
                } else {
                    batch.push(per_submatch);

                    let too_big = batch.len() >= MAX_BATCH_SIZE;
                    let too_old =
                        last_batch_flush_at.elapsed().as_millis() >= MAX_BATCH_AGE_MS as u128;

                    if too_big || too_old {
                        flush_batch(&spawner, search_id, &mut batch).await;
                        last_batch_flush_at = Instant::now();
                    }
                }
            }
        }

        if !batch.is_empty() {
            flush_batch(&spawner, search_id, &mut batch).await;
        }

        Ok(total_match_count)
    }

    fn local_match_to_global(m: RipgrepMatch) -> GlobalSearchMatch {
        GlobalSearchMatch {
            location: LocalOrRemotePath::Local(m.file_path),
            line_number: m.line_number,
            column_num: None,
            line_text: m.line_text,
            submatches: m.submatches,
        }
    }

    /// Expand a single match (which may contain multiple submatches
    /// on the same line) into one result per submatch. Each result gets the
    /// line text trimmed of leading whitespace up to that submatch.
    fn expand_submatches(m: GlobalSearchMatch) -> Vec<GlobalSearchMatch> {
        if m.submatches.len() <= 1 {
            let submatch = m.submatches.into_iter().next();
            let column_num = Self::column_from_submatch(&m.line_text, submatch.as_ref());
            return vec![Self::trim_leading_whitespace_for_submatch(
                &m.line_text,
                m.location,
                m.line_number,
                column_num,
                submatch,
            )];
        }

        m.submatches
            .into_iter()
            .map(|sub| {
                let column_num = Self::column_from_submatch(&m.line_text, Some(&sub));
                Self::trim_leading_whitespace_for_submatch(
                    &m.line_text,
                    m.location.clone(),
                    m.line_number,
                    column_num,
                    Some(sub),
                )
            })
            .collect()
    }

    /// Returns the original 1-based character column for a submatch.
    fn column_from_submatch(line_text: &str, submatch: Option<&Submatch>) -> Option<usize> {
        let byte_start = submatch?.byte_start.as_usize();
        if byte_start > line_text.len() || !line_text.is_char_boundary(byte_start) {
            return None;
        }
        Some(line_text[..byte_start].chars().count() + 1)
    }

    /// Trim leading whitespace from a line up to the given submatch,
    /// adjusting the submatch offset accordingly.
    fn trim_leading_whitespace_for_submatch(
        original_line: &str,
        location: LocalOrRemotePath,
        line_number: u32,
        column_num: Option<usize>,
        submatch: Option<Submatch>,
    ) -> GlobalSearchMatch {
        let submatch_start = submatch
            .as_ref()
            .map(|s| s.byte_start)
            .unwrap_or(ByteOffset::zero());

        let mut leading_trimmed_bytes = ByteOffset::zero();
        for (byte_index, ch) in original_line.char_indices() {
            if byte_index >= submatch_start.as_usize() {
                break;
            }
            if !ch.is_ascii_whitespace() {
                break;
            }
            leading_trimmed_bytes += ch.len_utf8();
        }

        let trimmed_line = original_line[leading_trimmed_bytes.as_usize()..].to_string();

        let submatches = if let Some(sub) = submatch {
            vec![Submatch {
                byte_start: sub.byte_start.saturating_sub(&leading_trimmed_bytes),
                byte_end: sub.byte_end.saturating_sub(&leading_trimmed_bytes),
            }]
        } else {
            Vec::new()
        };

        GlobalSearchMatch {
            location,
            line_number,
            column_num,
            line_text: trimmed_line,
            submatches,
        }
    }
}

impl Default for GlobalSearch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
