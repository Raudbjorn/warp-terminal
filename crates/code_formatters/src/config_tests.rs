use super::*;
use crate::test_support::Project;

fn scope(project: &Project, file: &str) -> SearchScope {
    SearchScope::new(&project.path(file), Some(&project.root()))
}

#[test]
fn every_prettier_config_file_name_is_recognized() {
    for name in PRETTIER_CONFIG_FILES {
        let project = Project::new();
        let config = project.write(name, "");
        assert_eq!(
            prettier_config(&scope(&project, "src/a.ts")),
            Some(config),
            "{name}"
        );
    }
}

#[test]
fn a_prettier_key_in_package_json_is_a_config() {
    let project = Project::new();
    let manifest = project.write(
        "package.json",
        r#"{"name": "app", "prettier": {"semi": false}}"#,
    );
    assert_eq!(
        prettier_config(&scope(&project, "src/a.ts")),
        Some(manifest)
    );
}

#[test]
fn a_package_json_without_a_prettier_key_is_not_a_config() {
    let project = Project::new();
    project.write(
        "package.json",
        r#"{"name": "prettier", "devDependencies": {"prettier": "^3.0.0"}}"#,
    );
    assert_eq!(prettier_config(&scope(&project, "src/a.ts")), None);
}

#[test]
fn the_nearest_prettier_config_wins() {
    let project = Project::new();
    project.write(".prettierrc", "{}");
    let nested = project.write("packages/app/prettier.config.mjs", "export default {};");
    assert_eq!(
        prettier_config(&scope(&project, "packages/app/src/a.ts")),
        Some(nested)
    );
}

