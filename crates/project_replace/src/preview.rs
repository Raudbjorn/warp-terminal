//! Showing what a replace would do to one search result, before anything is replaced.

use std::ops::Range;

use grep::matcher::{Captures, Matcher};
use grep::regex::RegexMatcher;

use crate::engine::{SearchSpec, build_matcher};
use crate::replacement::{ReplaceSpec, preserve_case};

/// Characters of context shown before the match.
const CONTEXT_BEFORE_CHARS: usize = 15;
/// Characters of context shown after the replacement.
const CONTEXT_AFTER_CHARS: usize = 120;
/// Longest stretch of removed or inserted text shown in full.
const MAX_CHANGE_CHARS: usize = 60;

const ELLIPSIS: char = '…';
const LINE_BREAK_SYMBOL: char = '⏎';

/// Works out the replacement of search results for display.
pub struct Previewer {
    /// Present for a regular expression search, whose replacement refers to capture groups.
    matcher: Option<RegexMatcher>,
    replacement: String,
    preserve_case: bool,
}

impl Previewer {
    pub fn new(search: &SearchSpec, replace: &ReplaceSpec) -> Self {
        let matcher = if search.use_regex {
            build_matcher(
                &[search.effective_pattern()],
                search.ignore_case(),
                search.is_multiline(),
            )
            .ok()
        } else {
            None
        };
        Self {
            matcher,
            replacement: replace.replacement.clone(),
            preserve_case: replace.preserve_case,
        }
    }

    /// What the match at the bytes `range` of `line_text` would be replaced with.
    ///
    /// Capture groups are left unexpanded when the match cannot be found again in `line_text`
    /// alone, which happens when it depends on text around the line. Replacing itself works on
    /// the whole file and does not have that limit.
    pub fn replacement_for(&self, line_text: &str, range: Range<usize>) -> String {
        let Some(matched) = line_text.get(range.clone()) else {
            return self.replacement.clone();
        };
        let expanded = self
            .matcher
            .as_ref()
            .and_then(|matcher| expand(matcher, line_text, &range, &self.replacement))
            .unwrap_or_else(|| self.replacement.clone());
        if self.preserve_case {
            preserve_case(matched, &expanded)
        } else {
            expanded
        }
    }
}

fn expand(
    matcher: &RegexMatcher,
    line_text: &str,
    range: &Range<usize>,
    replacement: &str,
) -> Option<String> {
    let haystack = line_text.as_bytes();
    let mut captures = matcher.new_captures().ok()?;
    if !matcher
        .captures_at(haystack, range.start, &mut captures)
        .ok()?
    {
        return None;
    }
    let whole = captures.get(0)?;
    if whole.start() != range.start || whole.end() != range.end {
        return None;
    }

    let mut expanded = Vec::new();
    captures.interpolate(
        |name| matcher.capture_index(name),
        haystack,
        replacement.as_bytes(),
        &mut expanded,
    );
    String::from_utf8(expanded).ok()
}

/// One search result laid out on a single line with its replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowPreview {
    /// The context before the match, the matched text, its replacement, then the context after.
    pub text: String,
    /// Character indices of `text` holding the matched text, which the replace removes.
    pub removed: Range<usize>,
    /// Character indices of `text` holding the replacement.
    pub inserted: Range<usize>,
}

/// Lays out the match at the bytes `range` of `line_text` next to `replacement`.
pub fn preview_row(line_text: &str, range: Range<usize>, replacement: &str) -> RowPreview {
    let (Some(before), Some(matched), Some(after)) = (
        line_text.get(..range.start),
        line_text.get(range.clone()),
        line_text.get(range.end..),
    ) else {
        return RowPreview {
            text: line_text.to_string(),
            removed: 0..0,
            inserted: 0..0,
        };
    };

    // Only the line the match starts and ends on is shown.
    let before = before.rsplit('\n').next().unwrap_or_default();
    let after = after.split('\n').next().unwrap_or_default();

    let mut text = String::new();
    let before_chars = before.chars().count();
    if before_chars > CONTEXT_BEFORE_CHARS {
        text.push(ELLIPSIS);
        text.extend(before.chars().skip(before_chars - CONTEXT_BEFORE_CHARS));
    } else {
        text.push_str(before);
    }

    let removed_start = text.chars().count();
    push_change(&mut text, matched);
    let inserted_start = text.chars().count();
    push_change(&mut text, replacement);
    let inserted_end = text.chars().count();

    let after = after.trim_end_matches('\r');
    text.extend(after.chars().take(CONTEXT_AFTER_CHARS));
    if after.chars().count() > CONTEXT_AFTER_CHARS {
        text.push(ELLIPSIS);
    }

    RowPreview {
        text,
        removed: removed_start..inserted_start,
        inserted: inserted_start..inserted_end,
    }
}

/// Appends removed or inserted text on one line, shortened when it is long.
fn push_change(text: &mut String, change: &str) {
    let single_line = change
        .chars()
        .filter(|ch| *ch != '\r')
        .map(|ch| if ch == '\n' { LINE_BREAK_SYMBOL } else { ch });
    text.extend(single_line.clone().take(MAX_CHANGE_CHARS));
    if single_line.count() > MAX_CHANGE_CHARS {
        text.push(ELLIPSIS);
    }
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;
