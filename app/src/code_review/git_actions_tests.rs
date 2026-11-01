use std::path::{Path, PathBuf};

use command::Stdio;
use command::r#async::Command;
use tempfile::TempDir;

use super::{SyncOutcome, SyncPlan, SyncRefusal, run_sync, sync_plan};
use crate::util::git::TrackingCounts;

async fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("failed to run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

async fn configure_identity(repo: &Path) {
    git(repo, &["config", "user.email", "test@test.com"]).await;
    git(repo, &["config", "user.name", "Test"]).await;
    git(repo, &["config", "commit.gpgsign", "false"]).await;
}

async fn commit(repo: &Path, file: &str) {
    std::fs::write(repo.join(file), "content\n").unwrap();
    git(repo, &["add", "-A"]).await;
    git(repo, &["commit", "-m", file]).await;
}

/// Creates a bare origin and two clones of it that both have its first commit. Returns the temp
/// dir that owns them, then the clone under test and the one that stands in for a collaborator.
async fn init_clones() -> (TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let root = dir.path().canonicalize().expect("canonical temp dir");
    let origin = root.join("origin.git");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--bare", "-b", "main"]).await;

    let repo = root.join("repo");
    git(&root, &["clone", origin.to_str().unwrap(), "repo"]).await;
    configure_identity(&repo).await;
    git(&repo, &["checkout", "-b", "main"]).await;
    commit(&repo, "initial.txt").await;
    git(&repo, &["push", "-u", "origin", "main"]).await;

    let other = root.join("other");
    git(&root, &["clone", origin.to_str().unwrap(), "other"]).await;
    configure_identity(&other).await;
    (dir, repo, other)
}

async fn commit_and_push(repo: &Path, file: &str) {
    commit(repo, file).await;
    git(repo, &["push"]).await;
}

fn counts(ahead: usize, behind: usize) -> Option<TrackingCounts> {
    Some(TrackingCounts { ahead, behind })
}

#[test]
fn sync_plan_covers_every_tracking_state() {
    assert_eq!(sync_plan(None, false), SyncPlan::NoUpstream);
    assert_eq!(sync_plan(counts(0, 0), false), SyncPlan::UpToDate);
    assert_eq!(sync_plan(counts(2, 0), false), SyncPlan::Push { ahead: 2 });
    assert_eq!(sync_plan(counts(0, 3), false), SyncPlan::Pull { behind: 3 });
    assert_eq!(
        sync_plan(counts(2, 3), false),
        SyncPlan::Diverged {
            ahead: 2,
            behind: 3
        }
    );
}

#[test]
fn sync_plan_is_blocked_by_an_operation_in_progress_whatever_the_counts() {
    for tracking in [None, counts(0, 0), counts(1, 0), counts(0, 1), counts(1, 1)] {
        assert_eq!(sync_plan(tracking, true), SyncPlan::Blocked);
    }
}

#[tokio::test]
async fn sync_fast_forwards_a_branch_that_is_behind() {
    let (_dir, repo, other) = init_clones().await;
    commit_and_push(&other, "remote1.txt").await;
    commit_and_push(&other, "remote2.txt").await;

    let outcome = run_sync(&repo, "main", None).await.unwrap();

    assert_eq!(
        outcome,
        SyncOutcome::Completed {
            pulled: 2,
            pushed: 0
        }
    );
    assert!(repo.join("remote2.txt").exists());
}

#[tokio::test]
async fn sync_pushes_a_branch_that_is_ahead() {
    let (_dir, repo, other) = init_clones().await;
    commit(&repo, "local.txt").await;

    let outcome = run_sync(&repo, "main", None).await.unwrap();

    assert_eq!(
        outcome,
        SyncOutcome::Completed {
            pulled: 0,
            pushed: 1
        }
    );
    git(&other, &["pull"]).await;
    assert!(other.join("local.txt").exists());
}

#[tokio::test]
async fn sync_leaves_a_diverged_branch_untouched() {
    let (_dir, repo, other) = init_clones().await;
    commit_and_push(&other, "remote.txt").await;
    commit(&repo, "local.txt").await;
    let head_before = git(&repo, &["rev-parse", "HEAD"]).await;

    let outcome = run_sync(&repo, "main", None).await.unwrap();

    assert_eq!(
        outcome,
        SyncOutcome::Refused(SyncRefusal::Diverged {
            ahead: 1,
            behind: 1
        })
    );
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]).await, head_before);
    assert!(!repo.join("remote.txt").exists());
    assert_eq!(git(&repo, &["status", "--porcelain"]).await, "");
}

