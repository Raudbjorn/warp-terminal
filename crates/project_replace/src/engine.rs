//! The search configuration shared by the search worker and by replacing, so that replacing finds
//! exactly the matches the search showed.

use std::io;
use std::ops::{Not, Range};
use std::path::Path;

use grep::printer::JSONBuilder;
use grep::regex::{RegexMatcher, RegexMatcherBuilder};
use grep::searcher::{BinaryDetection, Searcher, SearcherBuilder};
use serde::Deserialize;
use serde_json::value::RawValue;
use thiserror::Error;

use crate::replacement::{ReplaceSpec, preserve_case};

/// Maximum line length (in bytes) the searcher tolerates. Files containing longer lines fail to
/// search, which bounds the memory minified or generated files can take. Only applied in
/// single-line mode, since a multiline search needs the whole file in memory anyway.
const SEARCHER_LINE_HEAP_LIMIT: usize = 64 * 1024;

/// What the user asked to search for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchSpec {
    pub pattern: String,
    pub use_regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

impl SearchSpec {
    /// The regular expression that finds what the user asked for.
    pub fn effective_pattern(&self) -> String {
        let pattern = if self.use_regex {
            self.pattern.clone()
        } else {
            regex::escape(&self.pattern)
        };
        if self.whole_word {
            format!(r"\b{{start-half}}(?:{pattern})\b{{end-half}}")
        } else {
            pattern
        }
    }

    /// Whether a match may span several lines.
    pub fn is_multiline(&self) -> bool {
        self.effective_pattern().contains('\n')
    }

    pub fn ignore_case(&self) -> bool {
        !self.case_sensitive
    }
}

/// Builds the matcher every search and every replace goes through.
pub fn build_matcher(
    patterns: &[String],
    ignore_case: bool,
    multiline: bool,
) -> Result<RegexMatcher, grep::regex::Error> {
    let mut builder = RegexMatcherBuilder::new();
    // `^` and `$` have to match at every line, as the single-line search may hand the matcher
    // several lines at once.
    builder.case_insensitive(ignore_case).multi_line(true);
    if multiline {
        builder.line_terminator(None);
    }
    builder.build_many(patterns)
}

/// Builds the searcher every search and every replace goes through.
pub fn build_searcher(multiline: bool) -> Searcher {
    SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .line_number(true)
        .multi_line(multiline)
        .heap_limit(multiline.not().then_some(SEARCHER_LINE_HEAP_LIMIT))
        .build()
}

/// Searches the file at `path`, appending what was found to `output` as JSON lines.
pub fn search_path_json(
    matcher: &RegexMatcher,
    searcher: &mut Searcher,
    path: &Path,
    output: &mut Vec<u8>,
) -> io::Result<()> {
    let mut printer = JSONBuilder::new().build(output);
    searcher.search_path(matcher, path, printer.sink_with_path(matcher, path))
}

/// The 1-based character column of the byte offset `byte_start` in `lines`, or `None` when the
/// offset does not fall between two characters.
pub fn column_of(lines: &str, byte_start: usize) -> Option<usize> {
    lines
        .get(..byte_start)
        .map(|prefix| prefix.chars().count() + 1)
}

/// One occurrence of the search in a text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundMatch {
    /// Line the search reported the occurrence on. A match spanning several lines, and every
    /// other match found together with it, is reported on the first of them.
    pub line: u32,
    /// 1-based character column, counted from the start of the reported line.
    pub column: usize,
    /// Bytes of the searched text the occurrence covers.
    pub range: Range<usize>,
    pub matched: String,
    /// What the occurrence is replaced with. Empty when the finder has no replacement.
    pub replacement: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FindError {
    #[error("the search pattern is not valid: {0}")]
    InvalidPattern(String),
    #[error("the search failed: {0}")]
    Search(String),
    #[error("the search result could not be read: {0}")]
    Output(String),
}

