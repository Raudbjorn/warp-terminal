//! Matching and ranking of file and folder names against a global search query.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::path::MAIN_SEPARATOR;

use fuzzy_match::{
    FuzzyMatchResult, contains_wildcards, match_indices_case_sensitive, match_wildcard_pattern,
};
use regex::{Regex, RegexBuilder};

use super::{NameMatch, NameMatchKind, NameSearchResults, RankedNameMatches, SearchConfig};
use crate::search::files::model::FileSearchModel;

/// Number of best matches kept per kind. Weaker matches only count towards the total.
pub(super) const MAX_RANKED_NAME_MATCHES: usize = 500;

const PATH_SEPARATORS: [char; 2] = ['/', '\\'];

/// How much an earlier regex match outweighs a shorter name when ranking.
const REGEX_MATCH_POSITION_WEIGHT: i64 = 1000;

/// Matches the paths of files and folders against a global search query.
pub(super) struct NameMatcher {
    strategy: MatchStrategy,
    /// Whether the query spells out a path, so that it has to be matched against the whole
    /// relative path instead of the name alone.
    matches_whole_path: bool,
}

enum MatchStrategy {
    Fuzzy { query: String, case_sensitive: bool },
    Regex(Regex),
}

impl NameMatcher {
    /// Returns `None` when no name can match `pattern`.
    pub(super) fn new(pattern: &str, config: &SearchConfig) -> Option<Self> {
        if pattern.contains('\n') {
            return None;
        }

        if config.use_regex {
            let regex = RegexBuilder::new(pattern)
                .case_insensitive(!config.use_case_sensitivity)
                .build()
                .ok()?;
            return Some(Self {
                strategy: MatchStrategy::Regex(regex),
                // A backslash escapes in a regex, so only `/` can mean a path here.
                matches_whole_path: pattern.contains('/'),
            });
        }

        let query = pattern.trim();
        if query.is_empty() || FileSearchModel::should_skip_overly_broad_query(query) {
            return None;
        }
        Some(Self {
            strategy: MatchStrategy::Fuzzy {
                query: query.to_string(),
                case_sensitive: config.use_case_sensitivity,
            },
            matches_whole_path: query.contains(['/', MAIN_SEPARATOR]),
        })
    }

    /// Matches the entry at `relative_path`, returning char indices into `relative_path`.
    pub(super) fn match_path(&self, relative_path: &str) -> Option<FuzzyMatchResult> {
        let name_start = if self.matches_whole_path {
            0
        } else {
            relative_path
                .rfind(PATH_SEPARATORS)
                .map_or(0, |separator| separator + 1)
        };
        let target = &relative_path[name_start..];

        let mut result = match &self.strategy {
            MatchStrategy::Fuzzy {
                query,
                case_sensitive: false,
            } => FileSearchModel::fuzzy_match_path(target, query),
            MatchStrategy::Fuzzy {
                query,
                case_sensitive: true,
            } => fuzzy_match_case_sensitive(target, query),
            MatchStrategy::Regex(regex) => regex_match(regex, target),
        }?;

        let preceding_chars = relative_path[..name_start].chars().count();
        for index in &mut result.matched_indices {
            *index += preceding_chars;
        }
        Some(result)
    }
}

/// Case-sensitive counterpart of [`FileSearchModel::fuzzy_match_path`]: every whitespace-separated
/// term has to match.
fn fuzzy_match_case_sensitive(target: &str, query: &str) -> Option<FuzzyMatchResult> {
    let mut score = 0;
    let mut matched_indices = Vec::new();
    for term in query.split_whitespace() {
        let term_match = if contains_wildcards(term) {
            match_wildcard_pattern(target, term)
        } else {
            match_indices_case_sensitive(target, term)
        }?;
        score += term_match.score;
        matched_indices.extend(term_match.matched_indices);
    }
    matched_indices.sort_unstable();
    matched_indices.dedup();
    Some(FuzzyMatchResult {
        score,
        matched_indices,
    })
}

