use super::*;

fn found(line: u32, column: usize, range: Range<usize>, matched: &str) -> FoundMatch {
    FoundMatch {
        line,
        column,
        range,
        matched: matched.to_string(),
        replacement: format!("<{matched}>"),
    }
}

fn expected(line: u32, column: usize, matched: &str, replace: bool) -> ExpectedMatch {
    ExpectedMatch {
        line,
        column,
        matched: matched.to_string(),
        replace,
    }
}

fn expectation(matches: Vec<ExpectedMatch>) -> FileExpectation {
    FileExpectation {
        matches,
        may_be_truncated: false,
    }
}

#[test]
fn matches_that_line_up_become_edits() {
    let found = [found(1, 1, 0..3, "foo"), found(2, 5, 12..15, "foo")];
    let expectation = expectation(vec![
        expected(2, 5, "foo", true),
        expected(1, 1, "foo", true),
    ]);
    let edits = plan_edits(&found, &expectation, false).unwrap();
    assert_eq!(
        edits,
        vec![
            TextEdit {
                range: 0..3,
                replacement: "<foo>".to_string()
            },
            TextEdit {
                range: 12..15,
                replacement: "<foo>".to_string()
            },
        ]
    );
}

#[test]
fn matches_set_aside_are_checked_but_not_replaced() {
    let found = [found(1, 1, 0..3, "foo"), found(2, 5, 12..15, "foo")];
    let expectation = expectation(vec![
        expected(1, 1, "foo", false),
        expected(2, 5, "foo", true),
    ]);
    let edits = plan_edits(&found, &expectation, false).unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].range, 12..15);
    assert_eq!(expectation.replace_count(), 1);
}

#[test]
fn a_file_that_changed_since_the_search_is_misaligned() {
    let shown = expectation(vec![
        expected(1, 1, "foo", true),
        expected(2, 5, "foo", true),
    ]);

    let moved = [found(1, 1, 0..3, "foo"), found(3, 5, 20..23, "foo")];
    assert_eq!(plan_edits(&moved, &shown, false), Err(Misaligned));

    let shifted = [found(1, 1, 0..3, "foo"), found(2, 6, 13..16, "foo")];
    assert_eq!(plan_edits(&shifted, &shown, false), Err(Misaligned));

    let fewer = [found(1, 1, 0..3, "foo")];
    assert_eq!(plan_edits(&fewer, &shown, false), Err(Misaligned));

    let more = [
        found(1, 1, 0..3, "foo"),
        found(2, 5, 12..15, "foo"),
        found(3, 1, 16..19, "foo"),
    ];
    assert_eq!(plan_edits(&more, &shown, false), Err(Misaligned));

    let other_text = [found(1, 1, 0..3, "foo"), found(2, 5, 12..15, "Foo")];
    assert_eq!(plan_edits(&other_text, &shown, false), Err(Misaligned));

    assert_eq!(plan_edits(&[], &shown, false), Err(Misaligned));
}

#[test]
fn a_truncated_search_may_have_missed_later_matches() {
    let shown = FileExpectation {
        matches: vec![expected(1, 1, "foo", true)],
        may_be_truncated: true,
    };
    let more = [found(1, 1, 0..3, "foo"), found(2, 5, 12..15, "foo")];
    let edits = plan_edits(&more, &shown, false).unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].range, 0..3);

    assert_eq!(plan_edits(&[], &shown, false), Err(Misaligned));
    let different = [found(1, 2, 1..4, "foo")];
    assert_eq!(plan_edits(&different, &shown, false), Err(Misaligned));
}

#[test]
fn overlapping_matches_are_never_planned() {
    let overlapping = [found(1, 1, 0..3, "foo"), found(1, 3, 2..5, "foo")];
    let shown = expectation(vec![
        expected(1, 1, "foo", true),
        expected(1, 3, "foo", true),
    ]);
    assert_eq!(plan_edits(&overlapping, &shown, false), Err(Misaligned));
}

