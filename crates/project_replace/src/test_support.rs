//! A throwaway project to search and replace in.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde_json::Value;
use tempfile::TempDir;

use crate::apply::{
    DEFAULT_MAX_FILE_BYTES, FileTarget, OpenBuffer, ReplaceOutcome, ReplaceRequest,
    replace_in_files,
};
use crate::engine::{SearchSpec, build_matcher, build_searcher, column_of, search_path_json};
use crate::journal::{Journal, JournalLimits};
use crate::replacement::ReplaceSpec;
use crate::scope::ScopeGlobs;
use crate::selection::{Dismissals, Selection, TargetsBuilder};

pub(crate) struct Project {
    directory: TempDir,
    pub(crate) journal: Journal,
}

impl Project {
    pub(crate) fn new() -> Self {
        Self::with_limits(JournalLimits::default())
    }

    pub(crate) fn with_limits(limits: JournalLimits) -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("project")).unwrap();
        let journal = Journal::new(directory.path().join("journal"), limits);
        Self { directory, journal }
    }

    pub(crate) fn root(&self) -> PathBuf {
        self.directory.path().join("project")
    }

    pub(crate) fn path(&self, relative: &str) -> PathBuf {
        self.root().join(relative)
    }

    pub(crate) fn write(&self, relative: &str, content: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }

    pub(crate) fn read(&self, relative: &str) -> Vec<u8> {
        fs::read(self.path(relative)).unwrap()
    }

    pub(crate) fn read_text(&self, relative: &str) -> String {
        String::from_utf8(self.read(relative)).unwrap()
    }

    /// The names of everything in the project folder, to check that nothing was left behind.
    pub(crate) fn file_names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.root())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// Searches `files` the way the search worker does and gathers what the results list would
    /// hand over for replacing.
    pub(crate) fn search(
        &self,
        spec: &SearchSpec,
        files: &[&str],
        dismissals: &Dismissals,
        selection: Selection,
    ) -> Vec<FileTarget> {
        let multiline = spec.is_multiline();
        let matcher =
            build_matcher(&[spec.effective_pattern()], spec.ignore_case(), multiline).unwrap();
        let mut builder = TargetsBuilder::new(dismissals, selection);
        for file in files {
            let path = self.path(file);
            let mut output = Vec::new();
            let _ = search_path_json(&matcher, &mut build_searcher(multiline), &path, &mut output);
            for row in rows(&output) {
                builder.add_row(&path, row.line, row.column, &row.lines, row.start..row.end);
            }
        }
        builder.build()
    }

    pub(crate) fn search_all(&self, spec: &SearchSpec, files: &[&str]) -> Vec<FileTarget> {
        self.search(spec, files, &Dismissals::default(), Selection::All)
    }

    pub(crate) fn request(
        &self,
        search: &SearchSpec,
        replacement: &str,
        files: Vec<FileTarget>,
    ) -> ReplaceRequest {
        ReplaceRequest {
            search: search.clone(),
            replace: ReplaceSpec {
                replacement: replacement.to_string(),
                preserve_case: false,
            },
            roots: vec![self.root()],
            scope: ScopeGlobs::default(),
            files,
            open_buffers: Vec::new(),
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        }
    }

    pub(crate) fn replace(&self, request: &ReplaceRequest) -> ReplaceOutcome {
        replace_in_files(request, &self.journal, &|_| {}, &AtomicBool::new(false)).unwrap()
    }

    /// Searches `files` for the literal `pattern` and replaces every match with `replacement`.
    pub(crate) fn replace_literal(
        &self,
        pattern: &str,
        replacement: &str,
        files: &[&str],
    ) -> ReplaceOutcome {
        let spec = literal(pattern);
        let targets = self.search_all(&spec, files);
        self.replace(&self.request(&spec, replacement, targets))
    }
}

pub(crate) fn literal(pattern: &str) -> SearchSpec {
    SearchSpec {
        pattern: pattern.to_string(),
        case_sensitive: true,
        ..Default::default()
    }
}

pub(crate) fn regex(pattern: &str) -> SearchSpec {
    SearchSpec {
        use_regex: true,
        ..literal(pattern)
    }
}

pub(crate) fn open_buffer(path: &Path, text: &str, has_unsaved_changes: bool) -> OpenBuffer {
    OpenBuffer {
        path: path.to_path_buf(),
        text: text.to_string(),
        has_unsaved_changes,
    }
}

struct WorkerRow {
    line: u32,
    column: usize,
    lines: String,
    start: usize,
    end: usize,
}

fn rows(output: &[u8]) -> Vec<WorkerRow> {
    let mut rows = Vec::new();
    for line in output
        .split(|byte| *byte == b'\n')
        .filter(|l| !l.is_empty())
    {
        let message: Value = serde_json::from_slice(line).unwrap();
        if message["type"] != "match" {
            continue;
        }
        let data = &message["data"];
        let Some(lines) = data["lines"]["text"].as_str() else {
            continue;
        };
        for submatch in data["submatches"].as_array().unwrap() {
            let start = submatch["start"].as_u64().unwrap() as usize;
            let end = submatch["end"].as_u64().unwrap() as usize;
            rows.push(WorkerRow {
                line: data["line_number"].as_u64().unwrap() as u32,
                column: column_of(lines, start).unwrap(),
                lines: lines.to_string(),
                start,
                end,
            });
        }
    }
    rows
}
