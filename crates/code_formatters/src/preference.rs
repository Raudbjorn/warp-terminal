use std::collections::HashMap;
use std::path::Path;

use crate::registry::Language;

/// Which formatter the user wants for a file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Preference {
    /// Use a project formatter when the project opts in to one, and the language server otherwise.
    #[default]
    Auto,
    /// Always leave formatting to the language server.
    Lsp,
    /// Never format.
    Off,
    /// Always use the named formatter.
    Tool(String),
}

impl Preference {
    pub fn parse(value: &str) -> Self {
        let value = value.trim().to_ascii_lowercase();
        match value.as_str() {
            "" | "auto" => Self::Auto,
            "lsp" => Self::Lsp,
            "off" => Self::Off,
            _ => Self::Tool(value),
        }
    }

    /// Resolves the preference for `file` from per-language overrides. Keys are matched from most
    /// to least specific: a file extension (`blade.php` before `php`, with or without a leading
    /// dot), then a language identifier such as `typescript`, then `*`.
    pub fn resolve(overrides: &HashMap<String, String>, file: &Path) -> Self {
        if overrides.is_empty() {
            return Self::Auto;
        }
        let overrides: HashMap<String, &str> = overrides
            .iter()
            .map(|(key, value)| (normalize_key(key), value.as_str()))
            .collect();

        let file_name = file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let extensions = file_name
            .match_indices('.')
            .map(|(dot, _)| &file_name[dot + 1..]);
        let language = Language::from_path(file).map(Language::id);

        extensions
            .chain(language)
            .chain(["*"])
            .find_map(|key| overrides.get(key))
            .map_or(Self::Auto, |value| Self::parse(value))
    }
}

fn normalize_key(key: &str) -> String {
    let key = key.trim().to_ascii_lowercase();
    match key.strip_prefix('.') {
        Some(extension) => extension.to_owned(),
        None => key,
    }
}

#[cfg(test)]
#[path = "preference_tests.rs"]
mod tests;
