use warp_util::local_or_remote_path::LocalOrRemotePath;

use super::*;

fn matcher(pattern: &str, use_regex: bool, use_case_sensitivity: bool) -> Option<NameMatcher> {
    NameMatcher::new(
        pattern,
        &SearchConfig {
            use_regex,
            use_case_sensitivity,
        },
    )
}

fn fuzzy(pattern: &str) -> NameMatcher {
    matcher(pattern, false, false).expect("query should be matchable")
}

fn regex(pattern: &str) -> NameMatcher {
    matcher(pattern, true, false).expect("regex should compile")
}

fn name_match(display_path: &str, score: i64) -> NameMatch {
    NameMatch {
        location: LocalOrRemotePath::Local(display_path.into()),
        display_path: display_path.to_string(),
        highlight_indices: Vec::new(),
        score,
    }
}

fn offer(collector: &mut NameMatchCollector, kind: NameMatchKind, display_path: &str, score: i64) {
    collector.offer(kind, score, display_path, || {
        name_match(display_path, score)
    });
}

fn display_paths(ranked: &RankedNameMatches) -> Vec<&str> {
    ranked
        .matches
        .iter()
        .map(|name_match| name_match.display_path.as_str())
        .collect()
}

#[test]
fn query_without_separator_matches_the_name_only() {
    let matcher = fuzzy("view");

    let matched = matcher
        .match_path("app/src/view.rs")
        .expect("name should match");
    assert_eq!(matched.matched_indices, vec![8, 9, 10, 11]);

    // The query appears in a parent directory, but not in the name.
    assert!(matcher.match_path("view/model.rs").is_none());
}

#[test]
fn name_at_the_root_is_matched_without_an_offset() {
    let matched = fuzzy("read")
        .match_path("README.md")
        .expect("name should match");
    assert_eq!(matched.matched_indices, vec![0, 1, 2, 3]);
}

#[test]
fn folder_names_match_like_file_names() {
    let matcher = fuzzy("search");
    assert!(matcher.match_path("app/src/global_search").is_some());
    assert!(
        matcher
            .match_path("app/src/global_search/view.rs")
            .is_none()
    );
}

#[test]
fn highlight_indices_count_chars_before_the_name() {
    let matched = fuzzy("view")
        .match_path("dönér/view.rs")
        .expect("name should match");
    assert_eq!(matched.matched_indices, vec![6, 7, 8, 9]);
}

#[test]
fn query_with_separator_matches_the_whole_path() {
    let matcher = fuzzy("src/view");
    assert!(matcher.match_path("app/src/view.rs").is_some());
    assert!(matcher.match_path("app/lib/view.rs").is_none());
}

#[test]
fn every_query_term_has_to_match() {
    let matcher = fuzzy("search view");
    assert!(matcher.match_path("src/search_view.rs").is_some());
    assert!(matcher.match_path("src/search_model.rs").is_none());
}

#[test]
fn fuzzy_matching_ignores_case_unless_case_sensitive() {
    let insensitive = fuzzy("readme");
    assert!(insensitive.match_path("docs/README.md").is_some());

    let sensitive = matcher("readme", false, true).expect("query should be matchable");
    assert!(sensitive.match_path("docs/README.md").is_none());
    assert!(sensitive.match_path("docs/readme.md").is_some());
}

#[test]
fn case_sensitive_matching_supports_wildcards_and_terms() {
    let wildcard = matcher("*.rs", false, true).expect("query should be matchable");
    assert!(wildcard.match_path("src/main.rs").is_some());
    assert!(wildcard.match_path("src/main.py").is_none());

    let terms = matcher("Search View", false, true).expect("query should be matchable");
    assert!(terms.match_path("src/SearchView.rs").is_some());
    assert!(terms.match_path("src/searchview.rs").is_none());
}

