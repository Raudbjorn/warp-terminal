//! Which search results a replace covers.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::apply::FileTarget;
use crate::plan::{ExpectedMatch, FileExpectation};
use crate::report::ReplaceCounts;

/// The search results the user set aside so that a replace leaves them alone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dismissals {
    files: HashSet<PathBuf>,
    matches: HashSet<(PathBuf, u32, usize)>,
}

impl Dismissals {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.matches.is_empty()
    }

    pub fn clear(&mut self) {
        self.files.clear();
        self.matches.clear();
    }

    /// Sets a whole file aside, or takes it back in when it already was.
    pub fn toggle_file(&mut self, path: &Path) {
        if !self.files.remove(path) {
            self.files.insert(path.to_path_buf());
        }
    }

    /// Sets one match aside, or takes it back in when it already was.
    pub fn toggle_match(&mut self, path: &Path, line: u32, column: usize) {
        let key = (path.to_path_buf(), line, column);
        if !self.matches.remove(&key) {
            self.matches.insert(key);
        }
    }

    /// Adjusts to a replace or an undo having changed `files`. The matches left in a changed
    /// file may have moved, so a file that had matches set aside is set aside as a whole rather
    /// than risk replacing one of them where it is now.
    pub fn files_changed(&mut self, files: &[PathBuf]) {
        for file in files {
            let matches_before = self.matches.len();
            self.matches.retain(|(path, _, _)| path != file);
            if self.matches.len() != matches_before {
                self.files.insert(file.clone());
            }
        }
    }

    pub fn is_file_dismissed(&self, path: &Path) -> bool {
        self.files.contains(path)
    }

    /// Whether the match was set aside, on its own or along with its file.
    pub fn is_match_dismissed(&self, path: &Path, line: u32, column: usize) -> bool {
        self.files.contains(path) || self.matches.contains(&(path.to_path_buf(), line, column))
    }
}

/// The part of the search results to replace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection<'a> {
    /// Everything that was not set aside.
    All,
    /// Everything in one file that was not set aside.
    File(&'a Path),
    /// One match.
    Match {
        path: &'a Path,
        line: u32,
        column: usize,
    },
}

/// Gathers the rows of the search results into the files to replace in.
///
/// Every row of a file has to be added, whether or not it is to be replaced: replacing checks
/// the file against all of them.
pub struct TargetsBuilder<'a> {
    dismissals: &'a Dismissals,
    selection: Selection<'a>,
    targets: Vec<FileTarget>,
    index_by_path: HashMap<PathBuf, usize>,
    seen: HashSet<(usize, u32, usize)>,
}

impl<'a> TargetsBuilder<'a> {
    pub fn new(dismissals: &'a Dismissals, selection: Selection<'a>) -> Self {
        Self {
            dismissals,
            selection,
            targets: Vec::new(),
            index_by_path: HashMap::new(),
            seen: HashSet::new(),
        }
    }

    /// Adds the match at the bytes `submatch` of `line_text`. A row listed more than once, as
    /// happens for a file that shows under two folders, counts once.
    pub fn add_row(
        &mut self,
        path: &Path,
        line: u32,
        column: usize,
        line_text: &str,
        submatch: Range<usize>,
    ) {
        // A match that ends inside a character cannot be replaced, and replacing leaves such
        // matches out when it checks the file.
        let Some(matched) = line_text.get(submatch) else {
            return;
        };
        let (in_selection, replace) = match self.selection {
            Selection::All => (
                true,
                !self.dismissals.is_match_dismissed(path, line, column),
            ),
            Selection::File(selected) => (
                selected == path,
                !self.dismissals.is_match_dismissed(path, line, column),
            ),
            Selection::Match {
                path: selected,
                line: selected_line,
                column: selected_column,
            } => (
                selected == path,
                selected_line == line && selected_column == column,
            ),
        };
        if !in_selection {
            return;
        }

        let index = match self.index_by_path.get(path) {
            Some(index) => *index,
            None => {
                let index = self.targets.len();
                self.targets.push(FileTarget {
                    path: path.to_path_buf(),
                    expectation: FileExpectation::default(),
                });
                self.index_by_path.insert(path.to_path_buf(), index);
                index
            }
        };
        if !self.seen.insert((index, line, column)) {
            return;
        }
        self.targets[index].expectation.matches.push(ExpectedMatch {
            line,
            column,
            matched: matched.to_string(),
            replace,
        });
    }

    /// Notes that the search may have stopped partway through the file at `path`.
    pub fn mark_truncated(&mut self, path: &Path) {
        if let Some(index) = self.index_by_path.get(path) {
            self.targets[*index].expectation.may_be_truncated = true;
        }
    }

    /// The files that have something to replace.
    pub fn build(self) -> Vec<FileTarget> {
        self.targets
            .into_iter()
            .filter(|target| target.expectation.replace_count() > 0)
            .collect()
    }
}

pub fn count_targets(targets: &[FileTarget]) -> ReplaceCounts {
    ReplaceCounts {
        occurrences: targets
            .iter()
            .map(|target| target.expectation.replace_count())
            .sum(),
        files: targets.len(),
    }
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
