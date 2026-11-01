use super::*;
use crate::code_review::diff_state::GitFileStatus;
use crate::code_review::nested_repos::NestedRepoFile;

fn status(upstream: Option<&str>, ahead: usize, behind: usize, changed: usize) -> NestedRepoStatus {
    NestedRepoStatus {
        branch: "main".to_owned(),
        is_detached: false,
        head_oid: Some("1a2b3c4".to_owned()),
        upstream: upstream.map(str::to_owned),
        ahead,
        behind,
        files: (0..changed)
            .map(|index| NestedRepoFile {
                path: format!("file{index}.txt"),
                status: GitFileStatus::Modified,
            })
            .collect(),
    }
}

#[test]
fn clean_repo_level_with_upstream_offers_sync_pull_request_and_fetch() {
    assert_eq!(
        applicable_actions(&status(Some("origin/main"), 0, 0, 0)),
        [RepoAction::Sync, RepoAction::PullRequest, RepoAction::Fetch]
    );
}

#[test]
fn changes_add_commit_and_discard() {
    assert_eq!(
        applicable_actions(&status(Some("origin/main"), 0, 0, 3)),
        [
            RepoAction::Commit,
            RepoAction::Sync,
            RepoAction::Discard,
            RepoAction::PullRequest,
            RepoAction::Fetch
        ]
    );
}

#[test]
fn unpushed_commits_add_push() {
    assert_eq!(
        applicable_actions(&status(Some("origin/main"), 2, 0, 0)),
        [
            RepoAction::Push,
            RepoAction::Sync,
            RepoAction::PullRequest,
            RepoAction::Fetch
        ]
    );
}

#[test]
fn branch_without_upstream_offers_publish_instead_of_push() {
    let actions = applicable_actions(&status(None, 0, 0, 1));

    assert_eq!(
        actions,
        [
            RepoAction::Commit,
            RepoAction::Publish,
            RepoAction::Sync,
            RepoAction::Discard,
            RepoAction::Fetch
        ]
    );
}

#[test]
fn detached_head_offers_no_branch_actions() {
    let mut detached = status(None, 0, 0, 1);
    detached.is_detached = true;

    assert_eq!(
        applicable_actions(&detached),
        [RepoAction::Commit, RepoAction::Discard, RepoAction::Fetch]
    );
}

#[test]
fn repo_without_commits_offers_only_commit_and_discard() {
    let mut unborn = status(None, 0, 0, 2);
    unborn.head_oid = None;

    assert_eq!(
        applicable_actions(&unborn),
        [RepoAction::Commit, RepoAction::Discard]
    );
    unborn.files.clear();
    assert!(applicable_actions(&unborn).is_empty());
}

#[test]
fn wide_and_medium_rows_show_every_button_and_keep_the_rest_in_the_menu() {
    let actions = applicable_actions(&status(Some("origin/main"), 1, 0, 3));

    let wide = layout_actions(&actions, RowWidth::Wide);
    assert_eq!(
        wide.buttons,
        [
            RepoAction::Commit,
            RepoAction::Push,
            RepoAction::Sync,
            RepoAction::Discard
        ]
    );
    assert_eq!(wide.overflow, [RepoAction::PullRequest, RepoAction::Fetch]);
    assert!(wide.shows_labels);

    let medium = layout_actions(&actions, RowWidth::Medium);
    assert_eq!(medium.buttons, wide.buttons);
    assert_eq!(medium.overflow, wide.overflow);
    assert!(!medium.shows_labels);
}

#[test]
fn narrow_rows_keep_one_button_and_move_the_rest_into_the_menu() {
    let actions = applicable_actions(&status(Some("origin/main"), 1, 0, 3));

    let narrow = layout_actions(&actions, RowWidth::Narrow);

    assert_eq!(narrow.buttons, [RepoAction::Commit]);
    assert_eq!(
        narrow.overflow,
        [
            RepoAction::Push,
            RepoAction::Sync,
            RepoAction::Discard,
            RepoAction::PullRequest,
            RepoAction::Fetch
        ]
    );
    assert!(!narrow.shows_labels);
}

#[test]
fn every_action_stays_reachable_at_every_width() {
    let actions = applicable_actions(&status(None, 0, 0, 2));

    for width in [RowWidth::Wide, RowWidth::Medium, RowWidth::Narrow] {
        let layout = layout_actions(&actions, width);
        let mut reachable = layout.buttons.clone();
        reachable.extend(layout.overflow);
        reachable.sort_by_key(|action| actions.iter().position(|a| a == action));
        assert_eq!(reachable, actions);
    }
}

#[test]
fn sync_messages_describe_each_outcome() {
    let upstream = Some("origin/main");

    assert_eq!(
        sync_message(
            SyncOutcome::Completed {
                pulled: 0,
                pushed: 0
            },
            upstream
        ),
        RowMessage::info("Already up to date with origin/main")
    );
    assert_eq!(
        sync_message(
            SyncOutcome::Completed {
                pulled: 2,
                pushed: 0
            },
            upstream
        ),
        RowMessage::info("Pulled 2 commits from origin/main")
    );
    assert_eq!(
        sync_message(
            SyncOutcome::Completed {
                pulled: 0,
                pushed: 1
            },
            upstream
        ),
        RowMessage::info("Pushed 1 commit to origin/main")
    );

    let diverged = sync_message(
        SyncOutcome::Refused(SyncRefusal::Diverged {
            ahead: 1,
            behind: 3,
        }),
        upstream,
    );
    assert!(diverged.is_error);
    assert!(diverged.text.contains("1 ahead, 3 behind"));

    assert!(sync_message(SyncOutcome::Refused(SyncRefusal::NoUpstream), None).is_error);
    assert!(sync_message(SyncOutcome::Refused(SyncRefusal::OperationInProgress), None).is_error);
}
