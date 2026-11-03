use std::path::Path;

use arborium::tree_sitter::{Parser, QueryCursor, StreamingIterator};

use warp_util::standardized_path::StandardizedPath;

use crate::{
    SUPPORTED_LANGUAGES, language_by_filename, language_by_local_filename, language_by_name,
    load_language,
};

/// Validate that every supported language can be loaded successfully.
/// This catches invalid node types, syntax errors, and other issues in .scm query files
/// (highlights, indents, identifiers) that would otherwise only surface at runtime.
#[test]
fn all_supported_languages_load_successfully() {
    let failures: Vec<_> = SUPPORTED_LANGUAGES
        .iter()
        .filter(|lang| load_language(lang).is_none())
        .collect();

    assert!(
        failures.is_empty(),
        "The following languages failed to load:\n{}",
        failures
            .iter()
            .map(|lang| format!("  - {lang}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Both `.html` and the legacy three-character `.htm` extension should resolve to
/// the same HTML language entry. `.htm` is widely produced by static-site generators
/// and historical web tooling (DOS 8.3 filename limits) and is already treated as
/// an HTML/text file elsewhere in the codebase
/// (see `is_development_text_extension` in `crates/warp_util/src/file_type.rs`).
#[test]
fn html_extensions_resolve_to_html() {
    for filename in ["index.html", "index.htm"] {
        let path = StandardizedPath::try_new(&format!("/tmp/{filename}"))
            .expect("test path should be absolute");
        let language = language_by_filename(&path)
            .unwrap_or_else(|| panic!("expected {filename} to resolve to a language"));
        assert_eq!(
            language.display_name(),
            "HTML",
            "{filename} should resolve to HTML",
        );
    }
}

#[test]
fn local_html_extensions_resolve_to_html() {
    for filename in ["index.html", "index.htm"] {
        let path = Path::new(filename);
        let language = language_by_local_filename(path)
            .unwrap_or_else(|| panic!("expected {filename} to resolve to a language"));
        assert_eq!(
            language.display_name(),
            "HTML",
            "{filename} should resolve to HTML",
        );
    }
}

/// `.command` is the macOS convention for double-clickable shell scripts.
/// Make sure `language_by_filename` recognizes it as shell so the editor
/// renders syntax highlighting instead of the
/// "Language support is unavailable for this file type" footer.
#[test]
fn command_extension_resolves_to_shell() {
    let path =
        StandardizedPath::try_new("/tmp/script.command").expect("test path should be absolute");
    let language =
        language_by_filename(&path).expect("`.command` files should resolve to a language");
    assert_eq!(language.display_name(), "Shell");
}

#[test]
fn local_command_extension_resolves_to_shell() {
    let language = language_by_local_filename(Path::new("script.command"))
        .expect("`.command` files should resolve to a language");
    assert_eq!(language.display_name(), "Shell");
}

/// `.md` and `.markdown` should resolve to the Markdown language so the editor applies
/// syntax highlighting to Markdown source files.
#[test]
fn markdown_extensions_resolve_to_markdown() {
    for filename in ["README.md", "notes.markdown"] {
        let path = StandardizedPath::try_new(&format!("/tmp/{filename}"))
            .expect("test path should be absolute");
        let language = language_by_filename(&path)
            .unwrap_or_else(|| panic!("expected {filename} to resolve to a language"));
        assert_eq!(
            language.display_name(),
            "Markdown",
            "{filename} should resolve to Markdown",
        );
    }
}

/// Parses `source` and returns `(capture name, captured text)` for every highlight capture.
fn highlights(language_name: &str, source: &str) -> Vec<(String, String)> {
    let language = language_by_name(language_name)
        .unwrap_or_else(|| panic!("{language_name} should be a supported language"));
    let mut parser = Parser::new();
    parser
        .set_language(&language.grammar)
        .expect("grammar should be compatible with the parser");
    let tree = parser.parse(source, None).expect("source should parse");

    let capture_names = language.highlight_query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(
        &language.highlight_query,
        tree.root_node(),
        source.as_bytes(),
    );
    let mut highlights = Vec::new();
    while let Some((query_match, capture_index)) = captures.next() {
        let capture = query_match.captures[*capture_index];
        highlights.push((
            capture_names[capture.index as usize].to_string(),
            source[capture.node.byte_range()].to_string(),
        ));
    }
    highlights
}

fn assert_highlights(language_name: &str, source: &str, expected: &[(&str, &str)]) {
    let actual = highlights(language_name, source);
    for (name, text) in expected {
        assert!(
            actual
                .iter()
                .any(|(actual_name, actual_text)| actual_name == name && actual_text == text),
            "expected {text:?} to be highlighted as {name} in {language_name}, got {actual:#?}",
        );
    }
}

#[test]
fn sibling_language_extensions_resolve_through_both_lookups() {
    for (filename, expected) in [
        ("theme.scss", "SCSS"),
        ("App.svelte", "Svelte"),
        ("report.typ", "Typst"),
        ("schema.prisma", "Prisma"),
    ] {
        let path = StandardizedPath::try_new(&format!("/tmp/{filename}")).unwrap();
        assert_eq!(
            language_by_filename(&path).unwrap().display_name(),
            expected
        );
        assert_eq!(
            language_by_local_filename(Path::new(filename))
                .unwrap()
                .display_name(),
            expected
        );
    }
}

#[test]
fn scss_highlights_rules_and_sass_directives() {
    let source = "\
// brand
$primary: #673ab7;
@mixin rounded($radius: 4px) {
  border-radius: $radius;
}
.card {
  @include rounded(8px);
}
";
    assert_highlights(
        "scss",
        source,
        &[
            ("comment", "// brand"),
            ("property", "$primary"),
            ("keyword", "@mixin"),
            ("function", "rounded"),
            ("property", "border-radius"),
            ("keyword.import", "@include"),
            ("number", "8px"),
        ],
    );
}

#[test]
fn svelte_highlights_markup_and_logic_blocks() {
    let source = "\
<main class=\"app\">
  {#if count > 0}
    <p>Hello</p>
  {/if}
</main>
";
    assert_highlights(
        "svelte",
        source,
        &[
            ("tag", "main"),
            ("attribute", "class"),
            ("string", "app"),
            ("keyword.conditional", "if"),
        ],
    );
}

#[test]
fn typst_highlights_markup_and_code() {
    let source = "\
#set page(paper: \"a4\", margin: 2cm)
= Introduction
// note
";
    assert_highlights(
        "typst",
        source,
        &[
            ("keyword.control", "set"),
            ("function", "page"),
            ("string", "\"a4\""),
            ("constant.numeric", "2cm"),
            ("markup.heading.1", "= Introduction"),
            ("comment", "// note"),
        ],
    );
}

#[test]
fn prisma_highlights_models_fields_and_attributes() {
    let source = "\
// schema
model User {
  id    Int    @id @default(autoincrement())
  email String @unique
  @@map(\"users\")
}
";
    assert_highlights(
        "prisma",
        source,
        &[
            ("comment", "// schema"),
            ("keyword", "model"),
            ("type", "User"),
            ("type", "Int"),
            ("property", "id"),
            ("property", "default"),
            ("function", "autoincrement"),
            ("property", "@@"),
            ("string", "\"users\""),
        ],
    );
}
