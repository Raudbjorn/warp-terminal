use std::path::{Path, PathBuf};

use warp_util::git::run_git_command;

use super::resolve_project;

async fn git(dir: &Path, args: &[&str]) {
    run_git_command(dir, args).await.expect("git");
}

/// A repository named `checkout` with one commit, so a worktree can be added.
async fn repo(parent: &Path) -> PathBuf {
    let root = parent.join("checkout");
    std::fs::create_dir_all(root.join("app/src")).expect("mkdir");
    git(&root, &["init", "-b", "main"]).await;
    git(&root, &["config", "user.email", "test@test.com"]).await;
    git(&root, &["config", "user.name", "Test"]).await;
    git(&root, &["commit", "--allow-empty", "-m", "init"]).await;
    root
}

#[tokio::test]
async fn the_repository_root_names_the_project() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = repo(dir.path()).await;

    assert_eq!(resolve_project(&root).await.as_deref(), root.to_str());
}

#[tokio::test]
async fn a_subdirectory_belongs_to_its_repository() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = repo(dir.path()).await;

    assert_eq!(
        resolve_project(&root.join("app/src")).await.as_deref(),
        root.to_str()
    );
}

#[tokio::test]
async fn a_linked_worktree_belongs_to_its_repository() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = repo(dir.path()).await;
    let worktree = dir.path().join("feature-x");
    git(
        &root,
        &[
            "worktree",
            "add",
            worktree.to_str().expect("utf-8 path"),
            "-b",
            "feature-x",
        ],
    )
    .await;

    assert_eq!(resolve_project(&worktree).await.as_deref(), root.to_str());
}

#[tokio::test]
async fn outside_git_the_directory_names_the_project() {
    let dir = tempfile::tempdir().expect("temp dir");
    let plain = dir.path().join("notes");
    std::fs::create_dir(&plain).expect("mkdir");

    assert_eq!(resolve_project(&plain).await.as_deref(), plain.to_str());
}

#[tokio::test]
async fn the_root_directory_names_nothing() {
    assert_eq!(resolve_project(Path::new("/")).await, None);
}

#[test]
fn an_ordinary_directory_names_no_network_host() {
    for path in ["/home/me/app", "/tmp", "relative/dir", ""] {
        assert!(
            !super::names_network_host(std::path::Path::new(path)),
            "{path:?} is local"
        );
    }
}

/// Windows authenticates to a share it is asked about, so a directory the terminal names on one is
/// never handed to git, while a WSL distribution, which is spelt the same way, still is.
#[cfg(windows)]
#[test]
fn a_network_share_is_not_a_place_to_run_git() {
    use std::path::Path;

    for path in [
        r"\\attacker-host\share\repo",
        r"\\?\UNC\attacker-host\share\repo",
        r"\\.\pipe\repo",
    ] {
        assert!(super::names_network_host(Path::new(path)), "{path}");
    }
    assert!(!super::names_network_host(Path::new(
        r"\\wsl.localhost\Ubuntu\home\me\repo"
    )));
    assert!(!super::names_network_host(Path::new(r"C:\Users\me\repo")));
}
