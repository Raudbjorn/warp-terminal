use super::*;

const A: &str = "/project/a.txt";
const B: &str = "/project/b.txt";

/// Adds the rows of two files: three matches in `A`, one in `B`.
fn add_rows(builder: &mut TargetsBuilder) {
    builder.add_row(Path::new(A), 1, 5, "let foo = foo;", 4..7);
    builder.add_row(Path::new(A), 1, 11, "let foo = foo;", 10..13);
    builder.add_row(Path::new(A), 7, 1, "foo()", 0..3);
    builder.add_row(Path::new(B), 3, 2, " Foo", 1..4);
}

fn targets(dismissals: &Dismissals, selection: Selection) -> Vec<FileTarget> {
    let mut builder = TargetsBuilder::new(dismissals, selection);
    add_rows(&mut builder);
    builder.build()
}

fn replaced(target: &FileTarget) -> Vec<(u32, usize)> {
    target
        .expectation
        .matches
        .iter()
        .filter(|expected| expected.replace)
        .map(|expected| (expected.line, expected.column))
        .collect()
}

#[test]
fn everything_is_replaced_by_default() {
    let targets = targets(&Dismissals::default(), Selection::All);
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0].path, Path::new(A));
    assert_eq!(replaced(&targets[0]), [(1, 5), (1, 11), (7, 1)]);
    assert_eq!(targets[1].expectation.matches[0].matched, "Foo");
    assert!(!targets[0].expectation.may_be_truncated);
    assert_eq!(
        count_targets(&targets),
        ReplaceCounts {
            occurrences: 4,
            files: 2
        }
    );
}

#[test]
fn a_dismissed_match_stays_expected_but_is_not_replaced() {
    let mut dismissals = Dismissals::default();
    dismissals.toggle_match(Path::new(A), 1, 11);
    assert!(dismissals.is_match_dismissed(Path::new(A), 1, 11));
    assert!(!dismissals.is_match_dismissed(Path::new(A), 1, 5));
    assert!(!dismissals.is_file_dismissed(Path::new(A)));

    let targets = targets(&dismissals, Selection::All);
    assert_eq!(targets[0].expectation.matches.len(), 3);
    assert_eq!(replaced(&targets[0]), [(1, 5), (7, 1)]);
    assert_eq!(count_targets(&targets).occurrences, 3);
}

#[test]
fn a_dismissed_file_is_left_out() {
    let mut dismissals = Dismissals::default();
    dismissals.toggle_file(Path::new(A));
    assert!(dismissals.is_file_dismissed(Path::new(A)));
    assert!(dismissals.is_match_dismissed(Path::new(A), 7, 1));

    let targets = targets(&dismissals, Selection::All);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].path, Path::new(B));
    assert_eq!(
        count_targets(&targets),
        ReplaceCounts {
            occurrences: 1,
            files: 1
        }
    );
}

#[test]
fn dismissing_twice_takes_it_back() {
    let mut dismissals = Dismissals::default();
    dismissals.toggle_file(Path::new(A));
    dismissals.toggle_match(Path::new(B), 3, 2);
    assert!(!dismissals.is_empty());

    dismissals.toggle_file(Path::new(A));
    dismissals.toggle_match(Path::new(B), 3, 2);
    assert!(dismissals.is_empty());
    assert_eq!(
        count_targets(&targets(&dismissals, Selection::All)).occurrences,
        4
    );

    dismissals.toggle_file(Path::new(A));
    dismissals.clear();
    assert!(dismissals.is_empty());
}

#[test]
fn every_match_dismissed_leaves_nothing_to_replace() {
    let mut dismissals = Dismissals::default();
    dismissals.toggle_file(Path::new(A));
    dismissals.toggle_match(Path::new(B), 3, 2);
    let targets = targets(&dismissals, Selection::All);
    assert!(targets.is_empty());
    assert!(count_targets(&targets).is_empty());
}

#[test]
fn one_file_can_be_selected() {
    let mut dismissals = Dismissals::default();
    dismissals.toggle_match(Path::new(A), 7, 1);
    let targets = targets(&dismissals, Selection::File(Path::new(A)));
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].path, Path::new(A));
    assert_eq!(replaced(&targets[0]), [(1, 5), (1, 11)]);
}

#[test]
fn one_match_can_be_selected_while_its_file_is_checked_whole() {
    let selection = Selection::Match {
        path: Path::new(A),
        line: 1,
        column: 11,
    };
    let targets = targets(&Dismissals::default(), selection);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].expectation.matches.len(), 3);
    assert_eq!(replaced(&targets[0]), [(1, 11)]);
}

#[test]
fn a_row_listed_twice_counts_once() {
    let dismissals = Dismissals::default();
    let mut builder = TargetsBuilder::new(&dismissals, Selection::All);
    add_rows(&mut builder);
    add_rows(&mut builder);
    let targets = builder.build();
    assert_eq!(
        count_targets(&targets),
        ReplaceCounts {
            occurrences: 4,
            files: 2
        }
    );
}

#[test]
fn a_file_the_search_stopped_in_is_marked() {
    let dismissals = Dismissals::default();
    let mut builder = TargetsBuilder::new(&dismissals, Selection::All);
    add_rows(&mut builder);
    builder.mark_truncated(Path::new(B));
    builder.mark_truncated(Path::new("/project/never-seen.txt"));
    let targets = builder.build();
    assert!(!targets[0].expectation.may_be_truncated);
    assert!(targets[1].expectation.may_be_truncated);
}

#[test]
fn a_match_that_cannot_be_read_back_is_left_out() {
    let dismissals = Dismissals::default();
    let mut builder = TargetsBuilder::new(&dismissals, Selection::All);
    builder.add_row(Path::new(A), 1, 1, "é", 1..2);
    builder.add_row(Path::new(A), 2, 1, "short", 3..40);
    assert!(builder.build().is_empty());
}

#[test]
fn a_changed_file_with_matches_set_aside_is_set_aside_whole() {
    let mut dismissals = Dismissals::default();
    dismissals.toggle_match(Path::new(A), 1, 11);
    dismissals.toggle_match(Path::new(B), 3, 2);

    dismissals.files_changed(&[PathBuf::from(A), PathBuf::from("/project/untouched.txt")]);

    // The match of `A` may be anywhere on its line now, so nothing in `A` is replaced.
    assert!(dismissals.is_file_dismissed(Path::new(A)));
    assert!(dismissals.is_match_dismissed(Path::new(A), 1, 5));
    assert!(dismissals.is_match_dismissed(Path::new(A), 7, 1));
    // Files that did not change keep what was set aside in them, and gain nothing.
    assert!(!dismissals.is_file_dismissed(Path::new(B)));
    assert!(dismissals.is_match_dismissed(Path::new(B), 3, 2));
    assert!(!dismissals.is_file_dismissed(Path::new("/project/untouched.txt")));

    let targets = targets(&dismissals, Selection::All);
    assert!(targets.is_empty());
}

#[test]
fn a_changed_file_with_nothing_set_aside_stays_included() {
    let mut dismissals = Dismissals::default();
    dismissals.files_changed(&[PathBuf::from(A)]);
    assert!(dismissals.is_empty());
}
