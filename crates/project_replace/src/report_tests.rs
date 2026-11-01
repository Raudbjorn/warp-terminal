use super::*;

fn counts(occurrences: usize, files: usize) -> ReplaceCounts {
    ReplaceCounts { occurrences, files }
}

fn skipped(count: usize) -> Vec<SkippedFile> {
    (0..count)
        .map(|index| SkippedFile {
            path: PathBuf::from(format!("/project/{index}.txt")),
            reason: SkipReason::ChangedSinceSearch,
        })
        .collect()
}

#[test]
fn the_replace_all_label_states_both_counts() {
    assert_eq!(
        replace_all_label(counts(37, 9)),
        "Replace 37 occurrences across 9 files"
    );
    assert_eq!(
        replace_all_label(counts(1, 1)),
        "Replace 1 occurrence across 1 file"
    );
    assert_eq!(
        replace_all_label(counts(2, 1)),
        "Replace 2 occurrences across 1 file"
    );
    assert!(counts(0, 0).is_empty());
    assert!(!counts(1, 1).is_empty());
}

#[test]
fn the_confirmation_states_the_counts_and_both_texts() {
    assert_eq!(
        confirmation_title(counts(37, 9), "foo", "bar"),
        "Replace 37 occurrences across 9 files of “foo” with “bar”?"
    );
    assert_eq!(
        confirmation_title(counts(1, 1), "a\nb", "x"),
        "Replace 1 occurrence across 1 file of “a⏎b” with “x”?"
    );

    let long = "x".repeat(100);
    let title = confirmation_title(counts(2, 2), &long, "y");
    assert!(title.contains(&format!("“{}…”", "x".repeat(40))));
}

#[test]
fn the_confirmation_of_an_empty_replacement_says_the_matches_are_deleted() {
    assert_eq!(
        confirmation_title(counts(37, 9), "foo", ""),
        "Delete 37 occurrences across 9 files of “foo”?"
    );
}

#[test]
fn the_summary_says_what_was_replaced_skipped_and_left() {
    assert_eq!(
        replace_summary(counts(37, 9), &[], None),
        "Replaced 37 occurrences in 9 files"
    );
    assert_eq!(
        replace_summary(counts(1, 1), &skipped(1), None),
        "Replaced 1 occurrence in 1 file. Skipped 1 file"
    );
    assert_eq!(
        replace_summary(counts(4, 2), &skipped(3), Some(5)),
        "Replaced 4 occurrences in 2 files. Stopped with 5 files left untouched. Skipped 3 files"
    );
    assert_eq!(
        replace_summary(counts(0, 0), &skipped(2), None),
        "Nothing was replaced. Skipped 2 files"
    );
}

#[test]
fn the_undo_summary_says_what_was_restored_and_what_was_not() {
    let refused = |count: usize| -> Vec<RefusedFile> {
        (0..count)
            .map(|index| RefusedFile {
                path: PathBuf::from(format!("/project/{index}.txt")),
                reason: RefusalReason::EditedSinceReplace,
            })
            .collect()
    };
    assert_eq!(undo_summary(9, &[], None), "Restored 9 files");
    assert_eq!(
        undo_summary(1, &refused(1), None),
        "Restored 1 file. Left 1 file as it is"
    );
    assert_eq!(
        undo_summary(0, &refused(2), None),
        "Nothing was restored. Left 2 files as they are"
    );
    assert_eq!(
        undo_summary(3, &refused(1), Some(4)),
        "Restored 3 files. Stopped with 4 files left to restore. Left 1 file as it is"
    );
}

#[test]
fn a_change_is_named_by_both_texts() {
    assert_eq!(describe_change("foo", "bar"), "“foo” → “bar”");
    assert_eq!(describe_change("a\nb", ""), "“a⏎b” → “”");
}

#[test]
fn every_reason_has_a_description() {
    assert_eq!(
        SkipReason::TooLarge {
            bytes: 17 * 1024 * 1024 + 1,
            limit: 16 * 1024 * 1024
        }
        .describe(),
        "too large (18 MB, the limit is 16 MB)"
    );
    assert_eq!(
        SkipReason::WriteFailed("disk full".to_string()).describe(),
        "could not be written: disk full"
    );
    assert_eq!(
        RefusalReason::EditedSinceReplace.describe(),
        "edited since the replace"
    );
}