/// Finds the occurrences of a search in a text the way the search worker finds them in a file,
/// along with what each one is replaced with.
#[derive(Clone, Debug)]
pub struct MatchFinder {
    matcher: RegexMatcher,
    multiline: bool,
    template: Option<Vec<u8>>,
    preserve_case: bool,
}

impl MatchFinder {
    pub fn new(search: &SearchSpec, replace: Option<&ReplaceSpec>) -> Result<Self, FindError> {
        let multiline = search.is_multiline();
        let matcher = build_matcher(
            &[search.effective_pattern()],
            search.ignore_case(),
            multiline,
        )
        .map_err(|err| FindError::InvalidPattern(err.to_string()))?;
        Ok(Self {
            matcher,
            multiline,
            template: replace.map(|replace| replace.template(search).into_bytes()),
            preserve_case: replace.is_some_and(|replace| replace.preserve_case),
        })
    }

    /// Every occurrence in `text`, in the order they appear.
    pub fn find(&self, text: &str) -> Result<Vec<FoundMatch>, FindError> {
        let mut output = Vec::new();
        {
            let mut printer = JSONBuilder::new()
                .replacement(self.template.clone())
                .build(&mut output);
            build_searcher(self.multiline)
                .search_reader(&self.matcher, text.as_bytes(), printer.sink(&self.matcher))
                .map_err(|err| FindError::Search(err.to_string()))?;
        }

        let mut found = Vec::new();
        for line in output.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let envelope: Envelope =
                serde_json::from_slice(line).map_err(|err| FindError::Output(err.to_string()))?;
            if envelope.kind != "match" {
                continue;
            }
            let data: MatchData = serde_json::from_str(envelope.data.get())
                .map_err(|err| FindError::Output(err.to_string()))?;
            self.collect(text, data, &mut found)?;
        }
        Ok(found)
    }

    fn collect(
        &self,
        text: &str,
        data: MatchData,
        found: &mut Vec<FoundMatch>,
    ) -> Result<(), FindError> {
        let lines = data
            .lines
            .text
            .ok_or_else(|| FindError::Output("a matched line is not text".to_string()))?;
        let line = data
            .line_number
            .and_then(|line| u32::try_from(line).ok())
            .ok_or_else(|| FindError::Output("missing line number".to_string()))?;
        let offset = usize::try_from(data.absolute_offset)
            .map_err(|err| FindError::Output(err.to_string()))?;

        for submatch in data.submatches {
            // A pattern that matches bytes can stop in the middle of a character. Such a match
            // has no place in a text and is left out, as the search results leave it out.
            let (Some(matched), Some(column)) =
                (submatch.matched.text, column_of(&lines, submatch.start))
            else {
                continue;
            };
            let range = offset + submatch.start..offset + submatch.end;
            // The offsets come from the search library; never edit a range that does not hold
            // what it said it matched.
            if text.get(range.clone()) != Some(matched.as_str()) {
                return Err(FindError::Output(
                    "a match is not where the search reported it".to_string(),
                ));
            }

            let replacement = match submatch.replacement {
                Some(replacement) => {
                    let replacement = replacement.text.ok_or_else(|| {
                        FindError::Output("a replacement is not text".to_string())
                    })?;
                    if self.preserve_case {
                        preserve_case(&matched, &replacement)
                    } else {
                        replacement
                    }
                }
                None => String::new(),
            };
            found.push(FoundMatch {
                line,
                column,
                range,
                matched,
                replacement,
            });
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(borrow)]
    data: &'a RawValue,
}

#[derive(Deserialize)]
struct MatchData {
    lines: Data,
    line_number: Option<u64>,
    absolute_offset: u64,
    submatches: Vec<SubmatchData>,
}

#[derive(Deserialize)]
struct SubmatchData {
    #[serde(rename = "match")]
    matched: Data,
    replacement: Option<Data>,
    start: usize,
    end: usize,
}

/// Text the search library reports. It has no `text` when the bytes are not valid UTF-8.
#[derive(Deserialize)]
struct Data {
    text: Option<String>,
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
