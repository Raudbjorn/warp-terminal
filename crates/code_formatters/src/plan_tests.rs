use super::*;
use crate::test_support::{Project, UPPERCASE};

const GLOBAL_BIN: &str = "global/bin";

/// Installs stand-in executables in a directory that is on the injected `PATH`.
fn install_globally(project: &Project, tools: &[&str]) -> String {
    project.mkdir(GLOBAL_BIN);
    for tool in tools {
        project.script(&format!("{GLOBAL_BIN}/{tool}"), UPPERCASE);
    }
    project.path_env(&[GLOBAL_BIN])
}

fn select_with(
    project: &Project,
    file: &str,
    path_env: Option<&str>,
    preference: &Preference,
) -> Selection {
    select(
        &project.path(file),
        Some(&project.root()),
        path_env,
        preference,
    )
}

fn auto(project: &Project, file: &str, path_env: Option<&str>) -> Selection {
    select_with(project, file, path_env, &Preference::Auto)
}

#[track_caller]
fn invocation(selection: Selection) -> Invocation {
    match selection {
        Selection::External(invocation) => invocation,
        other => panic!("expected a project formatter, got {other:?}"),
    }
}

/// The command line with `<copy>` standing in for the temporary copy.
fn args(invocation: &Invocation) -> Vec<String> {
    invocation
        .rendered_args(Path::new("<copy>"))
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

#[test]
fn prettier_from_the_nearest_node_modules() {
    let project = Project::new();
    project.write(".prettierrc", "{}");
    project.script("node_modules/.bin/prettier", UPPERCASE);
    let nested = project.script("packages/app/node_modules/.bin/prettier", UPPERCASE);
    let file = project.path("packages/app/src/a.ts");

    let invocation = invocation(auto(&project, "packages/app/src/a.ts", None));
    assert_eq!(invocation.tool, Tool::Prettier);
    assert_eq!(invocation.program, nested);
    assert_eq!(args(&invocation), ["--stdin-filepath", &display(&file)]);
    assert_eq!(invocation.cwd, project.root());
    assert!(!invocation.uses_temp_copy());
}

#[test]
fn prettier_falls_back_to_a_global_install_on_the_injected_path() {
    let project = Project::new();
    project.write("package.json", r#"{"prettier": {}}"#);
    let path_env = install_globally(&project, &["prettier"]);

    let invocation = invocation(auto(&project, "src/a.tsx", Some(&path_env)));
    assert_eq!(invocation.program, project.path("global/bin/prettier"));
    assert_eq!(auto(&project, "src/a.tsx", None), Selection::LanguageServer);
}

#[test]
fn prettier_needs_the_project_to_opt_in() {
    let project = Project::new();
    project.script("node_modules/.bin/prettier", UPPERCASE);
    assert_eq!(auto(&project, "a.ts", None), Selection::LanguageServer);

    project.write("package.json", r#"{"devDependencies": {"prettier": "^3"}}"#);
    assert_eq!(
        invocation(auto(&project, "a.ts", None)).tool,
        Tool::Prettier
    );
}

#[test]
fn a_config_without_an_executable_changes_nothing() {
    let project = Project::new();
    project.write(".prettierrc", "{}");
    project.write("biome.json", "{}");
    project.write("pyproject.toml", "[tool.ruff]\n");
    let path_env = install_globally(&project, &[]);

    for file in ["a.ts", "a.json", "a.py"] {
        assert_eq!(
            auto(&project, file, Some(&path_env)),
            Selection::LanguageServer,
            "{file}"
        );
    }
}

#[test]
fn prettier_runs_from_the_directory_of_its_ignore_file() {
    let project = Project::new();
    project.write(".prettierrc", "{}");
    project.script("node_modules/.bin/prettier", UPPERCASE);
    project.write("packages/app/.prettierignore", "generated\n");

    let invocation = invocation(auto(&project, "packages/app/src/a.ts", None));
    assert_eq!(invocation.cwd, project.path("packages/app"));
}

#[test]
fn prettier_and_biome_precedence() {
    let both = Project::new();
    both.write(".prettierrc", "{}");
    both.write("biome.json", "{}");
    both.script("node_modules/.bin/prettier", UPPERCASE);
    both.script("node_modules/.bin/biome", UPPERCASE);
    assert_eq!(invocation(auto(&both, "a.ts", None)).tool, Tool::Prettier);

    let biome_only = Project::new();
    biome_only.write("biome.json", "{}");
    biome_only.write("package.json", r#"{"devDependencies": {"prettier": "3"}}"#);
    biome_only.script("node_modules/.bin/prettier", UPPERCASE);
    biome_only.script("node_modules/.bin/biome", UPPERCASE);
    assert_eq!(
        invocation(auto(&biome_only, "a.ts", None)).tool,
        Tool::Biome
    );

    let biome_not_installed = Project::new();
    biome_not_installed.write("biome.json", "{}");
    biome_not_installed.write("package.json", r#"{"devDependencies": {"prettier": "3"}}"#);
    biome_not_installed.script("node_modules/.bin/prettier", UPPERCASE);
    assert_eq!(
        invocation(auto(&biome_not_installed, "a.ts", None)).tool,
        Tool::Prettier
    );
}

#[test]
fn biome_command_line() {
    let project = Project::new();
    let config_dir = project.mkdir("web");
    project.write("web/biome.jsonc", "{}");
    let program = project.script("node_modules/.bin/biome", UPPERCASE);
    let file = project.path("web/src/a.ts");

    let invocation = invocation(auto(&project, "web/src/a.ts", None));
    assert_eq!(invocation.program, program);
    assert_eq!(
        args(&invocation),
        ["format", &format!("--stdin-file-path={}", file.display())]
    );
    assert_eq!(invocation.cwd, config_dir);
}

#[test]
fn biome_does_not_take_languages_it_cannot_format() {
    let project = Project::new();
    project.write("biome.json", "{}");
    project.script("node_modules/.bin/biome", UPPERCASE);
    assert_eq!(auto(&project, "README.md", None), Selection::LanguageServer);
    assert_eq!(auto(&project, "ci.yaml", None), Selection::LanguageServer);
}

#[test]
fn dprint_needs_its_config_and_yields_to_a_configured_prettier() {
    let project = Project::new();
    let path_env = install_globally(&project, &["dprint"]);
    assert_eq!(
        auto(&project, "a.ts", Some(&path_env)),
        Selection::LanguageServer
    );

    project.write("dprint.json", "{}");
    let file = project.path("a.ts");
    let invocation = invocation(auto(&project, "a.ts", Some(&path_env)));
    assert_eq!(invocation.tool, Tool::Dprint);
    assert_eq!(args(&invocation), ["fmt", "--stdin", &display(&file)]);

    project.write(".prettierrc", "{}");
    project.script("node_modules/.bin/prettier", UPPERCASE);
    assert_eq!(
        self::invocation(auto(&project, "a.ts", Some(&path_env))).tool,
        Tool::Prettier
    );
}

#[test]
fn svelte_is_skipped_unless_the_project_has_the_prettier_plugin() {
    let project = Project::new();
    project.write(".prettierrc", "{}");
    project.script("node_modules/.bin/prettier", UPPERCASE);
    assert_eq!(
        auto(&project, "App.svelte", None),
        Selection::LanguageServer
    );
    assert_eq!(
        invocation(auto(&project, "App.vue", None)).tool,
        Tool::Prettier
    );

    project.write("node_modules/prettier-plugin-svelte/package.json", "{}");
    assert_eq!(
        invocation(auto(&project, "App.svelte", None)).tool,
        Tool::Prettier
    );
}

#[test]
fn ruff_with_and_without_its_config() {
    let project = Project::new();
    let path_env = install_globally(&project, &["ruff", "black"]);
    let file = project.path("pkg/a.py");

    assert_eq!(
        auto(&project, "pkg/a.py", Some(&path_env)),
        Selection::LanguageServer
    );

    project.write("pkg/ruff.toml", "");
    let configured = invocation(auto(&project, "pkg/a.py", Some(&path_env)));
    assert_eq!(configured.tool, Tool::Ruff);
    assert_eq!(configured.program, project.path("global/bin/ruff"));
    assert_eq!(
        args(&configured),
        ["format", "--stdin-filename", &display(&file), "-"]
    );
    assert_eq!(configured.cwd, project.path("pkg"));
}

#[test]
fn black_is_used_when_the_project_configures_it_and_not_ruff() {
    let project = Project::new();
    project.write("pyproject.toml", "[tool.black]\nline-length = 100\n");
    let path_env = install_globally(&project, &["ruff", "black"]);
    let file = project.path("a.py");

    let invocation = invocation(auto(&project, "a.py", Some(&path_env)));
    assert_eq!(invocation.tool, Tool::Black);
    assert_eq!(
        args(&invocation),
        ["--quiet", "--stdin-filename", &display(&file), "-"]
    );

    project.write("ruff.toml", "");
    assert_eq!(
        self::invocation(auto(&project, "a.py", Some(&path_env))).tool,
        Tool::Ruff
    );
}

#[test]
fn a_python_formatter_that_is_only_installed_globally_is_not_used() {
    let project = Project::new();
    let path_env = install_globally(&project, &["black"]);
    assert_eq!(
        auto(&project, "a.py", Some(&path_env)),
        Selection::LanguageServer
    );

    let black = Preference::Tool("black".to_owned());
    assert_eq!(
        invocation(select_with(&project, "a.py", Some(&path_env), &black)).tool,
        Tool::Black
    );
}

#[test]
fn a_python_formatter_in_the_project_virtualenv_needs_no_config() {
    let project = Project::new();
    let local = project.script(".venv/bin/black", UPPERCASE);
    let invocation = invocation(auto(&project, "a.py", None));
    assert_eq!(invocation.tool, Tool::Black);
    assert_eq!(invocation.program, local);
}

#[test]
fn a_project_virtualenv_beats_the_global_install() {
    let project = Project::new();
    let path_env = install_globally(&project, &["ruff"]);
    let local = project.script(".venv/bin/ruff", UPPERCASE);

    let invocation = invocation(auto(&project, "src/pkg/a.py", Some(&path_env)));
    assert_eq!(invocation.program, local);
}

#[test]
fn terraform_shfmt_taplo_and_gofmt_command_lines() {
    let project = Project::new();
    let path_env = install_globally(&project, &["terraform", "shfmt", "taplo", "gofmt"]);

    let terraform = invocation(auto(&project, "infra/main.tf", Some(&path_env)));
    assert_eq!(terraform.tool, Tool::Terraform);
    assert_eq!(args(&terraform), ["fmt", "-no-color", "-"]);
    assert_eq!(terraform.cwd, project.path("infra"));
    assert_eq!(
        invocation(auto(&project, "infra/prod.tfvars", Some(&path_env))).tool,
        Tool::Terraform
    );

    let script = project.path("scripts/build.sh");
    let shfmt = invocation(auto(&project, "scripts/build.sh", Some(&path_env)));
    assert_eq!(shfmt.tool, Tool::Shfmt);
    assert_eq!(args(&shfmt), ["--filename", &display(&script)]);
    assert_eq!(shfmt.cwd, project.root());

    let manifest = project.path("Cargo.toml");
    let taplo = invocation(auto(&project, "Cargo.toml", Some(&path_env)));
    assert_eq!(taplo.tool, Tool::Taplo);
    assert_eq!(
        args(&taplo),
        ["format", "--stdin-filepath", &display(&manifest), "-"]
    );

    let gofmt = invocation(auto(&project, "cmd/main.go", Some(&path_env)));
    assert_eq!(gofmt.tool, Tool::Gofmt);
    assert_eq!(args(&gofmt), Vec::<String>::new());
}

#[test]
fn rustfmt_takes_the_edition_from_the_manifest() {
    let project = Project::new();
    let path_env = install_globally(&project, &["rustfmt"]);

    let default = invocation(auto(&project, "src/lib.rs", Some(&path_env)));
    assert_eq!(args(&default), ["--edition", "2021", "--emit", "stdout"]);

    project.write(
        "Cargo.toml",
        "[package]\nname = \"x\"\nedition = \"2024\"\n",
    );
    let declared = invocation(auto(&project, "src/lib.rs", Some(&path_env)));
    assert_eq!(args(&declared), ["--edition", "2024", "--emit", "stdout"]);
    assert_eq!(declared.cwd, project.path("src"));
}

#[test]
fn pint_runs_on_a_temporary_copy_with_the_project_config() {
    let project = Project::new();
    let program = project.script("vendor/bin/pint", UPPERCASE);
    let config = project.write("pint.json", "{}");

    let invocation = invocation(auto(&project, "app/Models/User.php", None));
    assert_eq!(invocation.tool, Tool::Pint);
    assert_eq!(invocation.program, program);
    assert!(invocation.uses_temp_copy());
    assert_eq!(
        args(&invocation),
        ["--quiet", "--config", &display(&config), "<copy>"]
    );
    assert_eq!(invocation.cwd, project.root());
}

#[test]
fn php_cs_fixer_command_line() {
    let project = Project::new();
    project.script("vendor/bin/php-cs-fixer", UPPERCASE);
    let config = project.write(".php-cs-fixer.dist.php", "<?php\n");

    let invocation = invocation(auto(&project, "src/A.php", None));
    assert_eq!(invocation.tool, Tool::PhpCsFixer);
    assert_eq!(
        args(&invocation),
        [
            "fix",
            "--using-cache=no",
            "--quiet",
            "--no-interaction",
            &format!("--config={}", config.display()),
            "<copy>"
        ]
    );
}

#[test]
fn the_configured_php_tool_wins_when_both_are_installed() {
    let project = Project::new();
    project.script("vendor/bin/pint", UPPERCASE);
    project.script("vendor/bin/php-cs-fixer", UPPERCASE);
    assert_eq!(invocation(auto(&project, "a.php", None)).tool, Tool::Pint);

    project.write(".php-cs-fixer.php", "<?php\n");
    assert_eq!(
        invocation(auto(&project, "a.php", None)).tool,
        Tool::PhpCsFixer
    );
}

#[test]
fn php_tools_are_never_taken_from_the_global_path() {
    let project = Project::new();
    let path_env = install_globally(&project, &["pint", "php-cs-fixer", "blade-formatter"]);
    assert_eq!(
        auto(&project, "a.php", Some(&path_env)),
        Selection::LanguageServer
    );
    assert_eq!(
        auto(&project, "home.blade.php", Some(&path_env)),
        Selection::LanguageServer
    );
}

#[test]
fn blade_templates_use_blade_formatter_not_pint() {
    let project = Project::new();
    project.script("vendor/bin/pint", UPPERCASE);
    assert_eq!(
        auto(&project, "resources/views/home.blade.php", None),
        Selection::LanguageServer
    );

    project.script("node_modules/.bin/blade-formatter", UPPERCASE);
    let invocation = invocation(auto(&project, "resources/views/home.blade.php", None));
    assert_eq!(invocation.tool, Tool::BladeFormatter);
    assert_eq!(args(&invocation), ["--stdin"]);
}

#[test]
fn sql_formatters_need_a_project_config() {
    let project = Project::new();
    let path_env = install_globally(&project, &["sqlfluff", "pg_format"]);
    assert_eq!(
        auto(&project, "db/q.sql", Some(&path_env)),
        Selection::LanguageServer
    );

    let pg_config = project.write(".pg_format", "spaces=2\n");
    let pg_format = invocation(auto(&project, "db/q.sql", Some(&path_env)));
    assert_eq!(pg_format.tool, Tool::PgFormat);
    assert_eq!(args(&pg_format), ["--config", &display(&pg_config), "-"]);

    project.write(".sqlfluff", "[sqlfluff]\ndialect = postgres\n");
    let file = project.path("db/q.sql");
    let sqlfluff = invocation(auto(&project, "db/q.sql", Some(&path_env)));
    assert_eq!(sqlfluff.tool, Tool::Sqlfluff);
    assert_eq!(
        args(&sqlfluff),
        [
            "format",
            "--nocolor",
            "--stdin-filename",
            &display(&file),
            "-"
        ]
    );
}

#[test]
fn files_of_an_unknown_type_are_left_to_the_language_server() {
    let project = Project::new();
    project.write(".prettierrc", "{}");
    project.script("node_modules/.bin/prettier", UPPERCASE);
    assert_eq!(auto(&project, "main.rb", None), Selection::LanguageServer);
    assert_eq!(auto(&project, "Makefile", None), Selection::LanguageServer);
}

#[test]
fn lsp_and_off_overrides_bypass_project_formatters() {
    let project = Project::new();
    project.write(".prettierrc", "{}");
    project.script("node_modules/.bin/prettier", UPPERCASE);

    assert_eq!(
        select_with(&project, "a.ts", None, &Preference::Lsp),
        Selection::LanguageServer
    );
    assert_eq!(
        select_with(&project, "a.ts", None, &Preference::Off),
        Selection::Off
    );
    assert_eq!(
        select_with(&project, "main.rb", None, &Preference::Off),
        Selection::Off
    );
}

#[test]
fn a_named_tool_is_used_without_the_project_opting_in() {
    let project = Project::new();
    let path_env = install_globally(&project, &["prettier", "biome"]);

    let biome = Preference::Tool("biome".to_owned());
    assert_eq!(
        invocation(select_with(&project, "a.ts", Some(&path_env), &biome)).tool,
        Tool::Biome
    );
    let prettier = Preference::Tool("prettier".to_owned());
    assert_eq!(
        invocation(select_with(
            &project,
            "App.svelte",
            Some(&path_env),
            &prettier
        ))
        .tool,
        Tool::Prettier
    );
}

#[test]
fn a_named_tool_overrides_the_automatic_choice() {
    let project = Project::new();
    project.write("ruff.toml", "");
    let path_env = install_globally(&project, &["ruff", "black"]);

    let black = Preference::Tool("black".to_owned());
    assert_eq!(
        invocation(select_with(&project, "a.py", Some(&path_env), &black)).tool,
        Tool::Black
    );
}

#[test]
fn a_named_tool_that_cannot_be_used_is_reported() {
    let project = Project::new();
    let path_env = install_globally(&project, &["gofmt"]);
    let unavailable = |file: &str, tool: &str| match select_with(
        &project,
        file,
        Some(&path_env),
        &Preference::Tool(tool.to_owned()),
    ) {
        Selection::Unavailable { tool, reason } => (tool, reason),
        other => panic!("expected the tool to be unavailable, got {other:?}"),
    };

    assert_eq!(
        unavailable("a.ts", "prettier"),
        (
            "prettier".to_owned(),
            "was not found in the project or on PATH".to_owned()
        )
    );
    assert_eq!(
        unavailable("a.ts", "eslint"),
        ("eslint".to_owned(), "is not a known formatter".to_owned())
    );
    assert_eq!(
        unavailable("a.ts", "gofmt"),
        (
            "gofmt".to_owned(),
            "does not format this type of file".to_owned()
        )
    );
}
