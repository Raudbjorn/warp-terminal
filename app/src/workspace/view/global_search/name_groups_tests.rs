use warp_util::local_or_remote_path::LocalOrRemotePath;

use super::*;

fn name_match(display_path: &str, highlight_indices: Vec<usize>) -> NameMatch {
    NameMatch {
        location: LocalOrRemotePath::Local(display_path.into()),
        display_path: display_path.to_string(),
        highlight_indices,
        score: 0,
    }
}

/// A group holding `kept` matches out of `total` that matched.
fn group(kept: usize, total: usize) -> NameGroup {
    NameGroup::new(
        NameMatchKind::File,
        RankedNameMatches {
            matches: (0..kept)
                .map(|index| name_match(&format!("file_{index}.rs"), Vec::new()))
                .collect(),
            total,
        },
    )
}

fn rows(group: &NameGroup) -> Vec<NameGroupRow> {
    (0..group.visible_count())
        .map(|offset| group.row_at(offset).expect("offset is within the group"))
        .collect()
}

#[test]
fn empty_group_has_no_rows() {
    let group = NameGroup::empty(NameMatchKind::Folder);

    assert!(group.is_empty());
    assert_eq!(group.visible_count(), 0);
    assert_eq!(group.row_at(0), None);
    assert_eq!(group.offset_of(NameGroupRow::Header), None);
}

#[test]
fn small_group_lists_every_match_under_its_header() {
    let group = group(2, 2);

    assert_eq!(
        rows(&group),
        vec![
            NameGroupRow::Header,
            NameGroupRow::Match(0),
            NameGroupRow::Match(1),
        ]
    );
    assert_eq!(group.hidden_count(), 0);
    assert_eq!(group.row_at(3), None);
    assert_eq!(group.offset_of(NameGroupRow::HiddenMatches), None);
}

#[test]
fn large_group_is_capped_to_one_page() {
    let group = group(120, 120);

    assert_eq!(group.visible_count(), 1 + NAME_GROUP_PAGE_SIZE + 1);
    assert_eq!(group.row_at(1), Some(NameGroupRow::Match(0)));
    assert_eq!(
        group.row_at(NAME_GROUP_PAGE_SIZE),
        Some(NameGroupRow::Match(NAME_GROUP_PAGE_SIZE - 1))
    );
    assert_eq!(
        group.row_at(NAME_GROUP_PAGE_SIZE + 1),
        Some(NameGroupRow::HiddenMatches)
    );
    assert_eq!(group.row_at(NAME_GROUP_PAGE_SIZE + 2), None);
    assert_eq!(group.hidden_count(), 70);
    assert!(group.entry(NAME_GROUP_PAGE_SIZE - 1).is_some());
    assert!(group.entry(NAME_GROUP_PAGE_SIZE).is_none());
}

#[test]
fn showing_more_reveals_one_page_at_a_time() {
    let mut group = group(120, 120);

    assert_eq!(group.show_more(), Some(50));
    assert_eq!(group.hidden_count(), 20);
    assert_eq!(group.row_at(101), Some(NameGroupRow::HiddenMatches));

    assert_eq!(group.show_more(), Some(100));
    assert_eq!(group.hidden_count(), 0);
    assert_eq!(group.visible_count(), 121);
    assert_eq!(group.row_at(120), Some(NameGroupRow::Match(119)));

    assert_eq!(group.show_more(), None);
}

#[test]
fn matches_dropped_before_the_group_stay_counted_as_hidden() {
    let mut group = group(60, 700);
    assert_eq!(group.total(), 700);
    assert_eq!(group.hidden_count(), 650);
    assert!(group.can_show_more());

    assert_eq!(group.show_more(), Some(50));
    assert!(!group.can_show_more());
    assert_eq!(group.hidden_count(), 640);
    // The remaining matches cannot be revealed, but the row reporting them stays.
    assert_eq!(group.row_at(61), Some(NameGroupRow::HiddenMatches));
    assert_eq!(group.show_more(), None);
}

#[test]
fn collapsed_group_shows_only_its_header() {
    let mut group = group(3, 3);
    group.toggle_collapsed();

    assert_eq!(rows(&group), vec![NameGroupRow::Header]);
    assert_eq!(group.offset_of(NameGroupRow::Header), Some(0));
    assert_eq!(group.offset_of(NameGroupRow::Match(0)), None);

    group.toggle_collapsed();
    assert_eq!(group.offset_of(NameGroupRow::Match(0)), Some(1));
}

