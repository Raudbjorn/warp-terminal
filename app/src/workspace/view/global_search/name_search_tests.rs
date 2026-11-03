use std::fs;

use repo_metadata::{DirectoryEntry, Entry, FileMetadata};
use tempfile::TempDir;

use super::*;
use crate::workspace::view::global_search::{RankedNameMatches, SearchConfig};

fn path(path: &str) -> StandardizedPath {
    StandardizedPath::try_new(path).expect("test paths are absolute")
}

fn file(file_path: &str) -> Entry {
    Entry::File(FileMetadata::from_standardized(path(file_path), false))
}

fn ignored_file(file_path: &str) -> Entry {
    Entry::File(FileMetadata::from_standardized(path(file_path), true))
}

fn directory(directory_path: &str, children: Vec<Entry>) -> Entry {
    Entry::Directory(DirectoryEntry {
        path: path(directory_path),
        children,
        ignored: false,
        loaded: true,
    })
}

fn ignored_directory(directory_path: &str, children: Vec<Entry>) -> Entry {
    Entry::Directory(DirectoryEntry {
        path: path(directory_path),
        children,
        ignored: true,
        loaded: true,
    })
}

/// A repository at `/repo` with matches for "search" among its ignored and `.git` entries too.
fn repo_tree() -> FileTreeEntry {
    directory(
        "/repo",
        vec![
            file("/repo/README.md"),
            ignored_file("/repo/search.log"),
            directory("/repo/.git", vec![file("/repo/.git/search_index")]),
            directory("/repo/.github", vec![file("/repo/.github/search.yml")]),
            ignored_directory(
                "/repo/target",
                vec![
                    file("/repo/target/search.rs"),
                    directory("/repo/target/search", vec![]),
                ],
            ),
            directory(
                "/repo/src",
                vec![
                    file("/repo/src/main.rs"),
                    file("/repo/src/search_bar.rs"),
                    directory("/repo/src/search", vec![file("/repo/src/search/mod.rs")]),
                ],
            ),
        ],
    )
    .into()
}

fn indexed_root(directory: &str, display_prefix: &str) -> NameSearchRoot {
    NameSearchRoot {
        root: LocalOrRemotePath::Local(directory.into()),
        source: NameSource::Index {
            tree: repo_tree(),
            directory: path(directory),
        },
        display_prefix: display_prefix.to_string(),
    }
}

fn search(roots: &[NameSearchRoot], query: &str) -> NameSearchResults {
    let matcher = NameMatcher::new(
        query,
        &SearchConfig {
            use_regex: false,
            use_case_sensitivity: false,
        },
    )
    .expect("query should be matchable");
    search_names(
        roots,
        &matcher,
        &Scope::unrestricted(),
        &AtomicBool::new(false),
    )
    .expect("search was not cancelled")
}

fn display_paths(ranked: &RankedNameMatches) -> Vec<&str> {
    let mut paths: Vec<_> = ranked
        .matches
        .iter()
        .map(|name_match| name_match.display_path.as_str())
        .collect();
    paths.sort_unstable();
    paths
}

#[test]
fn indexed_search_finds_folders_and_files_but_skips_ignored_and_git_entries() {
    let results = search(&[indexed_root("/repo", "")], "search");

    assert_eq!(display_paths(&results.folders), vec!["src/search"]);
    assert_eq!(
        display_paths(&results.files),
        vec![".github/search.yml", "src/search_bar.rs"]
    );
    assert_eq!(results.folders.total, 1);
    assert_eq!(results.files.total, 2);
}

#[test]
fn matches_are_located_under_the_search_root() {
    let results = search(&[indexed_root("/repo", "")], "search_bar");

    let name_match = &results.files.matches[0];
    assert_eq!(
        name_match.location,
        LocalOrRemotePath::Local("/repo/src/search_bar.rs".into())
    );
    // "src/" precedes the name.
    assert_eq!(name_match.highlight_indices, (4..14).collect::<Vec<_>>());
}

#[test]
fn indexed_search_is_limited_to_the_root_directory() {
    let results = search(&[indexed_root("/repo/src", "")], "search");

    assert_eq!(display_paths(&results.folders), vec!["search"]);
    assert_eq!(display_paths(&results.files), vec!["search_bar.rs"]);
    assert_eq!(
        results.folders.matches[0].location,
        LocalOrRemotePath::Local("/repo/src/search".into())
    );
}

#[test]
fn display_prefix_tells_roots_apart_and_shifts_highlights() {
    let roots = [
        indexed_root("/repo/src", "src/"),
        indexed_root("/repo/.github", ".github/"),
    ];
    let results = search(&roots, "search");

    assert_eq!(
        display_paths(&results.files),
        vec![".github/search.yml", "src/search_bar.rs"]
    );
    let workflow = results
        .files
        .matches
        .iter()
        .find(|name_match| name_match.display_path == ".github/search.yml")
        .expect("workflow file should match");
    assert_eq!(workflow.highlight_indices, (8..14).collect::<Vec<_>>());
    assert_eq!(
        workflow.location,
        LocalOrRemotePath::Local("/repo/.github/search.yml".into())
    );
}

#[test]
fn cancelled_search_returns_nothing() {
    let matcher = NameMatcher::new(
        "search",
        &SearchConfig {
            use_regex: false,
            use_case_sensitivity: false,
        },
    )
    .expect("query should be matchable");

    let results = search_names(
        &[indexed_root("/repo", "")],
        &matcher,
        &Scope::unrestricted(),
        &AtomicBool::new(true),
    );
    assert!(results.is_none());
}

#[test]
fn disk_search_honours_ignore_files_and_skips_git_internals() {
    let directory = TempDir::new().expect("should create a temporary directory");
    let root = directory.path();
    for subdirectory in ["src/search", "ignored/search", ".git/search", ".config"] {
        fs::create_dir_all(root.join(subdirectory)).expect("should create directory");
    }
    for file in [
        ".ignore",
        "src/search_bar.rs",
        "src/main.rs",
        "ignored/search.rs",
        ".git/search_index",
        ".config/search.toml",
    ] {
        let contents = if file == ".ignore" { "ignored/\n" } else { "" };
        fs::write(root.join(file), contents).expect("should write file");
    }

    let roots = [NameSearchRoot {
        root: LocalOrRemotePath::Local(root.to_path_buf()),
        source: NameSource::Disk,
        display_prefix: String::new(),
    }];
    let results = search(&roots, "search");

    assert_eq!(display_paths(&results.folders), vec!["src/search"]);
    assert_eq!(
        display_paths(&results.files),
        vec![".config/search.toml", "src/search_bar.rs"]
    );
    assert_eq!(
        results.folders.matches[0].location,
        LocalOrRemotePath::Local(root.join("src/search"))
    );
}
