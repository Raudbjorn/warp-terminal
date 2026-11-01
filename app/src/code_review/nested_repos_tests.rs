use std::fs;
use std::path::Path;

use tempfile::TempDir;

use super::*;

fn init_repo(root: &Path) {
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
}

fn init_linked_worktree(main_repo: &Path, worktree: &Path, name: &str) {
    let git_dir = main_repo.join(".git/worktrees").join(name);
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature\n").unwrap();
    fs::create_dir_all(worktree).unwrap();
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", git_dir.display()),
    )
    .unwrap();
}

fn relative_roots(scan_root: &Path, repos: &[NestedRepo]) -> Vec<String> {
    repos
        .iter()
        .map(|repo| {
            repo.root
                .strip_prefix(scan_root)
                .unwrap()
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        })
        .collect()
}

#[test]
fn finds_repositories_and_linked_worktrees() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    init_repo(&root.join("Repos/app"));
    init_linked_worktree(
        &root.join("Repos/app"),
        &root.join("Repos/app-wt-fix"),
        "fix",
    );
    init_repo(&root.join("Projects/site/Repos/web"));

    let repos = find_nested_repos(&root);

    assert_eq!(
        relative_roots(&root, &repos),
        ["Projects/site/Repos/web", "Repos/app", "Repos/app-wt-fix"]
    );
    assert_eq!(repos[1].kind, NestedRepoKind::Repository);
    assert_eq!(repos[2].kind, NestedRepoKind::Worktree);
    assert_eq!(repos[2].git_dir, root.join("Repos/app/.git/worktrees/fix"));
}

#[test]
fn skips_node_modules_and_git_internals() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    init_repo(&root.join("app"));
    init_repo(&root.join("app/node_modules/dependency"));
    init_repo(&root.join("app/.git/modules/vendored"));

    assert_eq!(relative_roots(root, &find_nested_repos(root)), ["app"]);
}

#[test]
fn includes_repositories_nested_in_other_repositories() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    init_repo(&root.join("outer"));
    init_repo(&root.join("outer/vendor/inner"));

    assert_eq!(
        relative_roots(root, &find_nested_repos(root)),
        ["outer", "outer/vendor/inner"]
    );
}

#[test]
fn stops_at_the_depth_limit() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    // The `.git` entry of `a/b/c/d/e` sits at depth 6, and that of `a/b/c/d/e/f` at depth 7.
    init_repo(&root.join("a/b/c/d/e"));
    init_repo(&root.join("a/b/c/d/e/f"));

    assert_eq!(
        relative_roots(root, &find_nested_repos(root)),
        ["a/b/c/d/e"]
    );
}

#[test]
fn ignores_git_entries_without_a_head() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join("empty/.git")).unwrap();
    fs::create_dir_all(root.join("dangling")).unwrap();
    fs::write(root.join("dangling/.git"), "gitdir: /nonexistent/git/dir\n").unwrap();

    assert!(find_nested_repos(root).is_empty());
}

#[test]
fn parses_branch_tracking_and_entries() {
    let output = [
        "# branch.oid 1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b",
        "# branch.head feature/login",
        "# branch.upstream origin/feature/login",
        "# branch.ab +3 -1",
        "1 .M N... 100644 100644 100644 aaaa bbbb src/app with space.ts",
        "1 A. N... 000000 100644 100644 0000 cccc src/new.ts",
        "1 .D N... 100644 100644 000000 dddd dddd src/removed.ts",
        "2 R. N... 100644 100644 100644 eeee eeee R100 src/renamed.ts",
        "src/original.ts",
        "u UU N... 100644 100644 100644 100644 ffff ffff ffff src/conflict.ts",
        "? notes.md",
        "",
    ]
    .join("\0");

    let status = parse_porcelain_v2_status(&output);

    assert_eq!(status.branch, "feature/login");
    assert!(!status.is_detached);
    assert_eq!(
        status.head_oid.as_deref(),
        Some("1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b")
    );
    assert_eq!(status.upstream.as_deref(), Some("origin/feature/login"));
    assert_eq!((status.ahead, status.behind), (3, 1));
    let files: Vec<(&str, &GitFileStatus)> = status
        .files
        .iter()
        .map(|file| (file.path.as_str(), &file.status))
        .collect();
    assert_eq!(
        files,
        [
            ("src/app with space.ts", &GitFileStatus::Modified),
            ("src/new.ts", &GitFileStatus::New),
            ("src/removed.ts", &GitFileStatus::Deleted),
            (
                "src/renamed.ts",
                &GitFileStatus::Renamed {
                    old_path: "src/original.ts".to_owned()
                }
            ),
            ("src/conflict.ts", &GitFileStatus::Conflicted),
            ("notes.md", &GitFileStatus::Untracked),
        ]
    );
}

#[test]
fn shows_abbreviated_commit_for_detached_head() {
    let output =
        "# branch.oid 1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b\0# branch.head (detached)\0";

    let status = parse_porcelain_v2_status(output);

    assert_eq!(status.branch, "1a2b3c4");
    assert!(status.is_detached);
    assert_eq!(status.upstream, None);
    assert!(status.files.is_empty());
}

#[test]
fn reports_no_head_commit_before_the_first_commit() {
    let output = "# branch.oid (initial)\0# branch.head main\0? notes.md\0";

    let status = parse_porcelain_v2_status(output);

    assert_eq!(status.branch, "main");
    assert!(!status.is_detached);
    assert_eq!(status.head_oid, None);
    assert_eq!(status.files.len(), 1);
}

#[test]
fn watch_filter_excludes_dependencies_and_git_objects() {
    assert!(is_watched_path(Path::new("/work/app/src/main.ts")));
    assert!(is_watched_path(Path::new("/work/app/.git/HEAD")));
    assert!(is_watched_path(Path::new("/work/app/.git/refs/heads/main")));
    assert!(is_watched_path(Path::new("/work/app/objects/model.ts")));
    assert!(!is_watched_path(Path::new(
        "/work/app/.git/objects/ab/cdef"
    )));
    assert!(!is_watched_path(Path::new(
        "/work/app/node_modules/react/index.js"
    )));
}

#[test]
fn moved_in_repository_directory_requests_discovery() {
    let temp = TempDir::new().unwrap();
    let incoming = temp.path().join("incoming");
    init_repo(&incoming);
    let scan_root = temp.path().join("workspace");
    fs::create_dir(&scan_root).unwrap();
    let destination = scan_root.join("moved-repo");
    fs::rename(incoming, &destination).unwrap();
    assert!(may_add_repository(&destination));
    assert_eq!(
        relative_roots(&scan_root, &find_nested_repos(&scan_root)),
        ["moved-repo"]
    );
    fs::create_dir_all(destination.join(".git/refs/heads")).unwrap();
    assert!(!may_add_repository(&destination.join(".git/refs/heads")));
    assert!(!may_add_repository(&destination.join("missing.txt")));
}

#[test]
fn status_failure_is_distinct_from_loading_and_recovers() {
    let temp = TempDir::new().unwrap();
    init_repo(temp.path());
    let repo = find_nested_repos(temp.path()).pop().unwrap();
    let mut entry = NestedRepoEntry {
        repo,
        display_name: "app".to_owned(),
        status: None,
        status_failed: false,
        load_state: StatusLoadState::Idle,
    };
    entry.update_status(Err(anyhow::anyhow!("git status timed out")));
    assert!(entry.status_failed);
    assert!(entry.status.is_none());
    entry.update_status(Ok(NestedRepoStatus::default()));
    assert!(!entry.status_failed);
    assert!(entry.status.is_some());
}
