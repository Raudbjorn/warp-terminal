//! Finds the files and folders under the global search roots whose names match the query.

use std::borrow::Cow;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use project_replace::scope::Scope;
use repo_metadata::file_tree_store::FileTreeEntryState;
use repo_metadata::repositories::DetectedRepositories;
use repo_metadata::{FileTreeEntry, RepoMetadataModel, RepositoryIdentifier};
use warp_ripgrep::walk::walk_unignored;
use warp_util::local_or_remote_path::LocalOrRemotePath;
use warp_util::standardized_path::StandardizedPath;
use warpui::{AppContext, SingletonEntity};

use super::name_match::{MAX_RANKED_NAME_MATCHES, NameMatchCollector, NameMatcher};
use super::{NameMatch, NameMatchKind, NameSearchResults};

/// Upper bound on the entries listed from disk for one root, so that searching an unindexed
/// directory as large as a home directory stays bounded.
const MAX_DISK_ENTRIES: usize = 200_000;

const GIT_DIRECTORY: &str = ".git";

/// Where the entries below a search root are listed from.
pub(super) enum NameSource {
    /// The repository index, for a directory inside an indexed repository.
    Index {
        tree: FileTreeEntry,
        /// The root's path as the index knows it.
        directory: StandardizedPath,
    },
    /// The local disk, for a directory no index covers.
    Disk,
}

pub(super) struct NameSearchRoot {
    pub(super) root: LocalOrRemotePath,
    pub(super) source: NameSource,
    /// Put in front of the root-relative path of every match, to tell matches from different
    /// roots apart.
    pub(super) display_prefix: String,
}

/// Picks the source to list each of `roots` from. Remote roots without an index are left out,
/// since there is no other way to list them.
pub(super) fn resolve_roots(roots: &[LocalOrRemotePath], app: &AppContext) -> Vec<NameSearchRoot> {
    let has_multiple_roots = roots.len() > 1;
    roots
        .iter()
        .filter_map(|root| {
            let source = indexed_source(root, app)
                .or_else(|| root.is_local().then_some(NameSource::Disk))?;
            let display_prefix = if has_multiple_roots {
                format!("{}/", root.display_name())
            } else {
                String::new()
            };
            Some(NameSearchRoot {
                root: root.clone(),
                source,
                display_prefix,
            })
        })
        .collect()
}

fn indexed_source(root: &LocalOrRemotePath, app: &AppContext) -> Option<NameSource> {
    let repository_root = DetectedRepositories::as_ref(app).get_root_for_path(root)?;
    let (repository, directory) = match (&repository_root, root) {
        (LocalOrRemotePath::Local(repository_root), LocalOrRemotePath::Local(root)) => (
            RepositoryIdentifier::try_local(repository_root)?,
            // The index holds canonical paths, while the root may go through a symlink.
            StandardizedPath::from_local_canonicalized(root).ok()?,
        ),
        (LocalOrRemotePath::Remote(repository_root), LocalOrRemotePath::Remote(root)) => (
            RepositoryIdentifier::Remote(repository_root.clone()),
            root.path.clone(),
        ),
        (LocalOrRemotePath::Local(_), LocalOrRemotePath::Remote(_))
        | (LocalOrRemotePath::Remote(_), LocalOrRemotePath::Local(_)) => return None,
    };

    let tree = &RepoMetadataModel::as_ref(app)
        .get_repository(&repository, app)?
        .entry;
    match tree.get(&directory)? {
        // An ignored or unloaded directory has no children in the index.
        FileTreeEntryState::Directory(state) if state.loaded && !state.ignored => {
            Some(NameSource::Index {
                tree: tree.clone(),
                directory,
            })
        }
        FileTreeEntryState::Directory(_) | FileTreeEntryState::File(_) => None,
    }
}

/// Matches the name of every entry below `roots` that `scope` covers. Returns `None` if
/// `cancelled` was set before the search finished.
pub(super) fn search_names(
    roots: &[NameSearchRoot],
    matcher: &NameMatcher,
    scope: &Scope,
    cancelled: &AtomicBool,
) -> Option<NameSearchResults> {
    let mut collector = NameMatchCollector::new(MAX_RANKED_NAME_MATCHES);

    for root in roots {
        let prefix_chars = root.display_prefix.chars().count();
        let root_path = match &root.root {
            LocalOrRemotePath::Local(path) => path.clone(),
            LocalOrRemotePath::Remote(remote) => PathBuf::from(remote.path.as_str()),
        };
        let mut visit = |relative_path: &str, kind| {
            if cancelled.load(Ordering::Relaxed) {
                return ControlFlow::Break(());
            }
            let Some(matched) = matcher.match_path(relative_path) else {
                return ControlFlow::Continue(());
            };
            if !scope.is_unrestricted()
                && !scope.includes(&root_path, &root_path.join(relative_path))
            {
                return ControlFlow::Continue(());
            }
            let display_path = if root.display_prefix.is_empty() {
                Cow::Borrowed(relative_path)
            } else {
                Cow::Owned(format!("{}{relative_path}", root.display_prefix))
            };
            let score = matched.score;
            collector.offer(kind, score, &display_path, || NameMatch {
                location: root.root.join(relative_path),
                display_path: display_path.to_string(),
                highlight_indices: matched
                    .matched_indices
                    .into_iter()
                    .map(|index| index + prefix_chars)
                    .collect(),
                score,
            });
            ControlFlow::Continue(())
        };

        match &root.source {
            NameSource::Index { tree, directory } => visit_indexed(tree, directory, &mut visit),
            NameSource::Disk => {
                if let Some(directory) = root.root.to_local_path() {
                    visit_disk(directory, &mut visit);
                }
            }
        }

        if cancelled.load(Ordering::Relaxed) {
            return None;
        }
    }

    Some(collector.finish())
}

/// Visits every entry below `directory` that is not ignored, passing its path relative to
/// `directory`.
fn visit_indexed(
    tree: &FileTreeEntry,
    directory: &StandardizedPath,
    visit: &mut impl FnMut(&str, NameMatchKind) -> ControlFlow<()>,
) {
    let mut pending: Vec<&Arc<StandardizedPath>> = tree.child_paths(directory).collect();
    while let Some(path) = pending.pop() {
        let Some(state) = tree.get(path) else {
            continue;
        };
        if state.ignored() || path.file_name() == Some(GIT_DIRECTORY) {
            continue;
        }
        let Some(relative_path) = path.strip_prefix(directory) else {
            continue;
        };
        let kind = match state {
            FileTreeEntryState::File(_) => NameMatchKind::File,
            FileTreeEntryState::Directory(_) => {
                pending.extend(tree.child_paths(path));
                NameMatchKind::Folder
            }
        };
        if visit(relative_path, kind).is_break() {
            return;
        }
    }
}

fn visit_disk(directory: &Path, visit: &mut impl FnMut(&str, NameMatchKind) -> ControlFlow<()>) {
    let mut remaining_entries = MAX_DISK_ENTRIES;
    walk_unignored(directory, |path, is_directory| {
        if remaining_entries == 0 {
            return ControlFlow::Break(());
        }
        remaining_entries -= 1;

        let Some(relative_path) = path.strip_prefix(directory).ok().and_then(Path::to_str) else {
            return ControlFlow::Continue(());
        };
        let kind = if is_directory {
            NameMatchKind::Folder
        } else {
            NameMatchKind::File
        };
        visit(relative_path, kind)
    });
}

#[cfg(test)]
#[path = "name_search_tests.rs"]
mod tests;
