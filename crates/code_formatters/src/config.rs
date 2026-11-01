//! Detection of the project files that opt a project in to a formatter.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::discovery::SearchScope;

/// Manifests larger than this are not parsed, so a pathological file cannot stall a save.
const MAX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;

const PRETTIER_CONFIG_FILES: &[&str] = &[
    ".prettierrc",
    ".prettierrc.json",
    ".prettierrc.json5",
    ".prettierrc.yaml",
    ".prettierrc.yml",
    ".prettierrc.toml",
    ".prettierrc.js",
    ".prettierrc.cjs",
    ".prettierrc.mjs",
    ".prettierrc.ts",
    ".prettierrc.cts",
    ".prettierrc.mts",
    "prettier.config.js",
    "prettier.config.cjs",
    "prettier.config.mjs",
    "prettier.config.ts",
    "prettier.config.cts",
    "prettier.config.mts",
];

const NODE_DEPENDENCY_SECTIONS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

/// The nearest Prettier config: a dedicated config file, or a `package.json` with a `prettier`
/// key.
pub fn prettier_config(scope: &SearchScope) -> Option<PathBuf> {
    scope.dirs().find_map(|dir| {
        PRETTIER_CONFIG_FILES
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.is_file())
            .or_else(|| {
                let manifest = dir.join("package.json");
                read_json(&manifest)?.get("prettier")?;
                Some(manifest)
            })
    })
}

/// Whether a `package.json` in scope lists one of `packages` as a dependency of any kind.
pub fn node_dependency(scope: &SearchScope, packages: &[&str]) -> bool {
    scope
        .dirs()
        .filter_map(|dir| read_json(&dir.join("package.json")))
        .any(|manifest| {
            NODE_DEPENDENCY_SECTIONS.iter().any(|section| {
                manifest
                    .get(section)
                    .and_then(Value::as_object)
                    .is_some_and(|deps| packages.iter().any(|package| deps.contains_key(*package)))
            })
        })
}

/// Whether one of `packages` is declared by, or installed in, the project.
pub fn node_package_available(scope: &SearchScope, packages: &[&str]) -> bool {
    node_dependency(scope, packages)
        || scope.dirs().any(|dir| {
            packages.iter().any(|package| {
                dir.join("node_modules")
                    .join(package)
                    .join("package.json")
                    .is_file()
            })
        })
}

pub fn biome_config(scope: &SearchScope) -> Option<PathBuf> {
    scope.find_file(&["biome.json", "biome.jsonc"])
}

pub fn dprint_config(scope: &SearchScope) -> Option<PathBuf> {
    scope.find_file(&[
        "dprint.json",
        "dprint.jsonc",
        ".dprint.json",
        ".dprint.jsonc",
    ])
}

pub fn ruff_config(scope: &SearchScope) -> Option<PathBuf> {
    scope.dirs().find_map(|dir| {
        ["ruff.toml", ".ruff.toml"]
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.is_file())
            .or_else(|| pyproject_with_tool(dir, "ruff"))
    })
}

pub fn black_config(scope: &SearchScope) -> Option<PathBuf> {
    scope
        .dirs()
        .find_map(|dir| pyproject_with_tool(dir, "black"))
}

pub fn sqlfluff_config(scope: &SearchScope) -> Option<PathBuf> {
    scope.dirs().find_map(|dir| {
        let dedicated = dir.join(".sqlfluff");
        if dedicated.is_file() {
            return Some(dedicated);
        }
        pyproject_with_tool(dir, "sqlfluff").or_else(|| {
            ["setup.cfg", "tox.ini", "pep8.ini"]
                .iter()
                .map(|name| dir.join(name))
                .find(|path| {
                    read_text(path).is_some_and(|contents| has_table(&contents, "sqlfluff"))
                })
        })
    })
}

pub fn pg_format_config(scope: &SearchScope) -> Option<PathBuf> {
    scope.find_file(&[".pg_format"])
}

pub fn taplo_config(scope: &SearchScope) -> Option<PathBuf> {
    scope.find_file(&[".taplo.toml", "taplo.toml"])
}

pub fn blade_formatter_config(scope: &SearchScope) -> Option<PathBuf> {
    scope.find_file(&[".bladeformatterrc", ".bladeformatterrc.json"])
}

/// The Rust edition of the crate that owns the scope's file, following `edition.workspace`
/// inheritance up to the workspace manifest.
pub fn rust_edition(scope: &SearchScope) -> Option<String> {
    let mut inherits_from_workspace = false;
    for dir in scope.dirs() {
        let Some(manifest) = read_text(&dir.join("Cargo.toml")) else {
            continue;
        };
        if !inherits_from_workspace {
            if let Some(edition) = table_value(&manifest, "package", "edition") {
                match parse_edition(edition) {
                    Some(edition) => return Some(edition),
                    None => inherits_from_workspace = true,
                }
            } else if table_value(&manifest, "package", "edition.workspace").is_some() {
                inherits_from_workspace = true;
            }
        }
        if let Some(edition) =
            table_value(&manifest, "workspace.package", "edition").and_then(parse_edition)
        {
            return Some(edition);
        }
        // A package that neither sets nor inherits an edition settles the question: nothing
        // further up applies to it.
        if !inherits_from_workspace && has_table(&manifest, "package") {
            return None;
        }
    }
    None
}

fn parse_edition(value: &str) -> Option<String> {
    let edition = value.trim_matches(|c| c == '"' || c == '\'');
    (edition.len() == 4 && edition.chars().all(|c| c.is_ascii_digit())).then(|| edition.to_owned())
}

fn pyproject_with_tool(dir: &Path, tool: &str) -> Option<PathBuf> {
    let path = dir.join("pyproject.toml");
    let contents = read_text(&path)?;
    has_table(&contents, &format!("tool.{tool}")).then_some(path)
}

fn read_text(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&read_text(path)?).ok()
}

/// The name inside a TOML or INI table header such as `[tool.ruff]`, `[[tool.x]]` or
/// `[sqlfluff:rules]`.
fn table_header(line: &str) -> Option<String> {
    let line = line.trim();
    let inner = line.strip_prefix('[')?;
    let inner = inner.strip_prefix('[').unwrap_or(inner);
    let (name, _) = inner.split_once(']')?;
    Some(name.chars().filter(|c| !c.is_whitespace()).collect())
}

/// Whether the document declares the table `name` or one nested inside it. Manifests are only
/// scanned for table headers, which is all that opting in to a tool requires, rather than parsed
/// in full.
fn has_table(contents: &str, name: &str) -> bool {
    contents
        .lines()
        .filter_map(table_header)
        .any(|header| match header.strip_prefix(name) {
            Some(rest) => rest.is_empty() || rest.starts_with(['.', ':']),
            None => false,
        })
}

/// The raw text assigned to `key` by a `key = value` line directly inside `table`.
fn table_value<'a>(contents: &'a str, table: &str, key: &str) -> Option<&'a str> {
    let mut in_table = false;
    for line in contents.lines() {
        if let Some(header) = table_header(line) {
            in_table = header == table;
            continue;
        }
        if !in_table {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() == key {
            let value = value.split_once('#').map_or(value, |(value, _)| value);
            return Some(value.trim());
        }
    }
    None
}

// The tests drive stand-in formatters written as shell scripts.
#[cfg(all(test, unix))]
#[path = "config_tests.rs"]
mod tests;
