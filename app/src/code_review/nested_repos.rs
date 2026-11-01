//! Discovery and lightweight git status for the repositories nested under a folder that is not
//! itself a repository.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify_debouncer_full::notify::{RecursiveMode, WatchFilter};
use warp_util::git::run_git_command;
use warpui::{Entity, ModelContext, ModelHandle};
use watcher::{BulkFilesystemWatcher, BulkFilesystemWatcherEvent};

use super::diff_state::GitFileStatus;

const GIT_ENTRY_NAME: &str = ".git";

/// Deepest level, relative to the scanned folder, at which a `.git` entry is still discovered.
const MAX_GIT_ENTRY_DEPTH: usize = 6;

/// Dependency trees are deep, large, and never hold repositories the user works in.
const SKIPPED_DIRECTORY_NAME: &str = "node_modules";

/// Loose and packed objects churn heavily during fetches without changing what status reports;
/// the ref updates that accompany them are observed separately.
const GIT_OBJECTS_DIRECTORY_NAME: &str = "objects";

const MAX_CONCURRENT_STATUS_LOADS: usize = 6;
const STATUS_LOAD_TIMEOUT: Duration = Duration::from_secs(30);
const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

const WATCHER_DEBOUNCE: Duration = Duration::from_millis(1500);

/// Length of the abbreviated commit shown in place of a branch name for a detached `HEAD`.
const SHORT_OID_LENGTH: usize = 7;

/// Value of `branch.head` in porcelain v2 output when `HEAD` is detached.
const DETACHED_HEAD: &str = "(detached)";

/// Value of `branch.oid` in porcelain v2 output before the first commit.
const UNBORN_OID: &str = "(initial)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NestedRepoKind {
    Repository,
    /// A linked worktree, whose `.git` is a file pointing into another repository.
    Worktree,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NestedRepo {
    pub root: PathBuf,
    /// Directory holding this working tree's `HEAD` and index. Lives outside `root` for linked
    /// worktrees.
    git_dir: PathBuf,
    pub kind: NestedRepoKind,
}

