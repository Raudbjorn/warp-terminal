use std::path::Path;

use command::Stdio;
use command::r#async::Command;
use tempfile::TempDir;

use super::{
    RepositoryInfo, detect_current_branch, detect_current_branch_display, get_pr_for_branch,
    is_gh_auth_error, is_gh_missing_error,
};

/// Helper: run a git command inside the given repo directory.
async fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("failed to run git");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[cfg(feature = "local_fs")]
#[test]
fn repository_info_from_gh_output_parses_name_and_owner() {
    // No url in the output => host is absent.
    assert_eq!(
        super::repository_info_from_gh_output(
            r#"{"name":"warp-internal","owner":{"login":"warpdotdev"}}"#
        )
        .unwrap(),
        RepositoryInfo {
            name: "warp-internal".to_owned(),
            owner: Some("warpdotdev".to_owned()),
            host: None,
        }
    );
}

#[cfg(feature = "local_fs")]
#[test]
fn repository_info_from_gh_output_parses_host_from_url() {
    assert_eq!(
        super::repository_info_from_gh_output(
            r#"{"name":"warp-internal","owner":{"login":"warpdotdev"},"url":"https://github.com/warpdotdev/warp-internal"}"#
        )
        .unwrap(),
        RepositoryInfo {
            name: "warp-internal".to_owned(),
            owner: Some("warpdotdev".to_owned()),
            host: Some("github.com".to_owned()),
        }
    );
}

#[cfg(all(feature = "local_fs", unix))]
#[tokio::test]
async fn get_repository_info_returns_none_when_gh_cannot_resolve_github_repo() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    let (_dir, repo) = init_repo().await;

    let fake_bin = tempfile::tempdir().expect("failed to create fake bin dir");
    let gh_path = fake_bin.path().join("gh");
    fs::write(
        &gh_path,
        "#!/bin/sh\nprintf 'none of the git remotes configured for this repository point to a known GitHub host\\n' >&2\nexit 1\n",
    )
    .expect("failed to write fake gh");
    let mut permissions = fs::metadata(&gh_path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&gh_path, permissions).unwrap();

    let path_env = format!(
        "{}:{}",
        fake_bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    assert_eq!(
        super::get_repository_info(&repo, Some(&path_env))
            .await
            .unwrap(),
        None
    );
}