#[test]
fn line_endings_are_ignored_only_when_asked() {
    let in_editor = [found(1, 1, 0..7, "foo\nbar")];
    let shown = expectation(vec![expected(1, 1, "foo\r\nbar", true)]);
    assert_eq!(plan_edits(&in_editor, &shown, false), Err(Misaligned));
    assert_eq!(plan_edits(&in_editor, &shown, true).unwrap().len(), 1);

    let trailing = [found(1, 1, 0..3, "foo")];
    let shown = expectation(vec![expected(1, 1, "foo\r", true)]);
    assert_eq!(plan_edits(&trailing, &shown, true).unwrap().len(), 1);
}

#[test]
fn edits_are_applied_in_order() {
    let text = "foo bar foo\nbaz foo\n";
    let edits = [
        TextEdit {
            range: 0..3,
            replacement: "quux".to_string(),
        },
        TextEdit {
            range: 8..11,
            replacement: String::new(),
        },
        TextEdit {
            range: 16..19,
            replacement: "é".to_string(),
        },
    ];
    assert_eq!(apply_edits(text, &edits), "quux bar \nbaz é\n");
    assert_eq!(apply_edits(text, &[]), text);
}

#[test]
fn an_empty_match_is_an_insertion() {
    let edits = [TextEdit {
        range: 3..3,
        replacement: "!".to_string(),
    }];
    assert_eq!(apply_edits("foo bar", &edits), "foo! bar");
}

fn streaming_plan(
    text: &str,
    replacement: &str,
    shown: &FileExpectation,
    limit: u64,
) -> Result<Vec<TextEdit>, SkipReason> {
    let finder = MatchFinder::new(
        &crate::test_support::literal("a"),
        Some(&crate::replacement::ReplaceSpec {
            replacement: replacement.to_owned(),
            preserve_case: false,
        }),
    )
    .unwrap();
    plan_selected_edits(&finder, text, shown, false, limit, &AtomicBool::new(false))
}

#[test]
fn a_dense_eight_megabyte_file_expands_only_its_selected_prefix() {
    let text = "a\n".repeat(4 * 1024 * 1024);
    let shown = FileExpectation {
        matches: vec![expected(1, 1, "a", true)],
        may_be_truncated: true,
    };
    let replacement = "x".repeat(1024);
    let edits = streaming_plan(
        &text,
        &replacement,
        &shown,
        crate::apply::DEFAULT_MAX_FILE_BYTES,
    )
    .unwrap();
    assert_eq!(
        edits,
        vec![TextEdit {
            range: 0..1,
            replacement
        }]
    );
}

#[test]
fn unselected_occurrences_do_not_expand_an_oversized_replacement() {
    let shown = expectation(vec![expected(1, 1, "a", false), expected(2, 1, "a", false)]);
    let edits = streaming_plan("a\na\n", &"x".repeat(4096), &shown, 128).unwrap();
    assert!(edits.is_empty());
}

#[test]
fn capture_expansion_is_bounded_before_copying_captures() {
    let text = "a".repeat(4096);
    let finder = MatchFinder::new(
        &crate::test_support::regex("(.+)"),
        Some(&crate::replacement::ReplaceSpec {
            replacement: "$1".repeat(1024),
            preserve_case: false,
        }),
    )
    .unwrap();
    let shown = expectation(vec![expected(1, 1, &text, true)]);
    let error = plan_selected_edits(&finder, &text, &shown, false, 8192, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error, SkipReason::TooLarge { .. }), "{error:?}");
}

#[test]
fn deletion_edit_metadata_is_also_bounded() {
    let text = "a\n".repeat(1000);
    let shown = expectation(
        (1..=1000)
            .map(|line| expected(line, 1, "a", true))
            .collect(),
    );
    let error = streaming_plan(&text, "", &shown, 16 * 1024).unwrap_err();
    assert!(matches!(error, SkipReason::TooLarge { .. }), "{error:?}");
}

#[test]
fn a_truncated_selection_stops_before_an_unbounded_trailing_line() {
    let text = format!(
        "a\n{}\n",
        "x".repeat(128 * 1024) // Larger than the searcher's 64 KiB line buffer.
    );
    let shown = FileExpectation {
        matches: vec![expected(1, 1, "a", true)],
        may_be_truncated: true,
    };
    let edits = streaming_plan(&text, "b", &shown, crate::apply::DEFAULT_MAX_FILE_BYTES).unwrap();
    assert_eq!(edits.len(), 1);
}