/// Finds every git working tree under `scan_root`, sorted by path.
///
/// Symlinked directories are not followed and the contents of `.git` are never searched.
/// Repositories nested inside other repositories are included.
pub(crate) fn find_nested_repos(scan_root: &Path) -> Vec<NestedRepo> {
    let mut repos = Vec::new();
    let mut pending = vec![(scan_root.to_path_buf(), 0)];
    while let Some((directory, depth)) = pending.pop() {
        if depth >= MAX_GIT_ENTRY_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            if name == GIT_ENTRY_NAME {
                repos.extend(nested_repo_at(
                    &directory,
                    &entry.path(),
                    file_type.is_dir(),
                ));
            } else if file_type.is_dir() && name != SKIPPED_DIRECTORY_NAME {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    repos.sort_by(|a, b| a.root.cmp(&b.root));
    repos
}

fn nested_repo_at(
    working_tree: &Path,
    git_entry: &Path,
    git_entry_is_dir: bool,
) -> Option<NestedRepo> {
    let git_dir = if git_entry_is_dir {
        git_entry.to_path_buf()
    } else {
        let contents = std::fs::read_to_string(git_entry).ok()?;
        let target = PathBuf::from(contents.trim().strip_prefix("gitdir:")?.trim());
        let resolved = if target.is_absolute() {
            target
        } else {
            working_tree.join(target)
        };
        // Watcher events carry resolved paths, so `..` segments would defeat prefix matching.
        std::fs::canonicalize(&resolved).unwrap_or(resolved)
    };
    if !git_dir.join("HEAD").is_file() {
        return None;
    }
    let is_linked_worktree = !git_entry_is_dir
        && git_dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "worktrees");
    Some(NestedRepo {
        root: working_tree.to_path_buf(),
        git_dir,
        kind: if is_linked_worktree {
            NestedRepoKind::Worktree
        } else {
            NestedRepoKind::Repository
        },
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct NestedRepoFile {
    /// Path relative to the repository root.
    pub path: String,
    pub status: GitFileStatus,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NestedRepoStatus {
    /// Branch name, or an abbreviated commit when `HEAD` is detached.
    pub branch: String,
    pub is_detached: bool,
    /// Commit that `HEAD` points at. `None` before the repository's first commit.
    pub head_oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub files: Vec<NestedRepoFile>,
}

async fn load_status(repo_root: &Path) -> anyhow::Result<NestedRepoStatus> {
    let command = run_git_command(repo_root, &["status", "--porcelain=v2", "--branch", "-z"]);
    futures::pin_mut!(command);
    let output =
        match futures::future::select(command, async_io::Timer::after(STATUS_LOAD_TIMEOUT)).await {
            futures::future::Either::Left((result, _)) => result?,
            futures::future::Either::Right(_) => {
                anyhow::bail!("Git status timed out for {}", repo_root.display());
            }
        };
    Ok(parse_porcelain_v2_status(&output))
}

/// Parses the output of `git status --porcelain=v2 --branch -z`.
pub(crate) fn parse_porcelain_v2_status(output: &str) -> NestedRepoStatus {
    let mut status = NestedRepoStatus::default();
    let mut head = None;
    let mut oid = None;
    let mut records = output.split('\0');
    while let Some(record) = records.next() {
        if let Some(value) = record.strip_prefix("# branch.head ") {
            head = Some(value);
        } else if let Some(value) = record.strip_prefix("# branch.oid ") {
            oid = Some(value);
        } else if let Some(value) = record.strip_prefix("# branch.upstream ") {
            status.upstream = Some(value.to_owned());
        } else if let Some(value) = record.strip_prefix("# branch.ab ") {
            let mut counts = value.split(' ');
            status.ahead = parse_tracking_count(counts.next(), '+');
            status.behind = parse_tracking_count(counts.next(), '-');
        } else if let Some(entry) = record.strip_prefix("1 ") {
            // <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
            let mut fields = entry.splitn(8, ' ');
            if let (Some(xy), Some(path)) = (fields.next(), fields.nth(6)) {
                status.files.push(NestedRepoFile {
                    path: path.to_owned(),
                    status: status_for_changed_entry(xy),
                });
            }
        } else if let Some(entry) = record.strip_prefix("2 ") {
            // <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>, then the original path as
            // its own record.
            let old_path = records.next().unwrap_or_default().to_owned();
            let mut fields = entry.splitn(9, ' ');
            if let (Some(_xy), Some(score), Some(path)) =
                (fields.next(), fields.nth(6), fields.next())
            {
                status.files.push(NestedRepoFile {
                    path: path.to_owned(),
                    status: if score.starts_with('C') {
                        GitFileStatus::Copied { old_path }
                    } else {
                        GitFileStatus::Renamed { old_path }
                    },
                });
            }
        } else if let Some(entry) = record.strip_prefix("u ") {
            // <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>
            if let Some(path) = entry.splitn(10, ' ').nth(9) {
                status.files.push(NestedRepoFile {
                    path: path.to_owned(),
                    status: GitFileStatus::Conflicted,
                });
            }
        } else if let Some(path) = record.strip_prefix("? ") {
            status.files.push(NestedRepoFile {
                path: path.to_owned(),
                status: GitFileStatus::Untracked,
            });
        }
    }
    status.is_detached = head == Some(DETACHED_HEAD);
    status.head_oid = oid.filter(|oid| *oid != UNBORN_OID).map(str::to_owned);
    status.branch = match (head, oid) {
        (Some(DETACHED_HEAD), Some(oid)) => oid.chars().take(SHORT_OID_LENGTH).collect(),
        (Some(head), _) => head.to_owned(),
        (None, _) => String::new(),
    };
    status
}

fn parse_tracking_count(field: Option<&str>, sign: char) -> usize {
    field
        .and_then(|field| field.strip_prefix(sign))
        .and_then(|count| count.parse().ok())
        .unwrap_or(0)
}

/// Maps the staged (`X`) and unstaged (`Y`) codes of an ordinary changed entry to one status.
fn status_for_changed_entry(xy: &str) -> GitFileStatus {
    if xy.contains('D') {
        GitFileStatus::Deleted
    } else if xy.contains('A') {
        GitFileStatus::New
    } else {
        GitFileStatus::Modified
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusLoadState {
    Idle,
    Queued,
    Loading {
        /// Set when a change arrives mid-load, since the running load may have missed it.
        reload_when_done: bool,
    },
}

pub struct NestedRepoEntry {
    pub repo: NestedRepo,
    /// Path relative to the scanned folder.
    pub display_name: String,
    /// `None` until the first load finishes, and again if the latest load failed.
    pub status: Option<NestedRepoStatus>,
    pub status_failed: bool,
    load_state: StatusLoadState,
}

impl NestedRepoEntry {
    fn update_status(&mut self, result: anyhow::Result<NestedRepoStatus>) {
        self.status_failed = result.is_err();
        self.status = match result {
            Ok(status) => Some(status),
            Err(err) => {
                log::warn!(
                    "Failed to load git status for {}: {err:#}",
                    self.repo.root.display()
                );
                None
            }
        };
    }
}

pub enum NestedReposEvent {
    Updated,
}

/// The repositories under one folder, with a status per repository that follows the filesystem.
pub struct NestedReposModel {
    scan_root: PathBuf,
    entries: Vec<NestedRepoEntry>,
    has_completed_scan: bool,
    is_scanning: bool,
    rescan_when_done: bool,
    queued_status_loads: VecDeque<PathBuf>,
    active_status_loads: usize,
    /// Held so the watch on `scan_root` lives exactly as long as this model.
    _watcher: ModelHandle<BulkFilesystemWatcher>,
}

impl Entity for NestedReposModel {
    type Event = NestedReposEvent;
}

impl NestedReposModel {
    pub fn new(scan_root: PathBuf, ctx: &mut ModelContext<Self>) -> Self {
        let watcher = ctx.add_model(|ctx| BulkFilesystemWatcher::new(WATCHER_DEBOUNCE, ctx));
        ctx.subscribe_to_model(&watcher, |me, _, event, ctx| {
            me.handle_watcher_event(event, ctx);
        });
        let registration = watcher.update(ctx, |watcher, _| {
            watcher.register_path(&scan_root, watch_filter(), RecursiveMode::Recursive)
        });
        ctx.spawn(registration, |me, result, _| {
            if let Err(err) = result {
                log::warn!(
                    "Failed to watch {} for repository changes: {err:#}",
                    me.scan_root.display()
                );
            }
        });

        let mut model = Self {
            scan_root,
            entries: Vec::new(),
            has_completed_scan: false,
            is_scanning: false,
            rescan_when_done: false,
            queued_status_loads: VecDeque::new(),
            active_status_loads: 0,
            _watcher: watcher,
        };
        model.rescan(ctx);
        model.schedule_status_refresh(ctx);
        model
    }

    fn schedule_status_refresh(&mut self, ctx: &mut ModelContext<Self>) {
        // shortcut: metadata outside the scanned folder refreshes every 30 seconds, watch external git dirs if immediate updates are needed.
        ctx.spawn(
            async_io::Timer::after(STATUS_REFRESH_INTERVAL),
            |me, _, ctx| {
                let roots: Vec<_> = me
                    .entries
                    .iter()
                    .map(|entry| entry.repo.root.clone())
                    .collect();
                for root in roots {
                    me.queue_status_load(&root);
                }
                me.start_queued_status_loads(ctx);
                me.schedule_status_refresh(ctx);
            },
        );
    }

    pub fn scan_root(&self) -> &Path {
        &self.scan_root
    }

    pub fn entries(&self) -> &[NestedRepoEntry] {
        &self.entries
    }

    /// Whether the first scan has finished, i.e. whether an empty list means "no repositories".
    pub fn has_completed_scan(&self) -> bool {
        self.has_completed_scan
    }

    /// Rediscovers the repositories under the folder and reloads every status.
    pub fn refresh(&mut self, ctx: &mut ModelContext<Self>) {
        let roots: Vec<PathBuf> = self
            .entries
            .iter()
            .map(|entry| entry.repo.root.clone())
            .collect();
        for root in roots {
            self.queue_status_load(&root);
        }
        self.start_queued_status_loads(ctx);
        self.rescan(ctx);
    }

    /// Reloads the status of one repository, e.g. after running a git operation in it.
    pub fn reload_status(&mut self, repo_root: &Path, ctx: &mut ModelContext<Self>) {
        self.queue_status_load(repo_root);
        self.start_queued_status_loads(ctx);
    }

    fn rescan(&mut self, ctx: &mut ModelContext<Self>) {
        if self.is_scanning {
            self.rescan_when_done = true;
            return;
        }
        self.is_scanning = true;
        let scan_root = self.scan_root.clone();
        ctx.spawn(
            async move { find_nested_repos(&scan_root) },
            Self::handle_scan_result,
        );
    }

    fn handle_scan_result(&mut self, repos: Vec<NestedRepo>, ctx: &mut ModelContext<Self>) {
        self.is_scanning = false;
        self.has_completed_scan = true;

        let mut previous: Vec<NestedRepoEntry> = std::mem::take(&mut self.entries);
        let mut new_roots = Vec::new();
        for repo in repos {
            let existing = previous
                .iter()
                .position(|entry| entry.repo == repo)
                .map(|index| previous.swap_remove(index));
            let entry = existing.unwrap_or_else(|| {
                new_roots.push(repo.root.clone());
                NestedRepoEntry {
                    display_name: display_name(&self.scan_root, &repo.root),
                    repo,
                    status: None,
                    status_failed: false,
                    load_state: StatusLoadState::Idle,
                }
            });
            self.entries.push(entry);
        }
        self.queued_status_loads
            .retain(|root| self.entries.iter().any(|entry| &entry.repo.root == root));

        for root in new_roots {
            self.queue_status_load(&root);
        }
        self.start_queued_status_loads(ctx);
        ctx.emit(NestedReposEvent::Updated);

        if std::mem::take(&mut self.rescan_when_done) {
            self.rescan(ctx);
        }
    }

    fn queue_status_load(&mut self, root: &Path) {
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.repo.root == root)
        else {
            return;
        };
        match &mut entry.load_state {
            StatusLoadState::Idle => {
                entry.load_state = StatusLoadState::Queued;
                self.queued_status_loads.push_back(root.to_path_buf());
            }
            StatusLoadState::Queued => {}
            StatusLoadState::Loading { reload_when_done } => *reload_when_done = true,
        }
    }

    fn start_queued_status_loads(&mut self, ctx: &mut ModelContext<Self>) {
        while self.active_status_loads < MAX_CONCURRENT_STATUS_LOADS {
            let Some(root) = self.queued_status_loads.pop_front() else {
                break;
            };
            let Some(entry) = self
                .entries
                .iter_mut()
                .find(|entry| entry.repo.root == root)
            else {
                continue;
            };
            entry.load_state = StatusLoadState::Loading {
                reload_when_done: false,
            };
            self.active_status_loads += 1;
            ctx.spawn(
                async move {
                    let result = load_status(&root).await;
                    (root, result)
                },
                |me, (root, result), ctx| me.handle_status_result(&root, result, ctx),
            );
        }
    }

    fn handle_status_result(
        &mut self,
        root: &Path,
        result: anyhow::Result<NestedRepoStatus>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.active_status_loads = self.active_status_loads.saturating_sub(1);
        // The repository may have disappeared in a rescan while its status was loading.
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.repo.root == root)
        {
            let reload = matches!(
                entry.load_state,
                StatusLoadState::Loading {
                    reload_when_done: true
                }
            );
            entry.load_state = StatusLoadState::Idle;
            entry.update_status(result);
            if reload {
                self.queue_status_load(root);
            }
            ctx.emit(NestedReposEvent::Updated);
        }
        self.start_queued_status_loads(ctx);
    }

    fn handle_watcher_event(
        &mut self,
        event: &BulkFilesystemWatcherEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        let structural_paths = event
            .added
            .iter()
            .chain(&event.deleted)
            .chain(event.moved.keys())
            .chain(event.moved.values());
        let mut needs_rescan = false;
        let mut changed_roots = HashSet::new();
        for path in structural_paths {
            if self.changes_repository_set(path) {
                needs_rescan = true;
            } else {
                changed_roots.extend(self.roots_affected_by(path));
            }
        }
        for path in &event.modified {
            changed_roots.extend(self.roots_affected_by(path));
        }

        for root in changed_roots {
            self.queue_status_load(&root);
        }
        self.start_queued_status_loads(ctx);
        if needs_rescan {
            self.rescan(ctx);
        }
    }

    /// Whether creating, removing or moving `path` can add or remove a repository.
    fn changes_repository_set(&self, path: &Path) -> bool {
        may_add_repository(path)
            || self
                .entries
                .iter()
                .any(|entry| entry.repo.root.starts_with(path))
    }

    fn roots_affected_by(&self, path: &Path) -> Vec<PathBuf> {
        // Git metadata is matched on the git dir first: a linked worktree's lives inside another
        // repository's `.git`, so the working tree that contains the path is the wrong answer.
        let metadata_owner = self
            .entries
            .iter()
            .filter(|entry| path.starts_with(&entry.repo.git_dir))
            .max_by_key(|entry| entry.repo.git_dir.as_os_str().len());
        if let Some(owner) = metadata_owner {
            // Refs are shared with linked worktrees, whose ahead/behind counts depend on them.
            return self
                .entries
                .iter()
                .filter(|entry| entry.repo.git_dir.starts_with(&owner.repo.git_dir))
                .map(|entry| entry.repo.root.clone())
                .collect();
        }
        self.entries
            .iter()
            .filter(|entry| path.starts_with(&entry.repo.root))
            .max_by_key(|entry| entry.repo.root.as_os_str().len())
            .map(|entry| entry.repo.root.clone())
            .into_iter()
            .collect()
    }
}

/// Moving an existing directory into the scan root need not emit events for its `.git` child.
fn may_add_repository(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == GIT_ENTRY_NAME)
        || (path.is_dir()
            && is_watched_path(path)
            && !path
                .components()
                .any(|part| part.as_os_str() == GIT_ENTRY_NAME))
}

fn display_name(scan_root: &Path, repo_root: &Path) -> String {
    let relative = repo_root.strip_prefix(scan_root).unwrap_or(repo_root);
    if relative.as_os_str().is_empty() {
        repo_root
            .file_name()
            .unwrap_or(repo_root.as_os_str())
            .to_string_lossy()
            .into_owned()
    } else {
        relative.to_string_lossy().into_owned()
    }
}

fn watch_filter() -> WatchFilter {
    WatchFilter::with_filter(Arc::new(is_watched_path), Arc::new(is_watched_path))
}

fn is_watched_path(path: &Path) -> bool {
    let mut inside_git_dir = false;
    for component in path.components() {
        let name = component.as_os_str();
        if name == SKIPPED_DIRECTORY_NAME || (inside_git_dir && name == GIT_OBJECTS_DIRECTORY_NAME)
        {
            return false;
        }
        inside_git_dir = name == GIT_ENTRY_NAME;
    }
    true
}

#[cfg(test)]
#[path = "nested_repos_tests.rs"]
mod tests;