#[test]
fn group_offsets_round_trip() {
    let group = group(120, 300);

    for offset in 0..group.visible_count() {
        let row = group.row_at(offset).expect("offset is within the group");
        assert_eq!(group.offset_of(row), Some(offset));
    }
    assert_eq!(
        group.offset_of(NameGroupRow::Match(NAME_GROUP_PAGE_SIZE)),
        None
    );
}

#[test]
fn sections_are_stacked_as_folders_files_then_text_matches() {
    let counts = SectionCounts {
        folders: 2,
        files: 3,
        text_matches: Some(2),
    };

    let rows: Vec<_> = (0..counts.total())
        .map(|index| counts.row_at(index).expect("index is within the list"))
        .collect();
    assert_eq!(
        rows,
        vec![
            SectionRow::Folders(0),
            SectionRow::Folders(1),
            SectionRow::Files(0),
            SectionRow::Files(1),
            SectionRow::Files(2),
            SectionRow::TextMatchesHeader,
            SectionRow::TextMatch(0),
            SectionRow::TextMatch(1),
        ]
    );
    assert_eq!(counts.row_at(counts.total()), None);

    for (index, row) in rows.into_iter().enumerate() {
        assert_eq!(counts.index_of(row), Some(index));
    }
}

#[test]
fn text_matches_alone_start_with_their_header() {
    let counts = SectionCounts {
        folders: 0,
        files: 0,
        text_matches: Some(4),
    };

    assert_eq!(counts.total(), 5);
    assert_eq!(counts.row_at(0), Some(SectionRow::TextMatchesHeader));
    assert_eq!(counts.row_at(1), Some(SectionRow::TextMatch(0)));
    assert_eq!(counts.index_of(SectionRow::Folders(0)), None);
}

#[test]
fn text_matches_header_is_left_out_without_text_matches() {
    let counts = SectionCounts {
        folders: 1,
        files: 2,
        text_matches: None,
    };

    assert_eq!(counts.total(), 3);
    assert_eq!(counts.row_at(3), None);
    assert_eq!(counts.index_of(SectionRow::TextMatchesHeader), None);
}

#[test]
fn collapsed_text_matches_keep_only_their_header() {
    let counts = SectionCounts {
        folders: 1,
        files: 0,
        text_matches: Some(0),
    };

    assert_eq!(counts.total(), 2);
    assert_eq!(counts.row_at(1), Some(SectionRow::TextMatchesHeader));
    assert_eq!(counts.index_of(SectionRow::TextMatch(0)), None);
}

#[test]
fn label_splits_highlights_between_name_and_parent() {
    // Highlights "src" in the parent, the separator before the name, and "view" in the name.
    let name_match = name_match("app/src/view.rs", vec![4, 5, 6, 7, 8, 9, 10, 11]);

    assert_eq!(
        NameMatchLabel::new(&name_match),
        NameMatchLabel {
            name: "view.rs",
            name_highlights: vec![0, 1, 2, 3],
            parent: "app/src",
            parent_highlights: vec![4, 5, 6],
        }
    );
}

#[test]
fn label_of_an_entry_at_the_root_has_no_parent() {
    let name_match = name_match("README.md", vec![0, 1]);

    assert_eq!(
        NameMatchLabel::new(&name_match),
        NameMatchLabel {
            name: "README.md",
            name_highlights: vec![0, 1],
            parent: "",
            parent_highlights: Vec::new(),
        }
    );
}

#[test]
fn label_highlights_are_char_indices() {
    let name_match = name_match("dönér/view.rs", vec![1, 6, 7]);
    let label = NameMatchLabel::new(&name_match);

    assert_eq!(label.parent, "dönér");
    assert_eq!(label.parent_highlights, vec![1]);
    assert_eq!(label.name_highlights, vec![0, 1]);
}

#[test]
fn summary_counts_folders_and_files() {
    assert_eq!(name_match_summary(0, 0), None);
    assert_eq!(
        name_match_summary(1, 0).as_deref(),
        Some("1 folder by name")
    );
    assert_eq!(name_match_summary(0, 3).as_deref(), Some("3 files by name"));
    assert_eq!(
        name_match_summary(2, 1).as_deref(),
        Some("2 folders and 1 file by name")
    );
}