#[test]
fn prettier_is_a_dependency_in_any_dependency_section() {
    for section in NODE_DEPENDENCY_SECTIONS {
        let project = Project::new();
        project.write(
            "package.json",
            &format!(r#"{{"{section}": {{"prettier": "3.3.0"}}}}"#),
        );
        assert!(
            node_dependency(&scope(&project, "src/a.ts"), &["prettier"]),
            "{section}"
        );
    }
}

#[test]
fn other_dependencies_and_broken_manifests_do_not_count() {
    let project = Project::new();
    project.write(
        "package.json",
        r#"{"dependencies": {"prettier-plugin-svelte": "1"}, "scripts": {"prettier": "x"}}"#,
    );
    project.write("broken/package.json", "{ not json");
    assert!(!node_dependency(
        &scope(&project, "src/a.ts"),
        &["prettier"]
    ));
    assert!(!node_dependency(
        &scope(&project, "broken/a.ts"),
        &["prettier"]
    ));
    assert_eq!(prettier_config(&scope(&project, "broken/a.ts")), None);
}

#[test]
fn a_plugin_is_available_when_declared_or_installed() {
    let plugin = ["prettier-plugin-svelte"];

    let declared = Project::new();
    declared.write(
        "package.json",
        r#"{"devDependencies": {"prettier-plugin-svelte": "3"}}"#,
    );
    assert!(node_package_available(
        &scope(&declared, "a.svelte"),
        &plugin
    ));

    let installed = Project::new();
    installed.write("node_modules/prettier-plugin-svelte/package.json", "{}");
    assert!(node_package_available(
        &scope(&installed, "src/a.svelte"),
        &plugin
    ));

    let neither = Project::new();
    neither.write("package.json", r#"{"devDependencies": {"prettier": "3"}}"#);
    assert!(!node_package_available(
        &scope(&neither, "a.svelte"),
        &plugin
    ));
}

#[test]
fn biome_and_dprint_configs() {
    let project = Project::new();
    assert_eq!(biome_config(&scope(&project, "a.ts")), None);
    assert_eq!(dprint_config(&scope(&project, "a.ts")), None);

    let biome = project.write("biome.jsonc", "{}");
    let dprint = project.write("dprint.json", "{}");
    assert_eq!(biome_config(&scope(&project, "src/a.ts")), Some(biome));
    assert_eq!(dprint_config(&scope(&project, "src/a.ts")), Some(dprint));
}

#[test]
fn ruff_config_files_and_pyproject_tables() {
    for name in ["ruff.toml", ".ruff.toml"] {
        let project = Project::new();
        let config = project.write(name, "line-length = 100\n");
        assert_eq!(ruff_config(&scope(&project, "pkg/a.py")), Some(config));
    }
    for table in ["[tool.ruff]", "[tool.ruff.format]", "[ tool.ruff.lint ]"] {
        let project = Project::new();
        let config = project.write(
            "pyproject.toml",
            &format!("[project]\nname = \"x\"\n\n{table}\nline-length = 100\n"),
        );
        assert_eq!(
            ruff_config(&scope(&project, "pkg/a.py")),
            Some(config),
            "{table}"
        );
    }
}

#[test]
fn a_pyproject_without_the_tool_table_is_not_its_config() {
    let project = Project::new();
    project.write(
        "pyproject.toml",
        "[project]\nname = \"x\"\ndependencies = [\"ruff\", \"black\"]\n\n[tool.ruffian]\nx = 1\n",
    );
    assert_eq!(ruff_config(&scope(&project, "a.py")), None);
    assert_eq!(black_config(&scope(&project, "a.py")), None);
}

#[test]
fn black_config_is_a_pyproject_table() {
    let project = Project::new();
    let config = project.write("pyproject.toml", "[tool.black]\nline-length = 100\n");
    assert_eq!(black_config(&scope(&project, "pkg/a.py")), Some(config));
    assert_eq!(ruff_config(&scope(&project, "pkg/a.py")), None);
}

#[test]
fn sqlfluff_config_in_each_supported_file() {
    let cases = [
        (".sqlfluff", "[sqlfluff]\ndialect = postgres\n"),
        (
            "pyproject.toml",
            "[tool.sqlfluff.core]\ndialect = \"postgres\"\n",
        ),
        (
            "setup.cfg",
            "[metadata]\nname = x\n\n[sqlfluff]\ndialect = postgres\n",
        ),
        ("tox.ini", "[sqlfluff:rules]\nmax_line_length = 100\n"),
    ];
    for (name, contents) in cases {
        let project = Project::new();
        let config = project.write(name, contents);
        assert_eq!(
            sqlfluff_config(&scope(&project, "db/q.sql")),
            Some(config),
            "{name}"
        );
    }

    let project = Project::new();
    project.write("setup.cfg", "[metadata]\nname = x\n");
    assert_eq!(sqlfluff_config(&scope(&project, "db/q.sql")), None);
}

#[test]
fn pg_format_taplo_and_blade_formatter_configs() {
    let project = Project::new();
    let pg_format = project.write(".pg_format", "spaces=2\n");
    let taplo = project.write(".taplo.toml", "");
    let blade = project.write(".bladeformatterrc.json", "{}");
    let scope = scope(&project, "a/b.sql");

    assert_eq!(pg_format_config(&scope), Some(pg_format));
    assert_eq!(taplo_config(&scope), Some(taplo));
    assert_eq!(blade_formatter_config(&scope), Some(blade));
}

#[test]
fn rust_edition_from_the_nearest_manifest() {
    let project = Project::new();
    project.write(
        "Cargo.toml",
        "[package]\nname = \"outer\"\nedition = \"2018\"\n",
    );
    project.write(
        "crates/inner/Cargo.toml",
        "[package]\nname = \"inner\"\nedition = \"2024\" # newest\n\n[dependencies]\n",
    );
    assert_eq!(
        rust_edition(&scope(&project, "crates/inner/src/lib.rs")),
        Some("2024".to_owned())
    );
    assert_eq!(
        rust_edition(&scope(&project, "src/main.rs")),
        Some("2018".to_owned())
    );
}

#[test]
fn rust_edition_inherited_from_the_workspace() {
    for inherit in ["edition.workspace = true", "edition = { workspace = true }"] {
        let project = Project::new();
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nedition = \"2024\"\n",
        );
        project.write(
            "crates/inner/Cargo.toml",
            &format!("[package]\nname = \"inner\"\n{inherit}\n"),
        );
        assert_eq!(
            rust_edition(&scope(&project, "crates/inner/src/lib.rs")),
            Some("2024".to_owned()),
            "{inherit}"
        );
    }
}

#[test]
fn rust_edition_is_unknown_without_a_declaration() {
    let project = Project::new();
    assert_eq!(rust_edition(&scope(&project, "src/lib.rs")), None);

    project.write(
        "Cargo.toml",
        "[workspace.package]\nedition = \"2024\"\n\n[workspace]\nmembers = [\"member\"]\n",
    );
    project.write("member/Cargo.toml", "[package]\nname = \"member\"\n");
    assert_eq!(rust_edition(&scope(&project, "member/src/lib.rs")), None);
}

#[test]
fn a_virtual_manifest_supplies_the_workspace_edition() {
    let project = Project::new();
    project.write(
        "Cargo.toml",
        "[workspace]\nmembers = []\n\n[workspace.package]\nedition = \"2021\"\n",
    );
    assert_eq!(
        rust_edition(&scope(&project, "tests/it.rs")),
        Some("2021".to_owned())
    );
}
