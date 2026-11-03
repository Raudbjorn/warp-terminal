//! The search itself: walking the folders to search and reporting the matches of every file.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use grep::regex::RegexMatcher;
use ignore::{WalkBuilder, WalkState};

use crate::engine::{build_searcher, search_path_json};
use crate::scope::Scope;

/// Searches every file below `roots` that ignore files do not exclude and that `scope` covers,
/// writing what is found to `output` as JSON lines. The matches of one file are written in one
/// piece, so that files searched in parallel never interleave.
///
/// Stops early when `output` can no longer be written to.
pub fn search_roots<W: Write + Send>(
    matcher: &RegexMatcher,
    multiline: bool,
    roots: &[PathBuf],
    scope: &Scope,
    output: &Mutex<W>,
) {
    let Some((first_root, other_roots)) = roots.split_first() else {
        return;
    };
    let mut walker_builder = WalkBuilder::new(first_root);
    for root in other_roots {
        walker_builder.add(root);
    }

    walker_builder.build_parallel().run(|| {
        let matcher = matcher.clone();
        // Allocated once per thread and reused across files.
        let mut buffer = Vec::new();
        let mut searcher = build_searcher(multiline);

        Box::new(move |entry| {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    log::warn!("search walk error: {err}");
                    return WalkState::Continue;
                }
            };
            let path = entry.path();
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                return WalkState::Continue;
            }
            let in_scope = scope.is_unrestricted()
                || roots
                    .iter()
                    .any(|root| path.starts_with(root) && scope.includes(root, path));
            if !in_scope {
                return WalkState::Continue;
            }

            buffer.clear();
            if let Err(err) = search_path_json(&matcher, &mut searcher, path, &mut buffer) {
                log::warn!("search error for {}: {err}", path.display());
            }
            if buffer.is_empty() {
                return WalkState::Continue;
            }

            // A poisoned lock means another search thread panicked, and a failed write that the
            // reader is gone. Either way nobody is left to search for.
            let Ok(mut output) = output.lock() else {
                return WalkState::Quit;
            };
            if output.write_all(&buffer).is_err() {
                return WalkState::Quit;
            }
            WalkState::Continue
        })
    });
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
