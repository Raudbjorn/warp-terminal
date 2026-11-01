use std::collections::HashMap;
use std::path::Path;

use super::*;

fn overrides(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn resolve(entries: &[(&str, &str)], file: &str) -> Preference {
    Preference::resolve(&overrides(entries), Path::new(file))
}

#[test]
fn parses_the_reserved_values_and_tool_names() {
    assert_eq!(Preference::parse("auto"), Preference::Auto);
    assert_eq!(Preference::parse(""), Preference::Auto);
    assert_eq!(Preference::parse(" LSP "), Preference::Lsp);
    assert_eq!(Preference::parse("Off"), Preference::Off);
    assert_eq!(
        Preference::parse("Prettier"),
        Preference::Tool("prettier".to_owned())
    );
}

#[test]
fn defaults_to_auto_without_a_matching_override() {
    assert_eq!(resolve(&[], "/p/a.ts"), Preference::Auto);
    assert_eq!(resolve(&[("python", "off")], "/p/a.ts"), Preference::Auto);
}

#[test]
fn matches_a_language_identifier() {
    let entries = [("typescript", "lsp"), ("python", "black")];
    assert_eq!(resolve(&entries, "/p/a.ts"), Preference::Lsp);
    assert_eq!(resolve(&entries, "/p/a.mts"), Preference::Lsp);
    assert_eq!(
        resolve(&entries, "/p/a.py"),
        Preference::Tool("black".to_owned())
    );
    assert_eq!(resolve(&entries, "/p/a.tsx"), Preference::Auto);
}

#[test]
fn matches_an_extension_with_or_without_a_dot_in_any_case() {
    assert_eq!(resolve(&[("md", "off")], "/p/README.md"), Preference::Off);
    assert_eq!(resolve(&[(".md", "off")], "/p/README.MD"), Preference::Off);
    assert_eq!(
        resolve(&[(" .MD ", "off")], "/p/README.md"),
        Preference::Off
    );
}

#[test]
fn an_extension_beats_a_language_identifier_which_beats_the_wildcard() {
    let entries = [("*", "off"), ("javascript", "lsp"), ("mjs", "biome")];
    assert_eq!(
        resolve(&entries, "/p/a.mjs"),
        Preference::Tool("biome".to_owned())
    );
    assert_eq!(resolve(&entries, "/p/a.js"), Preference::Lsp);
    assert_eq!(resolve(&entries, "/p/a.css"), Preference::Off);
}

#[test]
fn the_longest_extension_wins() {
    let entries = [("php", "pint"), ("blade.php", "off")];
    assert_eq!(resolve(&entries, "/p/home.blade.php"), Preference::Off);
    assert_eq!(
        resolve(&entries, "/p/index.php"),
        Preference::Tool("pint".to_owned())
    );
}

#[test]
fn the_wildcard_covers_files_without_a_known_language() {
    assert_eq!(resolve(&[("*", "off")], "/p/main.rb"), Preference::Off);
    assert_eq!(resolve(&[("rb", "lsp")], "/p/main.rb"), Preference::Lsp);
}
