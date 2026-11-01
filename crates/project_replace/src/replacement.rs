//! Working out the text that takes the place of a match.

use regex::{Regex, RegexBuilder};

use crate::engine::SearchSpec;

/// What the user asked to replace matches with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplaceSpec {
    /// In a regular expression search, `$1` and `${name}` stand for capture groups and `$$` for
    /// a dollar sign. Otherwise the text is used as it is.
    pub replacement: String,
    pub preserve_case: bool,
}

impl ReplaceSpec {
    /// The replacement in the syntax the search library expands, where `$` introduces a capture
    /// group.
    pub(crate) fn template(&self, search: &SearchSpec) -> String {
        if search.use_regex {
            delimit_numbered_groups(&self.replacement)
        } else {
            self.replacement.replace('$', "$$")
        }
    }
}

/// Rewrites every `$1` of `replacement` as `${1}`, so that a numbered capture group can be
/// followed by text. The search library reads `$1_total` as a group named `1_total`, which does
/// not exist and expands to nothing.
pub(crate) fn delimit_numbered_groups(replacement: &str) -> String {
    let mut template = String::with_capacity(replacement.len() + 4);
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        template.push(c);
        if c != '$' {
            continue;
        }
        match chars.peek() {
            // An escaped dollar sign, which must not start a reference with what follows it.
            Some('$') => template.extend(chars.next()),
            Some(digit) if digit.is_ascii_digit() => {
                template.push('{');
                while let Some(digit) = chars.next_if(char::is_ascii_digit) {
                    template.push(digit);
                }
                template.push('}');
            }
            Some(_) | None => {}
        }
    }
    template
}

/// Carries the casing of `original` over to `replacement`.
///
/// - Plain words take the casing of the original: "begin" replaced with "end" becomes "End" for
///   "Begin" and "END" for "BEGIN".
/// - Hyphenated, underscored and camelCase or PascalCase text is cased part by part, keeping the
///   original delimiter.
/// - Text delimited differently on the two sides is cased as a whole.
pub fn preserve_case(original: &str, replacement: &str) -> String {
    let (mut original_parts, mut original_delimiter) = split_text(original);
    let (mut replacement_parts, replacement_delimiter) = split_text(replacement);
    if original_delimiter != replacement_delimiter {
        (original_parts, original_delimiter) = (vec![original.to_string()], None);
        replacement_parts = vec![replacement.to_string()];
    }

    let parts: Vec<String> = replacement_parts
        .iter()
        .enumerate()
        .map(
            |(index, replacement_part)| match original_parts.get(index) {
                Some(original_part) => preserve_case_for_part(original_part, replacement_part),
                None => replacement_part.clone(),
            },
        )
        .collect();

    let delimiter = original_delimiter
        .map(|delimiter| delimiter.to_string())
        .unwrap_or_default();
    parts.join(&delimiter)
}

/// Splits text at hyphens, at underscores, or else at camelCase and PascalCase boundaries.
/// Returns the parts and the delimiter that separated them, if there was one.
fn split_text(text: &str) -> (Vec<String>, Option<char>) {
    if text.is_empty() {
        return (vec![], None);
    }

    for delimiter in ['-', '_'] {
        if text.contains(delimiter) {
            let parts = text.split(delimiter).map(str::to_string).collect();
            return (parts, Some(delimiter));
        }
    }

    let mut parts = Vec::new();
    let mut current_part = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if !ch.is_uppercase() || current_part.is_empty() {
            current_part.push(ch);
            continue;
        }
        let starts_new_part = match chars.peek() {
            // An uppercase letter before a lowercase one starts a word, like the "P" of
            // "HTMLParser"; otherwise it continues a run of capitals.
            Some(next) => next.is_lowercase(),
            None => !current_part
                .chars()
                .next_back()
                .is_some_and(|previous| previous.is_uppercase()),
        };
        if starts_new_part {
            parts.push(std::mem::take(&mut current_part));
        }
        current_part.push(ch);
    }

    if !current_part.is_empty() {
        parts.push(current_part);
    }
    (parts, None)
}

fn preserve_case_for_part(original_part: &str, replacement_part: &str) -> String {
    if original_part.chars().all(|c| c.is_lowercase()) {
        return replacement_part.to_lowercase();
    }
    if original_part.chars().all(|c| c.is_uppercase()) {
        return replacement_part.to_uppercase();
    }
    if !original_part.chars().next().is_some_and(char::is_uppercase) {
        return replacement_part.to_string();
    }

    let mut chars = replacement_part.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut result: String = first.to_uppercase().collect();
    result.push_str(&chars.as_str().to_lowercase());
    result
}

/// Expands capture group references in the replacement of a find-and-replace within one editor.
pub struct CaptureExpander {
    regex: Regex,
}

impl CaptureExpander {
    /// Returns `None` when `pattern` is not a valid regular expression.
    pub fn new(pattern: &str, case_sensitive: bool, whole_word: bool) -> Option<Self> {
        let regex = RegexBuilder::new(&in_editor_pattern(pattern, true, whole_word))
            .case_insensitive(!case_sensitive)
            .multi_line(true)
            .build()
            .ok()?;
        Some(Self { regex })
    }

    /// The text to put in place of `matched`. `before` and `after` are the characters next to
    /// the match, which anchors and word boundaries in the pattern depend on. Returns `None`
    /// when the pattern does not match exactly `matched` there.
    pub fn expand(
        &self,
        before: Option<char>,
        matched: &str,
        after: Option<char>,
        replacement: &str,
    ) -> Option<String> {
        let mut haystack = String::with_capacity(matched.len() + 8);
        haystack.extend(before);
        let start = haystack.len();
        haystack.push_str(matched);
        let end = haystack.len();
        haystack.extend(after);

        let captures = self.regex.captures_at(&haystack, start)?;
        let whole = captures.get(0)?;
        if whole.start() != start || whole.end() != end {
            return None;
        }
        let mut expanded = String::new();
        captures.expand(&delimit_numbered_groups(replacement), &mut expanded);
        Some(expanded)
    }
}

/// The regular expression a find within one editor runs for `query`.
///
/// Whole words are delimited by ASCII word characters only: the editor's search engine gives up
/// on text with other characters when a pattern has a Unicode word boundary.
pub fn in_editor_pattern(query: &str, use_regex: bool, whole_word: bool) -> String {
    let pattern = if use_regex {
        query.to_string()
    } else {
        regex::escape(query)
    };
    if whole_word {
        format!(r"(?-u:\b{{start-half}})(?:{pattern})(?-u:\b{{end-half}})")
    } else {
        pattern
    }
}

#[cfg(test)]
#[path = "replacement_tests.rs"]
mod tests;