#[tokio::test]
async fn sync_refuses_a_branch_without_an_upstream() {
    let (_dir, repo, _other) = init_clones().await;
    git(&repo, &["checkout", "-b", "unpublished"]).await;
    commit(&repo, "local.txt").await;

    let outcome = run_sync(&repo, "unpublished", None).await.unwrap();

    assert_eq!(outcome, SyncOutcome::Refused(SyncRefusal::NoUpstream));
    assert_eq!(
        git(&repo, &["ls-remote", "--heads", "origin", "unpublished"]).await,
        "",
        "syncing must not publish the branch"
    );
}

#[tokio::test]
async fn sync_refuses_while_a_merge_is_in_progress() {
    let (_dir, repo, _other) = init_clones().await;
    std::fs::write(repo.join(".git/MERGE_HEAD"), "0000\n").unwrap();

    let outcome = run_sync(&repo, "main", None).await.unwrap();

    assert_eq!(
        outcome,
        SyncOutcome::Refused(SyncRefusal::OperationInProgress)
    );
}

#[tokio::test]
async fn sync_reports_an_up_to_date_branch() {
    let (_dir, repo, _other) = init_clones().await;

    let outcome = run_sync(&repo, "main", None).await.unwrap();

    assert_eq!(
        outcome,
        SyncOutcome::Completed {
            pulled: 0,
            pushed: 0
        }
    );
}

#[tokio::test]
async fn sync_does_not_publish_a_branch_tracking_a_local_branch() {
    let (_dir, repo, _other) = init_clones().await;
    git(
        &repo,
        &["checkout", "-b", "local-tracking", "--track", "main"],
    )
    .await;
    commit(&repo, "local.txt").await;

    assert_eq!(
        run_sync(&repo, "local-tracking", None).await.unwrap(),
        SyncOutcome::Refused(SyncRefusal::NoUpstream),
        "a local upstream must not turn Sync into Publish"
    );
    assert_eq!(
        git(&repo, &["ls-remote", "--heads", "origin", "local-tracking"]).await,
        ""
    );
}

#[tokio::test]
async fn sync_refuses_a_stale_branch_instead_of_acting_on_the_new_checkout() {
    let (_dir, repo, _other) = init_clones().await;
    git(&repo, &["checkout", "-b", "new-checkout"]).await;
    commit(&repo, "local.txt").await;
    let head_before = git(&repo, &["rev-parse", "HEAD"]).await;

    let error = run_sync(&repo, "main", None).await.unwrap_err();

    assert!(error.to_string().contains("checked-out branch changed"));
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]).await, head_before);
    assert_eq!(
        git(&repo, &["ls-remote", "--heads", "origin", "new-checkout"]).await,
        ""
    );
}

#[cfg(unix)]
#[tokio::test]
async fn sync_refuses_when_the_checkout_changes_during_fetch() {
    use std::os::unix::fs::PermissionsExt;

    let (dir, repo, _other) = init_clones().await;
    git(&repo, &["branch", "new-checkout"]).await;
    let hook = dir.path().join("upload-pack");
    let quoted_repo = repo.display().to_string().replace('\'', "'\"'\"'");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\ngit -C '{quoted_repo}' checkout new-checkout >&2 || exit 1\nexec git-upload-pack \"$@\"\n"),
    ).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    git(
        &repo,
        &["config", "remote.origin.uploadpack", hook.to_str().unwrap()],
    )
    .await;

    let error = run_sync(&repo, "main", None).await.unwrap_err();

    assert!(error.to_string().contains("branch changed during fetch"));
    assert_eq!(
        git(&repo, &["branch", "--show-current"]).await,
        "new-checkout"
    );
    assert_eq!(
        git(&repo, &["ls-remote", "--heads", "origin", "new-checkout"]).await,
        ""
    );
}
