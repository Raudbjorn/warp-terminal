use std::collections::BTreeMap;
use std::path::Path;
use std::{fs, io};

use serde_json::Value;

use super::*;
use crate::engine::{SearchSpec, build_matcher};
use crate::scope::ScopeGlobs;

/// Writes `files`, given as paths relative to `root`, each holding one line with "needle".
fn write_tree(root: &Path, files: &[&str]) {
    for file in files {
        let path = root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "a needle here\n").unwrap();
    }
}

/// Searches `roots` for "needle" and returns the number of matches per file, by path relative to
/// `base`.
fn search(base: &Path, roots: &[PathBuf], include: &str, exclude: &str) -> BTreeMap<String, usize> {
    let spec = SearchSpec {
        pattern: "needle".to_string(),
        case_sensitive: true,
        ..Default::default()
    };
    let matcher = build_matcher(&[spec.effective_pattern()], false, false).unwrap();
    let scope = Scope::parse(&ScopeGlobs {
        include: include.to_string(),
        exclude: exclude.to_string(),
    })
    .unwrap();
    let output = Mutex::new(Vec::new());
    search_roots(&matcher, false, roots, &scope, &output);

    let mut counts = BTreeMap::new();
    let output = output.into_inner().unwrap();
    for line in output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let message: Value = serde_json::from_slice(line).expect("every line is whole JSON");
        if message["type"] != "match" {
            continue;
        }
        let path = Path::new(message["data"]["path"]["text"].as_str().unwrap());
        let relative = path.strip_prefix(base).unwrap().to_str().unwrap();
        *counts.entry(relative.to_string()).or_default() +=
            message["data"]["submatches"].as_array().unwrap().len();
    }
    counts
}

fn found(base: &Path, roots: &[PathBuf], include: &str, exclude: &str) -> Vec<String> {
    search(base, roots, include, exclude).into_keys().collect()
}

#[test]
fn every_file_below_the_roots_is_searched() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    write_tree(&root, &["a.txt", "src/b.rs", "src/deep/c.rs"]);
    fs::write(root.join("no-match.txt"), "nothing\n").unwrap();
    fs::write(root.join("twice.txt"), "needle needle\nneedle\n").unwrap();

    let counts = search(&root, std::slice::from_ref(&root), "", "");
    assert_eq!(
        counts,
        BTreeMap::from([
            ("a.txt".to_string(), 1),
            ("src/b.rs".to_string(), 1),
            ("src/deep/c.rs".to_string(), 1),
            ("twice.txt".to_string(), 3),
        ])
    );
}

#[test]
fn ignore_files_and_hidden_files_are_respected() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    write_tree(
        &root,
        &[
            "kept.txt",
            "build/out.txt",
            "generated.gen.txt",
            ".hidden/secret.txt",
            ".env",
        ],
    );
    fs::write(root.join(".ignore"), "build/\n*.gen.txt\n").unwrap();

    assert_eq!(
        found(&root, std::slice::from_ref(&root), "", ""),
        ["kept.txt"]
    );
    // Naming an ignored file in the scope does not bring it back.
    assert_eq!(
        found(&root, std::slice::from_ref(&root), "*.txt", ""),
        ["kept.txt"]
    );
    assert!(found(&root, std::slice::from_ref(&root), "build", "").is_empty());
}

#[test]
fn the_scope_limits_which_files_are_searched() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    write_tree(
        &root,
        &[
            "src/a.ts",
            "src/deep/b.ts",
            "src/c.js",
            "src/dist/d.ts",
            "lib/e.ts",
            "f.ts",
        ],
    );
    let roots = std::slice::from_ref(&root);

    assert_eq!(
        found(&root, roots, "src/**/*.ts", ""),
        ["src/a.ts", "src/deep/b.ts", "src/dist/d.ts"]
    );
    assert_eq!(
        found(&root, roots, "src/**/*.ts", "**/dist/**"),
        ["src/a.ts", "src/deep/b.ts"]
    );
    assert_eq!(found(&root, roots, "*.ts, !src", ""), ["f.ts", "lib/e.ts"]);
    assert_eq!(found(&root, roots, "./lib", ""), ["lib/e.ts"]);
    assert_eq!(found(&root, roots, "", "*.ts"), ["src/c.js"]);

    let folder = root.join("src/deep");
    assert_eq!(
        found(&root, roots, folder.to_str().unwrap(), ""),
        ["src/deep/b.ts"]
    );
}

#[test]
fn several_roots_are_searched_with_the_scope_relative_to_each() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().to_path_buf();
    let first = base.join("first");
    let second = base.join("second");
    write_tree(&first, &["src/a.ts", "docs/a.md"]);
    write_tree(&second, &["src/b.ts", "docs/b.md"]);
    let roots = [first, second];

    assert_eq!(
        found(&base, &roots, "", ""),
        [
            "first/docs/a.md",
            "first/src/a.ts",
            "second/docs/b.md",
            "second/src/b.ts"
        ]
    );
    assert_eq!(
        found(&base, &roots, "./src", ""),
        ["first/src/a.ts", "second/src/b.ts"]
    );
    assert!(found(&base, &[], "", "").is_empty());
}

struct ClosedPipe;

impl Write for ClosedPipe {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_reader_that_went_away_ends_the_search() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let files: Vec<String> = (0..50).map(|index| format!("{index}.txt")).collect();
    let files: Vec<&str> = files.iter().map(String::as_str).collect();
    write_tree(&root, &files);

    let matcher = build_matcher(&["needle".to_string()], false, false).unwrap();
    search_roots(
        &matcher,
        false,
        std::slice::from_ref(&root),
        &Scope::unrestricted(),
        &Mutex::new(ClosedPipe),
    );
}
