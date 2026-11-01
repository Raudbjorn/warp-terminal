//! Turning the matches of a file into edits, after checking that they are still the matches the
//! search showed.

use std::borrow::Cow;
use std::ops::Range;

use crate::engine::FoundMatch;

/// A match of a file as the search results showed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedMatch {
    pub line: u32,
    /// 1-based character column, counted from the start of the reported line.
    pub column: usize,
    pub matched: String,
    /// Whether to replace the match. Matches the user set aside are still expected to be there.
    pub replace: bool,
}

/// Every match of one file as the search results showed them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileExpectation {
    pub matches: Vec<ExpectedMatch>,
    /// Whether the search may have stopped partway through the file, in which case the file may
    /// hold more matches after the expected ones.
    pub may_be_truncated: bool,
}

impl FileExpectation {
    pub fn replace_count(&self) -> usize {
        self.matches
            .iter()
            .filter(|expected| expected.replace)
            .count()
    }
}

/// One replacement of a range of bytes of a text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub replacement: String,
}

/// The matches of a file are no longer the ones the search showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Misaligned;

/// The edits that replace the matches the user kept, in the order of the text.
///
/// `ignore_line_endings` compares matched text without regard to `\r\n` against `\n`, for text
/// held by an editor, which normalizes line endings.
pub fn plan_edits(
    found: &[FoundMatch],
    expectation: &FileExpectation,
    ignore_line_endings: bool,
) -> Result<Vec<TextEdit>, Misaligned> {
    let mut expected: Vec<&ExpectedMatch> = expectation.matches.iter().collect();
    expected.sort_by_key(|expected| (expected.line, expected.column));

    let lengths_agree = if expectation.may_be_truncated {
        found.len() >= expected.len()
    } else {
        found.len() == expected.len()
    };
    if !lengths_agree {
        return Err(Misaligned);
    }

    let mut edits = Vec::new();
    let mut previous_end = 0;
    for (found, expected) in found.iter().zip(expected) {
        let same_text = if ignore_line_endings {
            without_carriage_returns(&found.matched) == without_carriage_returns(&expected.matched)
        } else {
            found.matched == expected.matched
        };
        if found.line != expected.line || found.column != expected.column || !same_text {
            return Err(Misaligned);
        }
        // Edits are applied front to back, so they have to be in order and apart.
        if found.range.start < previous_end || found.range.end < found.range.start {
            return Err(Misaligned);
        }
        previous_end = found.range.end;

        if expected.replace {
            edits.push(TextEdit {
                range: found.range.clone(),
                replacement: found.replacement.clone(),
            });
        }
    }
    Ok(edits)
}

fn without_carriage_returns(text: &str) -> Cow<'_, str> {
    if text.contains('\r') {
        Cow::Owned(
            text.replace("\r\n", "\n")
                .trim_end_matches('\r')
                .to_string(),
        )
    } else {
        Cow::Borrowed(text)
    }
}

/// Applies `edits`, which must be in order and apart, to `text`.
pub fn apply_edits(text: &str, edits: &[TextEdit]) -> String {
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;
    for edit in edits {
        result.push_str(&text[cursor..edit.range.start]);
        result.push_str(&edit.replacement);
        cursor = edit.range.end;
    }
    result.push_str(&text[cursor..]);
    result
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
