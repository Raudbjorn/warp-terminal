//! Narrowing a search to the files some patterns include and exclude.

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use thiserror::Error;

/// The "files to include" and "files to exclude" fields as the user typed them.
///
/// Each holds patterns separated by commas. A pattern follows `.gitignore` rules: without a slash
/// it matches at any depth, with one it is relative to the search root, and a folder stands for
/// everything below it. `./` anchors a pattern to the search root, an absolute path stands for
/// that file or folder, and a pattern starting with `!` excludes whichever field it is in. A
/// backslash makes the character after it stand for itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeGlobs {
    pub include: String,
    pub exclude: String,
}

impl ScopeGlobs {
    pub fn is_empty(&self) -> bool {
        self.include.trim().is_empty() && self.exclude.trim().is_empty()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("“{pattern}” is not a valid file pattern: {message}")]
pub struct ScopeError {
    pub pattern: String,
    pub message: String,
}

#[derive(Default)]
struct PatternSet {
    globs: Option<Gitignore>,
    /// Absolute paths, each standing for that file or everything below that folder.
    paths: Vec<PathBuf>,
}

impl PatternSet {
    fn is_empty(&self) -> bool {
        self.globs.is_none() && self.paths.is_empty()
    }

    fn matches(&self, path: &Path, relative: Option<&Path>) -> bool {
        if self.paths.iter().any(|prefix| path.starts_with(prefix)) {
            return true;
        }
        match (&self.globs, relative) {
            (Some(globs), Some(relative)) => {
                // The matcher panics on a path that is not relative to its root.
                !relative.as_os_str().is_empty()
                    && !relative.has_root()
                    && globs
                        .matched_path_or_any_parents(relative, false)
                        .is_ignore()
            }
            (None, _) | (_, None) => false,
        }
    }
}

/// Decides which files a search covers.
#[derive(Default)]
pub struct Scope {
    include: PatternSet,
    exclude: PatternSet,
}

impl Scope {
    /// A scope that covers every file.
    pub fn unrestricted() -> Self {
        Self::default()
    }

    pub fn parse(globs: &ScopeGlobs) -> Result<Self, ScopeError> {
        let mut include = PatternSetBuilder::default();
        let mut exclude = PatternSetBuilder::default();

        for pattern in patterns(&globs.include) {
            match pattern.strip_prefix('!') {
                Some(excluded) => exclude.add(excluded.trim())?,
                None => include.add(pattern)?,
            }
        }
        for pattern in patterns(&globs.exclude) {
            exclude.add(pattern.strip_prefix('!').unwrap_or(pattern).trim())?;
        }

        Ok(Self {
            include: include.build()?,
            exclude: exclude.build()?,
        })
    }

    pub fn is_unrestricted(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    /// Whether the file or folder at `path`, found below the search root `root`, is covered.
    pub fn includes(&self, root: &Path, path: &Path) -> bool {
        let relative = path.strip_prefix(root).ok();
        let included = self.include.is_empty() || self.include.matches(path, relative);
        included && !self.exclude.matches(path, relative)
    }
}

/// The characters that need a backslash to stand for themselves in a pattern.
const SPECIAL_CHARACTERS: [char; 10] = ['*', '?', '[', ']', '{', '}', ',', '\\', '!', '#'];

/// The pattern that limits a search to the folder at `folder`: relative to `root` when the folder
/// is below that search root, and the absolute path of the folder otherwise.
pub fn folder_pattern(root: Option<&Path>, folder: &Path) -> String {
    let relative = root
        .and_then(|root| folder.strip_prefix(root).ok())
        .filter(|relative| !relative.as_os_str().is_empty());
    match relative {
        Some(relative) => format!("./{}", escaped(&relative.to_string_lossy())),
        None => escaped(&folder.to_string_lossy()),
    }
}

fn escaped(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if SPECIAL_CHARACTERS.contains(&ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// Splits a field at the commas and line breaks that no backslash escapes.
fn patterns(field: &str) -> Vec<&str> {
    let mut patterns = Vec::new();
    let mut start = 0;
    let mut chars = field.char_indices();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '\\' => {
                chars.next();
            }
            ',' | '\n' => {
                patterns.push(&field[start..index]);
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    patterns.push(&field[start..]);

    patterns
        .into_iter()
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .collect()
}

/// The path `pattern` names, when it is an absolute path with no wildcard in it.
fn literal_absolute_path(pattern: &str) -> Option<PathBuf> {
    let mut literal = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => literal.push(chars.next()?),
            '*' | '?' | '[' | '{' => return None,
            _ => literal.push(ch),
        }
    }
    Path::new(&literal)
        .is_absolute()
        .then(|| PathBuf::from(literal))
}

#[derive(Default)]
struct PatternSetBuilder {
    globs: Option<GitignoreBuilder>,
    paths: Vec<PathBuf>,
}

impl PatternSetBuilder {
    fn add(&mut self, pattern: &str) -> Result<(), ScopeError> {
        if pattern.is_empty() {
            return Ok(());
        }

        if let Some(path) = literal_absolute_path(pattern) {
            self.paths.push(path);
            return Ok(());
        }

        let anchored = pattern.strip_prefix("./").map(|rest| format!("/{rest}"));
        let pattern = anchored.as_deref().unwrap_or(pattern);
        // A leading `#` or `!` would turn the line into a comment or a negation.
        let line = if pattern.starts_with(['#', '!']) {
            format!("\\{pattern}")
        } else {
            pattern.to_string()
        };
        self.globs
            .get_or_insert_with(|| GitignoreBuilder::new(""))
            .add_line(None, &line)
            .map_err(|err| ScopeError {
                pattern: pattern.to_string(),
                message: err.to_string(),
            })?;
        Ok(())
    }

    fn build(self) -> Result<PatternSet, ScopeError> {
        let globs = self
            .globs
            .map(|builder| builder.build())
            .transpose()
            .map_err(|err| ScopeError {
                pattern: String::new(),
                message: err.to_string(),
            })?;
        Ok(PatternSet {
            globs,
            paths: self.paths,
        })
    }
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
