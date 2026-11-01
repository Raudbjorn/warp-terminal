//! Directory listing that honours the ignore files content search honours.

use std::ops::ControlFlow;
use std::path::Path;

use ignore::WalkBuilder;

/// Visits every file and directory below `root` that ignore files do not exclude, passing each
/// one's path and whether it is a directory to `visit` until it returns [`ControlFlow::Break`].
///
/// Unlike content search, hidden entries are visited, apart from `.git` directories.
pub fn walk_unignored(root: &Path, mut visit: impl FnMut(&Path, bool) -> ControlFlow<()>) {
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                log::debug!(
                    "skipping unreadable entry while listing {}: {err}",
                    root.display()
                );
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        let is_directory = entry
            .file_type()
            .is_some_and(|file_type| file_type.is_dir());
        if visit(entry.path(), is_directory).is_break() {
            return;
        }
    }
}
