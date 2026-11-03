use super::*;

fn file(path: &str, status: GitFileStatus) -> NestedRepoFile {
    NestedRepoFile {
        path: path.to_owned(),
        status,
    }
}

#[test]
fn files_new_to_the_repository_have_no_committed_base() {
    assert_eq!(base_path_at_head(&file("a.rs", GitFileStatus::New)), None);
    assert_eq!(
        base_path_at_head(&file("a.rs", GitFileStatus::Untracked)),
        None
    );
}

#[test]
fn edited_and_removed_files_compare_with_their_own_path() {
    for status in [
        GitFileStatus::Modified,
        GitFileStatus::Deleted,
        GitFileStatus::Conflicted,
    ] {
        assert_eq!(
            base_path_at_head(&file("src/a.rs", status)),
            Some("src/a.rs")
        );
    }
}

#[test]
fn renamed_and_copied_files_compare_with_the_path_they_came_from() {
    let renamed = file(
        "src/new.rs",
        GitFileStatus::Renamed {
            old_path: "src/old.rs".to_owned(),
        },
    );
    let copied = file(
        "src/copy.rs",
        GitFileStatus::Copied {
            old_path: "src/original.rs".to_owned(),
        },
    );

    assert_eq!(base_path_at_head(&renamed), Some("src/old.rs"));
    assert_eq!(base_path_at_head(&copied), Some("src/original.rs"));
}

#[test]
fn only_untracked_entries_ending_in_a_slash_are_folders() {
    assert!(is_untracked_folder(&file(
        "newdir/",
        GitFileStatus::Untracked
    )));
    assert!(!is_untracked_folder(&file(
        "newdir/a.txt",
        GitFileStatus::Untracked
    )));
    assert!(!is_untracked_folder(&file(
        "newdir/",
        GitFileStatus::Modified
    )));
}
