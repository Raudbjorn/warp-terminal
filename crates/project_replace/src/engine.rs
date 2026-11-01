//! The search configuration shared by the search worker and by replacing, so that replacing finds
//! exactly the matches the search showed.

use std::cell::Cell;
use std::io::{self, Read};
use std::ops::{Not, Range};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use grep::matcher::{Captures, Match, Matcher};
use grep::printer::JSONBuilder;
use grep::regex::{RegexMatcher, RegexMatcherBuilder};
use grep::searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkError, SinkMatch};
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
    #[error("replacement exceeds the {limit} byte limit")]
    TooLarge { bytes: usize, limit: usize },
    #[error("replacement was cancelled")]
    Cancelled,
}

impl SinkError for FindError {
    fn error_message<T: std::fmt::Display>(message: T) -> Self {
        Self::Search(message.to_string())
    }
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

    /// Every occurrence in `text`, bounded even for callers collecting all results.
    pub fn find(&self, text: &str) -> Result<Vec<FoundMatch>, FindError> {
        let mut found = Vec::new();
        let mut remaining = crate::apply::DEFAULT_MAX_FILE_BYTES as usize;
        self.visit(text, &AtomicBool::new(false), |matched| {
            let cost = std::mem::size_of::<FoundMatch>().saturating_add(matched.matched.len());
            remaining = remaining.checked_sub(cost).ok_or(FindError::TooLarge {
                bytes: cost,
                limit: remaining,
            })?;
            let replacement = self.expand(&matched, remaining)?;
            remaining -= replacement.len();
            found.push(FoundMatch {
                line: matched.line,
                column: matched.column,
                range: matched.range.clone(),
                matched: matched.matched.to_owned(),
                replacement,
            });
            Ok(true)
        })?;
        Ok(found)
    }

    /// Visits borrowed matches without collecting JSON or expanding replacements. Returning
    /// false stops at the selected search prefix instead of scanning the rest of the file.
    pub(crate) fn visit(
        &self,
        text: &str,
        cancel: &AtomicBool,
        visitor: impl FnMut(MatchContext<'_>) -> Result<bool, FindError>,
    ) -> Result<(), FindError> {
        let result = build_searcher(self.multiline).search_reader(
            &self.matcher,
            CancelReader {
                remaining: text.as_bytes(),
                cancel,
            },
            MatchSink {
                finder: self,
                text,
                cancel,
                visitor,
            },
        );
        if cancel.load(Ordering::Relaxed) {
            Err(FindError::Cancelled)
        } else {
            result
        }
    }

    pub(crate) fn expand(
        &self,
        matched: &MatchContext<'_>,
        limit: usize,
    ) -> Result<String, FindError> {
        let Some(template) = &self.template else {
            return Ok(String::new());
        };
        // Unicode case conversion can triple UTF-8 byte length.
        let expansion_limit = if self.preserve_case { limit / 3 } else { limit };
        if template.len() > expansion_limit {
            return Err(FindError::TooLarge {
                bytes: template.len(),
                limit: expansion_limit,
            });
        }
        let mut captures = self
            .matcher
            .new_captures()
            .map_err(FindError::error_message)?;
        if !self
            .matcher
            .captures_at(matched.context, matched.start, &mut captures)
            .map_err(FindError::error_message)?
        {
            return Err(FindError::Output("a match lost its captures".to_owned()));
        }
        // Ask the same interpolator to count capture bytes before allowing it to copy them.
        let counted = CaptureLengths {
            inner: &captures,
            bytes: Cell::new(0),
        };
        let mut output = Vec::new();
        counted.interpolate(
            |name| self.matcher.capture_index(name),
            &[],
            template,
            &mut output,
        );
        let bytes = output.len().saturating_add(counted.bytes.get());
        if bytes > expansion_limit {
            return Err(FindError::TooLarge {
                bytes,
                limit: expansion_limit,
            });
        }
        output.clear();
        output.reserve_exact(bytes);
        captures.interpolate(
            |name| self.matcher.capture_index(name),
            matched.context,
            template,
            &mut output,
        );
        let replacement =
            String::from_utf8(output).map_err(|err| FindError::Output(err.to_string()))?;
        Ok(if self.preserve_case {
            preserve_case(matched.matched, &replacement)
        } else {
            replacement
        })
    }
}

struct CaptureLengths<'a, C> {
    inner: &'a C,
    bytes: Cell<usize>,
}

impl<C: Captures> Captures for CaptureLengths<'_, C> {
    fn len(&self) -> usize {
        self.inner.len()
    }
    fn get(&self, index: usize) -> Option<Match> {
        let matched = self.inner.get(index)?;
        self.bytes
            .set(self.bytes.get().saturating_add(matched.len()));
        Some(Match::new(0, 0))
    }
}

pub(crate) struct MatchContext<'a> {
    pub line: u32,
    pub column: usize,
    pub range: Range<usize>,
    pub matched: &'a str,
    context: &'a [u8],
    start: usize,
}

struct CancelReader<'a> {
    remaining: &'a [u8],
    cancel: &'a AtomicBool,
}

impl Read for CancelReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("replacement cancelled"));
        }
        self.remaining.read(output)
    }
}

struct MatchSink<'a, F> {
    finder: &'a MatchFinder,
    text: &'a str,
    cancel: &'a AtomicBool,
    visitor: F,
}

impl<F: FnMut(MatchContext<'_>) -> Result<bool, FindError>> Sink for MatchSink<'_, F> {
    type Error = FindError;
    fn matched(&mut self, searcher: &Searcher, block: &SinkMatch<'_>) -> Result<bool, FindError> {
        let range = block.bytes_range_in_buffer();
        let mut bytes = block.buffer();
        // Keep the same context and trailing-newline rules as grep's JSON printer.
        if searcher.multi_line_with_matcher(&self.finder.matcher) {
            const JSON_LOOK_AHEAD_BYTES: usize = 128;
            bytes = &bytes[..bytes
                .len()
                .min(range.end.saturating_add(JSON_LOOK_AHEAD_BYTES))];
        } else {
            let end =
                range.end - usize::from(searcher.line_terminator().is_suffix(&bytes[..range.end]));
            bytes = &bytes[..end];
        }
        let offset =
            usize::try_from(block.absolute_byte_offset()).map_err(FindError::error_message)?;
        let line = block
            .line_number()
            .and_then(|line| u32::try_from(line).ok())
            .ok_or_else(|| FindError::Output("missing line number".to_owned()))?;
        let mut keep_going = true;
        let mut previous_start = 0;
        let mut previous_column = 1;
        self.finder
            .matcher
            .try_find_iter_at(bytes, range.start, |found| {
                if self.cancel.load(Ordering::Relaxed) {
                    return Err(FindError::Cancelled);
                }
                if found.start() >= range.end {
                    return Ok(false);
                }
                let start = found.start() - range.start;
                let end = found.end() - range.start;
                let Some(matched) = self.text.get(offset + start..offset + end) else {
                    return Ok(true);
                };
                let Some(prefix) = self.text.get(offset + previous_start..offset + start) else {
                    return Ok(true);
                };
                let column = previous_column + prefix.chars().count();
                previous_start = start;
                previous_column = column;
                keep_going = (self.visitor)(MatchContext {
                    line,
                    column,
                    range: offset + start..offset + end,
                    matched,
                    context: bytes,
                    start: found.start(),
                })?;
                Ok(keep_going)
            })
            .map_err(FindError::error_message)??;
        Ok(keep_going)
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