#[cfg(feature = "local_fs")]
#[test]
fn repository_info_from_gh_output_rejects_missing_name() {
    assert!(super::repository_info_from_gh_output(r#"{"owner":{"login":"warpdotdev"}}"#).is_err());
}

#[cfg(feature = "local_fs")]
#[test]
fn repository_info_from_gh_output_rejects_missing_owner_login() {
    assert!(
        super::repository_info_from_gh_output(r#"{"name":"warp-internal","owner":{}}"#).is_err()
    );
}

#[cfg(feature = "local_fs")]
#[test]
fn repository_info_from_gh_output_rejects_empty_fields() {
    assert!(
        super::repository_info_from_gh_output(r#"{"name":"","owner":{"login":"warpdotdev"}}"#)
            .is_err()
    );
    assert!(
        super::repository_info_from_gh_output(r#"{"name":"warp-internal","owner":{"login":""}}"#)
            .is_err()
    );
}

#[cfg(feature = "local_fs")]
#[test]
fn repository_info_from_gh_output_rejects_malformed_json() {
    assert!(super::repository_info_from_gh_output("not json").is_err());
}

/// Creates a temp git repo with one commit and returns `(dir_handle, repo_path)`.
async fn init_repo() -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let path = dir.path().to_path_buf();

    git(&path, &["init", "-b", "main"]).await;
    git(&path, &["config", "user.email", "test@test.com"]).await;
    git(&path, &["config", "user.name", "Test"]).await;
    git(&path, &["commit", "--allow-empty", "-m", "initial"]).await;

    (dir, path)
}

#[cfg(all(feature = "local_fs", unix))]
#[tokio::test]
async fn get_repository_info_reads_gh_repo_view() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    let (_dir, repo) = init_repo().await;

    let fake_bin = tempfile::tempdir().expect("failed to create fake bin dir");
    let gh_path = fake_bin.path().join("gh");
    fs::write(
        &gh_path,
        "#!/bin/sh\nprintf '{\"name\":\"warp-internal\",\"owner\":{\"login\":\"warpdotdev\"}}\\n'\n",
    )
    .expect("failed to write fake gh");
    let mut permissions = fs::metadata(&gh_path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&gh_path, permissions).unwrap();

    let path_env = format!(
        "{}:{}",
        fake_bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    assert_eq!(
        super::get_repository_info(&repo, Some(&path_env))
            .await
            .unwrap(),
        Some(RepositoryInfo {
            name: "warp-internal".to_owned(),
            owner: Some("warpdotdev".to_owned()),
            host: None,
        })
    );
}

#[test]
fn detects_missing_gh_errors() {
    assert!(is_gh_missing_error(
        "Failed to execute gh command: No such file or directory (os error 2)"
    ));
    assert!(is_gh_missing_error(
        "Failed to execute gh command: program not found"
    ));

    assert!(!is_gh_missing_error(
        "gh command failed: GraphQL: authentication required; run gh auth login"
    ));
    assert!(!is_gh_missing_error(
        "Post \"https://api.github.com/graphql\": dial tcp: lookup api.github.com: no such host"
    ));
}

#[cfg(feature = "local_fs")]
#[test]
fn detects_no_pr_for_branch_errors() {
    assert!(super::is_no_pr_for_branch_error(
        "gh command failed: no pull requests found for branch \"feature-a\""
    ));
    assert!(super::is_no_pr_for_branch_error(
        "gh command failed: no open pull requests found for branch \"feature-a\""
    ));
    assert!(super::is_no_pr_for_branch_error(
        "GraphQL: NO OPEN PULL REQUESTS FOUND FOR BRANCH feature-a"
    ));
    assert!(!super::is_no_pr_for_branch_error("authentication required"));
    assert!(!super::is_no_pr_for_branch_error("repository not found"));
}

#[cfg(feature = "local_fs")]
#[test]
fn detects_repository_lookup_not_applicable_errors() {
    assert!(super::is_repository_lookup_not_applicable_error(
        "gh command failed: none of the git remotes configured for this repository point to a known GitHub host"
    ));
    assert!(super::is_repository_lookup_not_applicable_error(
        "gh command failed: no GitHub remotes"
    ));
    assert!(super::is_repository_lookup_not_applicable_error(
        "gh command failed: not a GitHub repository"
    ));
    assert!(!super::is_repository_lookup_not_applicable_error(
        "authentication required"
    ));
    assert!(!super::is_repository_lookup_not_applicable_error(
        "repository not found"
    ));
}

#[tokio::test]
async fn on_normal_branch_returns_branch_name() {
    let (_dir, repo) = init_repo().await;
    git(&repo, &["checkout", "-b", "feature-xyz"]).await;

    assert_eq!(detect_current_branch(&repo).await.unwrap(), "feature-xyz");
    assert_eq!(
        detect_current_branch_display(&repo).await.unwrap(),
        "feature-xyz"
    );
}

#[tokio::test]
async fn detached_head_raw_returns_head() {
    let (_dir, repo) = init_repo().await;
    git(&repo, &["checkout", "--detach", "HEAD"]).await;

    assert_eq!(detect_current_branch(&repo).await.unwrap(), "HEAD");
}

#[tokio::test]
async fn detached_head_display_returns_short_sha() {
    let (_dir, repo) = init_repo().await;
    let full_sha = git(&repo, &["rev-parse", "HEAD"]).await;
    git(&repo, &["checkout", "--detach", "HEAD"]).await;

    let result = detect_current_branch_display(&repo).await.unwrap();

    assert_ne!(
        result, "HEAD",
        "display variant should not return literal HEAD"
    );
    assert!(
        full_sha.starts_with(&result),
        "expected {full_sha} to start with {result}"
    );
}

#[tokio::test]
async fn get_pr_for_branch_returns_none_for_detached_head() {
    let (_dir, repo) = init_repo().await;
    git(&repo, &["checkout", "--detach", "HEAD"]).await;
    assert_eq!(get_pr_for_branch(&repo, None).await.unwrap(), None);
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn committed_branch_files_excludes_uncommitted_and_untracked() {
    let (_dir, repo) = init_repo().await;
    // Branch off main; the merge base is main's initial commit.
    git(&repo, &["checkout", "-b", "feature"]).await;

    // Commit a new file on the feature branch — this SHOULD appear in the
    // committed branch diff.
    std::fs::write(repo.join("committed.txt"), "line1\nline2\n").expect("write committed.txt");
    git(&repo, &["add", "committed.txt"]).await;
    git(&repo, &["commit", "-m", "add committed.txt"]).await;

    // Further-modify the committed file in the working tree (uncommitted) and
    // add an untracked file. Neither is part of the PR's committed history, so
    // neither should appear, and the committed file's counts must reflect only
    // the committed change (2 added lines, not 3).
    std::fs::write(repo.join("committed.txt"), "line1\nline2\nline3\n")
        .expect("modify committed.txt");
    std::fs::write(repo.join("untracked.txt"), "new\n").expect("write untracked.txt");

    let entries = super::get_committed_branch_file_entries(&repo)
        .await
        .expect("committed branch files");

    assert_eq!(
        entries.len(),
        1,
        "expected only the committed file: {entries:?}"
    );
    assert_eq!(entries[0].path, "committed.txt");
    assert_eq!(entries[0].additions, 2);
    assert_eq!(entries[0].deletions, 0);
    assert!(
        !entries.iter().any(|e| e.path == "untracked.txt"),
        "untracked files must be excluded: {entries:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn get_pr_for_branch_does_not_require_origin_remote() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::PrInfo;

    let (_dir, repo) = init_repo().await;

    let fake_bin = tempfile::tempdir().expect("failed to create fake bin dir");
    let gh_path = fake_bin.path().join("gh");
    fs::write(
        &gh_path,
        "#!/bin/sh\nprintf '{\"number\":123,\"url\":\"https://github.com/warp/warp/pull/123\",\"state\":\"OPEN\",\"isDraft\":true,\"baseRefName\":\"main\"}\\n'\n",
    )
    .expect("failed to write fake gh");
    let mut permissions = fs::metadata(&gh_path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&gh_path, permissions).unwrap();

    let path_env = format!(
        "{}:{}",
        fake_bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    assert_eq!(
        get_pr_for_branch(&repo, Some(&path_env)).await.unwrap(),
        Some(PrInfo {
            number: 123,
            url: "https://github.com/warp/warp/pull/123".to_string(),
            state: "OPEN".to_string(),
            draft: true,
            base_branch: "main".to_string(),
        })
    );
}

#[cfg(unix)]
#[tokio::test]
async fn get_pr_for_branch_returns_none_when_gh_finds_no_pr() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let (_dir, repo) = init_repo().await;

    let fake_bin = tempfile::tempdir().expect("failed to create fake bin dir");
    let gh_path = fake_bin.path().join("gh");
    fs::write(
        &gh_path,
        "#!/bin/sh\nprintf 'no pull requests found for branch \"main\"\\n' >&2\nexit 1\n",
    )
    .expect("failed to write fake gh");
    let mut permissions = fs::metadata(&gh_path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&gh_path, permissions).unwrap();

    let path_env = format!(
        "{}:{}",
        fake_bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    assert_eq!(
        get_pr_for_branch(&repo, Some(&path_env)).await.unwrap(),
        None
    );
}

#[cfg(unix)]
#[tokio::test]
async fn get_pr_for_branch_returns_none_when_gh_cannot_resolve_github_repo() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let (_dir, repo) = init_repo().await;

    let fake_bin = tempfile::tempdir().expect("failed to create fake bin dir");
    let gh_path = fake_bin.path().join("gh");
    fs::write(
        &gh_path,
        "#!/bin/sh\nprintf 'none of the git remotes configured for this repository point to a known GitHub host\\n' >&2\nexit 1\n",
    )
    .expect("failed to write fake gh");
    let mut permissions = fs::metadata(&gh_path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&gh_path, permissions).unwrap();

    let path_env = format!(
        "{}:{}",
        fake_bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    assert_eq!(
        get_pr_for_branch(&repo, Some(&path_env)).await.unwrap(),
        None
    );
}
#[test]
fn detects_gh_auth_errors() {
    assert!(is_gh_auth_error(
        "You are not logged in to any GitHub hosts"
    ));
    assert!(is_gh_auth_error(
        "GraphQL: authentication required; run gh auth login"
    ));
    assert!(is_gh_auth_error(
        "To get started with GitHub CLI, run: gh auth login"
    ));

    assert!(!is_gh_auth_error(
        "Post \"https://api.github.com/graphql\": dial tcp: lookup api.github.com: no such host"
    ));
    assert!(!is_gh_auth_error("no pull requests found for branch"));
}

#[tokio::test]
async fn detached_tag_display_returns_short_sha() {
    let (_dir, repo) = init_repo().await;
    git(&repo, &["tag", "v1.0"]).await;
    git(&repo, &["checkout", "v1.0"]).await;

    let full_sha = git(&repo, &["rev-parse", "HEAD"]).await;
    let result = detect_current_branch_display(&repo).await.unwrap();

    assert_ne!(result, "HEAD");
    assert!(
        full_sha.starts_with(&result),
        "expected {full_sha} to start with {result}"
    );
}

/// Creates a bare repository and a clone of it with one pushed commit on `main`. Returns the
/// temp dir that owns both and the clone's path.
#[cfg(feature = "local_fs")]
async fn init_repo_with_origin() -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let root = dir.path().canonicalize().expect("canonical temp dir");
    let origin = root.join("origin.git");
    let repo = root.join("repo");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--bare", "-b", "main"]).await;
    git(&root, &["clone", origin.to_str().unwrap(), "repo"]).await;
    configure_identity(&repo).await;
    git(&repo, &["checkout", "-b", "main"]).await;
    std::fs::write(repo.join("tracked.txt"), "one\n").unwrap();
    git(&repo, &["add", "-A"]).await;
    git(&repo, &["commit", "-m", "initial"]).await;
    git(&repo, &["push", "-u", "origin", "main"]).await;
    (dir, repo)
}

#[cfg(feature = "local_fs")]
async fn configure_identity(repo: &Path) {
    git(repo, &["config", "user.email", "test@test.com"]).await;
    git(repo, &["config", "user.name", "Test"]).await;
    git(repo, &["config", "commit.gpgsign", "false"]).await;
}

/// Adds a commit to the origin of `repo` from a second clone, so that `repo` falls behind.
#[cfg(feature = "local_fs")]
async fn commit_on_origin(repo: &Path, file: &str) {
    let root = repo.parent().unwrap();
    let other = root.join("other");
    if !other.exists() {
        git(
            root,
            &["clone", root.join("origin.git").to_str().unwrap(), "other"],
        )
        .await;
        configure_identity(&other).await;
    }
    git(&other, &["pull"]).await;
    std::fs::write(other.join(file), "remote\n").unwrap();
    git(&other, &["add", "-A"]).await;
    git(&other, &["commit", "-m", "remote change"]).await;
    git(&other, &["push"]).await;
}

#[cfg(feature = "local_fs")]
async fn commit_locally(repo: &Path, file: &str) {
    std::fs::write(repo.join(file), "local\n").unwrap();
    git(repo, &["add", "-A"]).await;
    git(repo, &["commit", "-m", "local change"]).await;
}

#[cfg(feature = "local_fs")]
#[test]
fn push_goes_to_the_tracked_remote_branch() {
    let destination = super::PushDestination {
        remote: "upstream".to_owned(),
        remote_ref: "refs/heads/release".to_owned(),
    };

    assert_eq!(
        super::push_args("feature", Some(&destination)),
        ["push", "upstream", "refs/heads/feature:refs/heads/release"]
    );
}

#[cfg(feature = "local_fs")]
#[test]
fn push_publishes_an_untracked_branch_to_origin() {
    assert_eq!(
        super::push_args("feature", None),
        ["push", "--set-upstream", "origin", "feature"]
    );
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn push_destination_follows_the_branch_configuration() {
    let (_dir, repo) = init_repo_with_origin().await;

    assert_eq!(
        super::push_destination(&repo, "main").await,
        Some(super::PushDestination {
            remote: "origin".to_owned(),
            remote_ref: "refs/heads/main".to_owned(),
        })
    );

    git(&repo, &["checkout", "-b", "unpublished"]).await;
    assert_eq!(super::push_destination(&repo, "unpublished").await, None);

    // A branch that tracks another local branch has no remote to push to.
    git(
        &repo,
        &["checkout", "-b", "local-tracking", "--track", "main"],
    )
    .await;
    assert_eq!(super::push_destination(&repo, "local-tracking").await, None);
}

#[cfg(feature = "local_fs")]
#[test]
fn parses_rev_list_left_right_counts() {
    assert_eq!(
        super::parse_tracking_counts("3\t1\n"),
        Some(super::TrackingCounts {
            ahead: 1,
            behind: 3
        })
    );
    assert_eq!(super::parse_tracking_counts(""), None);
    assert_eq!(super::parse_tracking_counts("x\ty"), None);
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn tracking_counts_reports_both_directions() {
    let (_dir, repo) = init_repo_with_origin().await;
    assert_eq!(
        super::tracking_counts(&repo).await.unwrap(),
        Some(super::TrackingCounts::default())
    );

    commit_locally(&repo, "local.txt").await;
    commit_on_origin(&repo, "remote.txt").await;
    super::run_fetch(&repo, None).await.unwrap();

    assert_eq!(
        super::tracking_counts(&repo).await.unwrap(),
        Some(super::TrackingCounts {
            ahead: 1,
            behind: 1
        })
    );

    git(&repo, &["checkout", "-b", "unpublished"]).await;
    assert_eq!(super::tracking_counts(&repo).await.unwrap(), None);
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn pull_fast_forwards_and_refuses_to_merge() {
    let (_dir, repo) = init_repo_with_origin().await;
    commit_on_origin(&repo, "remote.txt").await;

    super::run_pull_ff_only(&repo, None).await.unwrap();
    assert!(repo.join("remote.txt").exists());

    commit_locally(&repo, "local.txt").await;
    commit_on_origin(&repo, "remote2.txt").await;
    let head_before = git(&repo, &["rev-parse", "HEAD"]).await;

    assert!(super::run_pull_ff_only(&repo, None).await.is_err());
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]).await, head_before);
    assert!(!super::git_operation_in_progress(&repo));
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn operation_in_progress_is_detected_in_a_linked_worktree() {
    let (_dir, repo) = init_repo_with_origin().await;
    let worktree = repo.parent().unwrap().join("worktree");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            worktree.to_str().unwrap(),
        ],
    )
    .await;
    assert!(worktree.join(".git").is_file());
    assert!(!super::git_operation_in_progress(&worktree));

    let git_dir =
        std::path::PathBuf::from(git(&worktree, &["rev-parse", "--absolute-git-dir"]).await);
    std::fs::write(git_dir.join("MERGE_HEAD"), "0000\n").unwrap();

    assert!(super::git_operation_in_progress(&worktree));
    assert!(
        !super::git_operation_in_progress(&repo),
        "a merge in a linked worktree does not block the main working tree"
    );
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn discard_all_changes_restores_tracked_files_and_removes_new_ones() {
    let (_dir, repo) = init_repo_with_origin().await;
    std::fs::write(repo.join("second.txt"), "two\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "ignored.log\n").unwrap();
    git(&repo, &["add", "-A"]).await;
    git(&repo, &["commit", "-m", "second"]).await;

    std::fs::write(repo.join("tracked.txt"), "edited\n").unwrap();
    std::fs::remove_file(repo.join("second.txt")).unwrap();
    std::fs::write(repo.join("staged.txt"), "staged\n").unwrap();
    git(&repo, &["add", "staged.txt"]).await;
    std::fs::create_dir_all(repo.join("newdir")).unwrap();
    std::fs::write(repo.join("newdir/untracked.txt"), "untracked\n").unwrap();
    std::fs::write(repo.join("ignored.log"), "kept\n").unwrap();

    super::discard_all_changes(&repo).await.unwrap();

    assert_eq!(git(&repo, &["status", "--porcelain"]).await, "");
    assert_eq!(
        std::fs::read_to_string(repo.join("tracked.txt")).unwrap(),
        "one\n"
    );
    assert!(repo.join("second.txt").exists());
    assert!(!repo.join("staged.txt").exists());
    assert!(!repo.join("newdir").exists());
    assert!(
        repo.join("ignored.log").exists(),
        "ignored files are not changes and must survive"
    );
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn discard_all_changes_empties_a_repository_without_commits() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let repo = dir.path().to_path_buf();
    git(&repo, &["init", "-b", "main"]).await;
    std::fs::write(repo.join("staged.txt"), "staged\n").unwrap();
    git(&repo, &["add", "staged.txt"]).await;
    std::fs::write(repo.join("untracked.txt"), "untracked\n").unwrap();

    super::discard_all_changes(&repo).await.unwrap();

    assert!(!repo.join("staged.txt").exists());
    assert!(!repo.join("untracked.txt").exists());
    assert_eq!(git(&repo, &["status", "--porcelain"]).await, "");
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn file_content_at_head_reads_the_committed_version() {
    let (_dir, repo) = init_repo_with_origin().await;
    std::fs::write(repo.join("tracked.txt"), "edited\n").unwrap();

    assert_eq!(
        super::file_content_at_head(&repo, "tracked.txt").await,
        Some("one\n".to_owned())
    );
    assert_eq!(
        super::file_content_at_head(&repo, "missing.txt").await,
        None
    );
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn push_refuses_a_local_upstream_instead_of_publishing_to_origin() {
    let (_dir, repo) = init_repo_with_origin().await;
    git(
        &repo,
        &["checkout", "-b", "local-tracking", "--track", "main"],
    )
    .await;
    commit_locally(&repo, "local.txt").await;

    let error = super::run_push(&repo, "local-tracking", None)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("tracks a local branch"));
    assert_eq!(
        git(&repo, &["ls-remote", "--heads", "origin", "local-tracking"]).await,
        ""
    );
}
