use std::ops::Range;
use std::time::Duration;

use similar::{DiffTag, TextDiff};

/// Upper bound on the time spent searching for an optimal diff. Past it the diff is still
/// correct, only coarser.
const DIFF_TIMEOUT: Duration = Duration::from_millis(500);

/// A replacement of part of a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    /// The replaced range of the original text, in characters.
    pub range: Range<usize>,
    pub new_text: String,
}

/// The edits that turn `old` into `new`, ordered by position and never overlapping. Each run of
/// changed lines becomes one edit, narrowed to the characters that actually differ, so cursors
/// and selections outside the changed spans stay where they are.
pub fn minimal_edits(old: &str, new: &str) -> Vec<TextEdit> {
    if old == new {
        return Vec::new();
    }
    let diff = TextDiff::configure()
        .timeout(DIFF_TIMEOUT)
        .diff_lines(old, new);
    let old_lines = diff.old_slices();
    let new_lines = diff.new_slices();

    let mut old_line_starts = Vec::with_capacity(old_lines.len() + 1);
    let mut offset = 0;
    for line in old_lines {
        old_line_starts.push(offset);
        offset += line.chars().count();
    }
    old_line_starts.push(offset);

    let mut hunks: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    for op in diff.ops() {
        match op.tag() {
            DiffTag::Equal => {}
            DiffTag::Delete | DiffTag::Insert | DiffTag::Replace => {
                let (old_range, new_range) = (op.old_range(), op.new_range());
                match hunks.last_mut() {
                    Some((last_old, last_new)) if last_old.end == old_range.start => {
                        last_old.end = old_range.end;
                        last_new.end = new_range.end;
                    }
                    _ => hunks.push((old_range, new_range)),
                }
            }
        }
    }

    hunks
        .into_iter()
        .filter_map(|(old_range, new_range)| {
            narrowed_edit(
                old_line_starts[old_range.start],
                &old_lines[old_range].concat(),
                &new_lines[new_range].concat(),
            )
        })
        .collect()
}

/// Applies `edits`, as produced by [`minimal_edits`], to `old`.
pub fn apply_edits(old: &str, edits: &[TextEdit]) -> String {
    let mut result = String::with_capacity(old.len());
    let mut chars = old.chars();
    let mut position = 0;
    for edit in edits {
        result.extend(chars.by_ref().take(edit.range.start - position));
        result.push_str(&edit.new_text);
        chars.by_ref().take(edit.range.len()).for_each(drop);
        position = edit.range.end;
    }
    result.extend(chars);
    result
}

/// Replaces `old`, which starts at character `start`, with `new`, leaving out the characters the
/// two share at either end.
fn narrowed_edit(start: usize, old: &str, new: &str) -> Option<TextEdit> {
    let old_len = old.chars().count();
    let new_len = new.chars().count();
    let prefix = old
        .chars()
        .zip(new.chars())
        .take_while(|(old, new)| old == new)
        .count();
    let suffix = old
        .chars()
        .rev()
        .zip(new.chars().rev())
        .take(old_len.min(new_len) - prefix)
        .take_while(|(old, new)| old == new)
        .count();
    if prefix + suffix == old_len && old_len == new_len {
        return None;
    }
    Some(TextEdit {
        range: start + prefix..start + old_len - suffix,
        new_text: new
            .chars()
            .skip(prefix)
            .take(new_len - prefix - suffix)
            .collect(),
    })
}

#[cfg(test)]
#[path = "edits_tests.rs"]
mod tests;
