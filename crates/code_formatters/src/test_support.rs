//! Throwaway project trees and stand-in formatter executables for tests.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// Uppercases its input, the way a real formatter rewrites stdin to stdout.
pub const UPPERCASE: &str = "#!/bin/sh\ntr '[:lower:]' '[:upper:]'\n";

/// A temporary directory tree standing in for a project.
pub struct Project {
    dir: TempDir,
}

impl Project {
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("code-formatters-")
            .tempdir()
            .expect("create temp dir");
        Self { dir }
    }

    /// The project root, canonicalized so it compares equal to paths that tools report.
    pub fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().expect("canonicalize root")
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root().join(relative)
    }

    pub fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(&path).expect("create dir");
        path
    }

    pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(&path, contents).expect("write file");
        path
    }

    /// Writes an executable shell script.
    pub fn script(&self, relative: &str, script: &str) -> PathBuf {
        let path = self.write(relative, script);
        make_executable(&path);
        path
    }

    /// A `PATH` value made of the given project directories.
    pub fn path_env(&self, relative_dirs: &[&str]) -> String {
        relative_dirs
            .iter()
            .map(|dir| self.path(dir).display().to_string())
            .collect::<Vec<_>>()
            .join(":")
    }
}

fn make_executable(path: &Path) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}
