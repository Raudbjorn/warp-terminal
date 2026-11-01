//! What a replace or an undo did, in words for the user.

use std::path::PathBuf;

/// Why a file was left alone by a replace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// The matches of the file are no longer the ones the search showed.
    ChangedSinceSearch,
    /// The file changed while it was being replaced.
    ChangedDuringReplace,
    Binary,
    UnsupportedEncoding,
    Symlink,
    NotAFile,
    TooLarge {
        bytes: u64,
        limit: u64,
    },
    ReadOnly,
    HardLinked,
    Missing,
    /// The file is outside the folders and patterns the search was limited to.
    OutOfScope,
    /// The file is open in an editor that changed between planning the replace and applying it.
    EditedWhileReplacing,
    /// The undo journal has no room for the file, so replacing it could not be undone.
    JournalFull,
    SearchFailed(String),
    ReadFailed(String),
    WriteFailed(String),
}

impl SkipReason {
    pub fn describe(&self) -> String {
        match self {
            SkipReason::ChangedSinceSearch => "changed since the search".to_string(),
            SkipReason::ChangedDuringReplace => "changed during the replace".to_string(),
            SkipReason::Binary => "binary file".to_string(),
            SkipReason::UnsupportedEncoding => "not UTF-8 or UTF-16 text".to_string(),
            SkipReason::Symlink => "symbolic link".to_string(),
            SkipReason::NotAFile => "not a regular file".to_string(),
            SkipReason::TooLarge { bytes, limit } => format!(
                "too large ({} MB, the limit is {} MB)",
                bytes.div_ceil(BYTES_PER_MEGABYTE),
                limit / BYTES_PER_MEGABYTE
            ),
            SkipReason::ReadOnly => "read-only".to_string(),
            SkipReason::HardLinked => "has other hard links".to_string(),
            SkipReason::Missing => "no longer exists".to_string(),
            SkipReason::OutOfScope => "outside the searched folders".to_string(),
            SkipReason::EditedWhileReplacing => "edited during the replace".to_string(),
            SkipReason::JournalFull => "no room left to keep an undo copy".to_string(),
            SkipReason::SearchFailed(error) => format!("could not be searched: {error}"),
            SkipReason::ReadFailed(error) => format!("could not be read: {error}"),
            SkipReason::WriteFailed(error) => format!("could not be written: {error}"),
        }
    }
}

const BYTES_PER_MEGABYTE: u64 = 1024 * 1024;

/// A file a replace left alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedFile {
    pub path: PathBuf,
    pub reason: SkipReason,
}

/// A file a replace changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacedFile {
    pub path: PathBuf,
    pub occurrences: usize,
}

/// Why an undo left a file as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefusalReason {
    /// The file no longer holds what the replace left in it.
    EditedSinceReplace,
    /// The file is open in an editor with changes that were never saved.
    UnsavedChanges,
    Missing,
    /// The copy kept for undoing is missing or damaged.
    JournalDamaged(String),
    ReadFailed(String),
    WriteFailed(String),
}

impl RefusalReason {
    pub fn describe(&self) -> String {
        match self {
            RefusalReason::EditedSinceReplace => "edited since the replace".to_string(),
            RefusalReason::UnsavedChanges => "open with unsaved changes".to_string(),
            RefusalReason::Missing => "no longer exists".to_string(),
            RefusalReason::JournalDamaged(error) => format!("the undo copy is unusable: {error}"),
            RefusalReason::ReadFailed(error) => format!("could not be read: {error}"),
            RefusalReason::WriteFailed(error) => format!("could not be written: {error}"),
        }
    }
}

/// A file an undo left as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefusedFile {
    pub path: PathBuf,
    pub reason: RefusalReason,
}

/// How many occurrences in how many files a replace covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplaceCounts {
    pub occurrences: usize,
    pub files: usize,
}

impl ReplaceCounts {
    pub fn is_empty(&self) -> bool {
        self.occurrences == 0
    }

    /// For example "37 occurrences across 9 files".
    pub fn describe(&self) -> String {
        format!(
            "{} across {}",
            count(self.occurrences, "occurrence", "occurrences"),
            count(self.files, "file", "files")
        )
    }
}

/// The label of the button that replaces everything `counts` covers.
pub fn replace_all_label(counts: ReplaceCounts) -> String {
    format!("Replace {}", counts.describe())
}

/// The question to confirm before replacing everything `counts` covers. An empty replacement
/// removes the matches, which the question says outright.
pub fn confirmation_title(counts: ReplaceCounts, pattern: &str, replacement: &str) -> String {
    if replacement.is_empty() {
        return format!("Delete {} of “{}”?", counts.describe(), shortened(pattern));
    }
    format!(
        "Replace {} of “{}” with “{}”?",
        counts.describe(),
        shortened(pattern),
        shortened(replacement)
    )
}

/// What the confirmation adds under its question when the replacement is empty.
pub const DELETION_WARNING: &str =
    "The replacement is empty, so every match will be deleted and nothing put in its place.";

/// For example "Replaced 37 occurrences in 9 files", then what was skipped and why.
pub fn replace_summary(
    counts: ReplaceCounts,
    skipped: &[SkippedFile],
    cancelled_with_remaining: Option<usize>,
) -> String {
    let mut summary = if counts.is_empty() {
        "Nothing was replaced".to_string()
    } else {
        format!(
            "Replaced {} in {}",
            count(counts.occurrences, "occurrence", "occurrences"),
            count(counts.files, "file", "files")
        )
    };
    if let Some(remaining) = cancelled_with_remaining {
        summary.push_str(&format!(
            ". Stopped with {} left untouched",
            count(remaining, "file", "files")
        ));
    }
    if !skipped.is_empty() {
        summary.push_str(&format!(
            ". Skipped {}",
            count(skipped.len(), "file", "files")
        ));
    }
    summary
}

/// For example "“foo” → “bar”", to name a replace by what it replaced.
pub fn describe_change(pattern: &str, replacement: &str) -> String {
    format!("“{}” → “{}”", shortened(pattern), shortened(replacement))
}

/// For example "Restored 9 files", then what could not be restored.
pub fn undo_summary(
    restored: usize,
    refused: &[RefusedFile],
    cancelled_with_remaining: Option<usize>,
) -> String {
    let mut summary = if restored == 0 {
        "Nothing was restored".to_string()
    } else {
        format!("Restored {}", count(restored, "file", "files"))
    };
    if let Some(remaining) = cancelled_with_remaining {
        summary.push_str(&format!(
            ". Stopped with {} left to restore",
            count(remaining, "file", "files")
        ));
    }
    if !refused.is_empty() {
        summary.push_str(&format!(
            ". Left {} as {}",
            count(refused.len(), "file", "files"),
            if refused.len() == 1 {
                "it is"
            } else {
                "they are"
            }
        ));
    }
    summary
}

fn count(number: usize, singular: &str, plural: &str) -> String {
    if number == 1 {
        format!("1 {singular}")
    } else {
        format!("{number} {plural}")
    }
}

const MAX_QUOTED_CHARS: usize = 40;

/// Keeps a quoted pattern short enough for a dialog and on one line.
fn shortened(text: &str) -> String {
    let single_line = text.replace('\n', "⏎");
    if single_line.chars().count() <= MAX_QUOTED_CHARS {
        return single_line;
    }
    let mut shortened: String = single_line.chars().take(MAX_QUOTED_CHARS).collect();
    shortened.push('…');
    shortened
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