#[test]
fn regex_matches_the_name_and_highlights_every_match() {
    let matcher = regex(r"^view\.rs$");
    let matched = matcher
        .match_path("app/src/view.rs")
        .expect("name should match");
    assert_eq!(matched.matched_indices, vec![8, 9, 10, 11, 12, 13, 14]);
    assert!(matcher.match_path("app/src/review.rs").is_none());

    let repeated = regex("a").match_path("src/banana").expect("should match");
    assert_eq!(repeated.matched_indices, vec![5, 7, 9]);
}

#[test]
fn regex_respects_the_case_filter() {
    assert!(regex("^VIEW").match_path("src/view.rs").is_some());

    let sensitive = matcher("^VIEW", true, true).expect("regex should compile");
    assert!(sensitive.match_path("src/view.rs").is_none());
    assert!(sensitive.match_path("src/VIEW.rs").is_some());
}

#[test]
fn regex_with_slash_matches_the_whole_path() {
    let matcher = regex("^app/.*view");
    assert!(matcher.match_path("app/src/view.rs").is_some());
    assert!(matcher.match_path("crates/app/view.rs").is_none());
}

#[test]
fn regex_ranks_early_matches_in_short_names_first() {
    let matcher = regex("view");
    let score = |path| matcher.match_path(path).expect("should match").score;

    assert!(score("view.rs") > score("preview.rs"));
    assert!(score("view.rs") > score("view_model.rs"));
}

#[test]
fn regex_matching_nothing_but_empty_strings_is_no_match() {
    assert!(regex("x*").match_path("src/view.rs").is_none());
}

#[test]
fn unmatchable_queries_produce_no_matcher() {
    assert!(matcher("   ", false, false).is_none());
    assert!(matcher("*", false, false).is_none());
    assert!(matcher("fn main\n", false, false).is_none());
    assert!(matcher("(unclosed", true, false).is_none());
    // The same text is a fine literal query.
    assert!(matcher("(unclosed", false, false).is_some());
}

#[test]
fn collector_ranks_by_score_then_path() {
    let mut collector = NameMatchCollector::new(10);
    offer(&mut collector, NameMatchKind::File, "b.rs", 5);
    offer(&mut collector, NameMatchKind::File, "z.rs", 9);
    offer(&mut collector, NameMatchKind::File, "a.rs", 5);

    let results = collector.finish();
    assert_eq!(display_paths(&results.files), vec!["z.rs", "a.rs", "b.rs"]);
    assert_eq!(results.files.total, 3);
}

#[test]
fn collector_keeps_folders_and_files_apart() {
    let mut collector = NameMatchCollector::new(10);
    offer(&mut collector, NameMatchKind::File, "search.rs", 1);
    offer(&mut collector, NameMatchKind::Folder, "search", 1);
    offer(&mut collector, NameMatchKind::Folder, "search_bar", 2);

    let results = collector.finish();
    assert_eq!(display_paths(&results.files), vec!["search.rs"]);
    assert_eq!(
        display_paths(&results.folders),
        vec!["search_bar", "search"]
    );
}

#[test]
fn collector_keeps_only_the_best_matches_but_counts_all() {
    let mut collector = NameMatchCollector::new(3);
    for score in [4, 9, 1, 7, 3, 8] {
        offer(
            &mut collector,
            NameMatchKind::File,
            &format!("file_{score}.rs"),
            score,
        );
    }

    let results = collector.finish();
    assert_eq!(
        display_paths(&results.files),
        vec!["file_9.rs", "file_8.rs", "file_7.rs"]
    );
    assert_eq!(results.files.total, 6);
    assert_eq!(results.folders, RankedNameMatches::default());
}

#[test]
fn collector_only_builds_matches_it_keeps() {
    let mut collector = NameMatchCollector::new(1);
    offer(&mut collector, NameMatchKind::File, "kept.rs", 10);
    collector.offer(NameMatchKind::File, 1, "dropped.rs", || {
        panic!("a match that ranks below the kept ones should not be built")
    });

    assert_eq!(display_paths(&collector.finish().files), vec!["kept.rs"]);
}