fn regex_match(regex: &Regex, target: &str) -> Option<FuzzyMatchResult> {
    let mut matches = regex
        .find_iter(target)
        .filter(|found| !found.is_empty())
        .peekable();
    matches.peek()?;

    let mut matched_indices = Vec::new();
    let mut char_count = 0;
    for (char_index, (byte_index, _)) in target.char_indices().enumerate() {
        char_count = char_index + 1;
        while matches
            .peek()
            .is_some_and(|found| found.end() <= byte_index)
        {
            matches.next();
        }
        if matches
            .peek()
            .is_some_and(|found| found.start() <= byte_index)
        {
            matched_indices.push(char_index);
        }
    }

    // A regex match has no relevance of its own, so prefer early matches, then short names.
    let first_match = *matched_indices.first()? as i64;
    Some(FuzzyMatchResult {
        score: -(first_match * REGEX_MATCH_POSITION_WEIGHT + char_count as i64),
        matched_indices,
    })
}

/// Orders two matches by rank: the higher score wins, and equal scores fall back to path order so
/// that ranking does not depend on the order entries were visited in.
fn compare_rank(score: i64, display_path: &str, other_score: i64, other_path: &str) -> Ordering {
    score
        .cmp(&other_score)
        .then_with(|| other_path.cmp(display_path))
}

/// Wrapper whose ordering puts the lowest-ranked match at the top of a max-heap.
struct LowestRankFirst(NameMatch);

impl Ord for LowestRankFirst {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_rank(
            other.0.score,
            &other.0.display_path,
            self.0.score,
            &self.0.display_path,
        )
    }
}

impl PartialOrd for LowestRankFirst {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for LowestRankFirst {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for LowestRankFirst {}

struct BestMatches {
    limit: usize,
    total: usize,
    matches: BinaryHeap<LowestRankFirst>,
}

impl BestMatches {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            total: 0,
            matches: BinaryHeap::new(),
        }
    }

    fn offer(&mut self, score: i64, display_path: &str, build: impl FnOnce() -> NameMatch) {
        self.total += 1;
        if self.matches.len() < self.limit {
            self.matches.push(LowestRankFirst(build()));
            return;
        }
        let Some(mut lowest) = self.matches.peek_mut() else {
            return;
        };
        if compare_rank(score, display_path, lowest.0.score, &lowest.0.display_path).is_gt() {
            *lowest = LowestRankFirst(build());
        }
    }

    fn finish(self) -> RankedNameMatches {
        RankedNameMatches {
            matches: self
                .matches
                .into_sorted_vec()
                .into_iter()
                .map(|ranked| ranked.0)
                .collect(),
            total: self.total,
        }
    }
}

/// Collects name matches, keeping only the best-ranked ones of each kind.
pub(super) struct NameMatchCollector {
    folders: BestMatches,
    files: BestMatches,
}

impl NameMatchCollector {
    /// `limit` is the number of matches kept per kind.
    pub(super) fn new(limit: usize) -> Self {
        Self {
            folders: BestMatches::new(limit),
            files: BestMatches::new(limit),
        }
    }

    /// Records a match with the given `score` and `display_path`. `build` has to produce that
    /// match, and only runs if the match ranks among the best seen so far.
    pub(super) fn offer(
        &mut self,
        kind: NameMatchKind,
        score: i64,
        display_path: &str,
        build: impl FnOnce() -> NameMatch,
    ) {
        let matches = match kind {
            NameMatchKind::Folder => &mut self.folders,
            NameMatchKind::File => &mut self.files,
        };
        matches.offer(score, display_path, build);
    }

    pub(super) fn finish(self) -> NameSearchResults {
        NameSearchResults {
            folders: self.folders.finish(),
            files: self.files.finish(),
        }
    }
}

#[cfg(test)]
#[path = "name_match_tests.rs"]
mod tests;
