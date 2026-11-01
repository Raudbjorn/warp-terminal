use std::path::Path;

use super::*;

fn language(file: &str) -> Option<Language> {
    Language::from_path(Path::new(file))
}

#[test]
fn maps_extensions_to_languages() {
    let cases = [
        ("a.ts", Language::TypeScript),
        ("a.mts", Language::TypeScript),
        ("a.cts", Language::TypeScript),
        ("a.d.ts", Language::TypeScript),
        ("a.tsx", Language::TypeScriptReact),
        ("a.js", Language::JavaScript),
        ("a.mjs", Language::JavaScript),
        ("a.cjs", Language::JavaScript),
        ("a.jsx", Language::JavaScriptReact),
        ("package.json", Language::Json),
        ("tsconfig.jsonc", Language::Jsonc),
        ("a.css", Language::Css),
        ("a.scss", Language::Scss),
        ("a.less", Language::Less),
        ("index.html", Language::Html),
        ("App.vue", Language::Vue),
        ("App.svelte", Language::Svelte),
        ("README.md", Language::Markdown),
        ("page.mdx", Language::Mdx),
        ("ci.yaml", Language::Yaml),
        ("ci.yml", Language::Yaml),
        ("schema.graphql", Language::GraphQl),
        ("main.py", Language::Python),
        ("stubs.pyi", Language::Python),
        ("main.tf", Language::Terraform),
        ("prod.tfvars", Language::Terraform),
        ("build.sh", Language::Shell),
        ("Cargo.toml", Language::Toml),
        ("main.go", Language::Go),
        ("lib.rs", Language::Rust),
        ("index.php", Language::Php),
        ("query.sql", Language::Sql),
    ];
    for (file, expected) in cases {
        assert_eq!(language(file), Some(expected), "{file}");
    }
}

#[test]
fn extension_matching_ignores_case() {
    assert_eq!(language("/p/README.MD"), Some(Language::Markdown));
}

#[test]
fn blade_templates_are_not_plain_php() {
    assert_eq!(
        language("/p/resources/views/home.blade.php"),
        Some(Language::Blade)
    );
}

#[test]
fn unknown_and_extensionless_files_have_no_language() {
    assert_eq!(language("/p/Makefile"), None);
    assert_eq!(language("/p/main.rb"), None);
    assert_eq!(language("/p/zshrc.zsh"), None);
}

#[test]
fn tool_names_round_trip() {
    for tool in Tool::ALL {
        assert_eq!(Tool::from_name(tool.name()), Some(tool));
    }
    assert_eq!(Tool::from_name(" Prettier "), Some(Tool::Prettier));
    assert_eq!(Tool::from_name("eslint"), None);
}

#[test]
fn prettier_is_preferred_for_web_languages() {
    for language in [
        Language::TypeScript,
        Language::TypeScriptReact,
        Language::JavaScript,
        Language::Json,
        Language::Css,
        Language::Markdown,
        Language::Yaml,
    ] {
        assert_eq!(language.candidates().first(), Some(&Tool::Prettier));
    }
    assert_eq!(Language::Python.candidates(), [Tool::Ruff, Tool::Black]);
}
