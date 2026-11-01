use std::fs;
use std::time::Duration;

use super::*;
use crate::test_support::{Project, UPPERCASE};

/// Like Prettier, echoes the input untouched when the file given by `--stdin-filepath` is listed
/// in the working directory's `.prettierignore`.
const IGNORE_AWARE: &str = "#!/bin/sh\nif [ -f .prettierignore ] && grep -qxF \"$(basename \"$2\")\" .prettierignore; then\n  cat\nelse\n  tr '[:lower:]' '[:upper:]'\nfi\n";

fn request(project: &Project, file: &str, text: &str) -> FormatRequest {
    FormatRequest {
        file: project.path(file),
        project_root: Some(project.root()),
        text: text.to_owned(),
        path_env: None,
        preference: Preference::Auto,
        limits: Limits::default(),
    }
}

fn prettier_project(script: &str) -> Project {
    let project = Project::new();
    project.write(".prettierrc", "{}");
    project.script("node_modules/.bin/prettier", script);
    project
}

#[test]
fn formats_with_the_project_formatter_and_reports_minimal_edits() {
    let project = prettier_project(UPPERCASE);
    let text = "KEEP\nchange me\nKEEP\n";

    let FormatOutcome::Formatted {
        tool,
        text: formatted,
        edits,
    } = format(&request(&project, "src/a.ts", text))
    else {
        panic!("expected the text to be formatted");
    };
    assert_eq!(tool, Tool::Prettier);
    assert_eq!(formatted, "KEEP\nCHANGE ME\nKEEP\n");
    assert_eq!(
        edits,
        [TextEdit {
            range: 5..14,
            new_text: "CHANGE ME".to_owned()
        }]
    );
    assert_eq!(apply_edits(text, &edits), formatted);
}

#[test]
fn already_formatted_text_is_reported_unchanged() {
    let project = prettier_project(UPPERCASE);
    assert_eq!(
        format(&request(&project, "src/a.ts", "ALREADY FORMATTED\n")),
        FormatOutcome::Unchanged {
            tool: Tool::Prettier
        }
    );
}

#[test]
fn a_file_the_formatter_ignores_is_left_unchanged() {
    let project = prettier_project(IGNORE_AWARE);
    project.write(".prettierignore", "generated.ts\n");

    assert_eq!(
        format(&request(&project, "src/generated.ts", "leave me\n")),
        FormatOutcome::Unchanged {
            tool: Tool::Prettier
        }
    );
    assert!(matches!(
        format(&request(&project, "src/a.ts", "format me\n")),
        FormatOutcome::Formatted { .. }
    ));
}

#[test]
fn a_failing_formatter_yields_a_failure_naming_the_tool() {
    let project =
        prettier_project("#!/bin/sh\ncat >/dev/null\necho 'SyntaxError: nope' >&2\nexit 2\n");

    let outcome = format(&request(&project, "src/a.ts", "let = ;\n"));
    let FormatOutcome::Failed(failure) = outcome else {
        panic!("expected a failure, got {outcome:?}");
    };
    assert_eq!(failure.tool, "prettier");
    assert_eq!(failure.message, "SyntaxError: nope");
    assert_eq!(failure.to_string(), "prettier: SyntaxError: nope");
}

#[test]
fn garbage_output_and_timeouts_are_failures() {
    let garbage = prettier_project("#!/bin/sh\ncat >/dev/null\nprintf '\\377\\376'\n");
    assert!(matches!(
        format(&request(&garbage, "a.ts", "x\n")),
        FormatOutcome::Failed(_)
    ));

    let hanging = prettier_project("#!/bin/sh\nexec sleep 30\n");
    let mut request = request(&hanging, "a.ts", "x\n");
    request.limits.timeout = Duration::from_millis(200);
    assert_eq!(
        format(&request),
        FormatOutcome::Failed(FormatFailure {
            tool: "prettier".to_owned(),
            message: "timed out after 200ms".to_owned()
        })
    );
}

#[test]
fn nothing_happens_without_a_project_formatter() {
    let project = Project::new();
    project.script("node_modules/.bin/prettier", UPPERCASE);
    assert_eq!(
        format(&request(&project, "src/a.ts", "x\n")),
        FormatOutcome::NoFormatter
    );
    assert_eq!(
        format(&request(&project, "notes.txt", "x\n")),
        FormatOutcome::NoFormatter
    );
}

#[test]
fn overrides_select_the_language_server_off_or_a_named_tool() {
    let project = prettier_project(UPPERCASE);
    let with = |preference| FormatRequest {
        preference,
        ..request(&project, "src/a.ts", "x\n")
    };

    assert_eq!(format(&with(Preference::Lsp)), FormatOutcome::NoFormatter);
    assert_eq!(format(&with(Preference::Off)), FormatOutcome::Disabled);
    assert_eq!(
        format(&with(Preference::Tool("biome".to_owned()))),
        FormatOutcome::Failed(FormatFailure {
            tool: "biome".to_owned(),
            message: "was not found in the project or on PATH".to_owned()
        })
    );
}

#[test]
fn the_global_path_reaches_discovery_and_the_formatter() {
    let project = Project::new();
    project.write("pyproject.toml", "[tool.ruff]\n");
    project.script(
        "global/bin/ruff",
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' \"$PATH\"\n",
    );
    let path_env = project.path_env(&["global/bin"]);
    let request = FormatRequest {
        path_env: Some(path_env.clone()),
        ..request(&project, "a.py", "x\n")
    };

    let FormatOutcome::Formatted { tool, text, .. } = format(&request) else {
        panic!("expected the text to be formatted");
    };
    assert_eq!(tool, Tool::Ruff);
    assert_eq!(text, format!("{path_env}\n"));
}

#[test]
fn an_in_place_tool_formats_the_buffer_without_touching_the_file() {
    let project = Project::new();
    let file = project.write("app/User.php", "<?php // saved\n");
    project.script(
        "vendor/bin/pint",
        "#!/bin/sh\nfor target; do :; done\ntr '[:lower:]' '[:upper:]' < \"$target\" > \"$target.tmp\" && mv \"$target.tmp\" \"$target\"\n",
    );

    let FormatOutcome::Formatted { tool, text, .. } =
        format(&request(&project, "app/User.php", "<?php // buffer\n"))
    else {
        panic!("expected the text to be formatted");
    };
    assert_eq!(tool, Tool::Pint);
    assert_eq!(text, "<?PHP // BUFFER\n");
    assert_eq!(fs::read_to_string(file).unwrap(), "<?php // saved\n");
}

#[test]
fn files_no_project_formatter_handles_are_ruled_out_by_name() {
    let tool = Preference::Tool("prettier".to_owned());
    assert!(may_apply(Path::new("/p/a.ts"), &Preference::Auto));
    assert!(!may_apply(Path::new("/p/a.rb"), &Preference::Auto));
    assert!(!may_apply(Path::new("/p/a.ts"), &Preference::Lsp));
    assert!(may_apply(Path::new("/p/a.rb"), &Preference::Off));
    assert!(may_apply(Path::new("/p/a.rb"), &tool));
}

#[test]
fn formats_on_a_background_thread() {
    let project = prettier_project(UPPERCASE);
    let outcome =
        futures_lite::future::block_on(format_in_background(request(&project, "a.ts", "x\n")));
    assert!(matches!(outcome, FormatOutcome::Formatted { text, .. } if text == "X\n"));
}

#[test]
fn a_formatter_selected_ahead_of_time_runs_in_the_background() {
    let project = prettier_project(UPPERCASE);
    let Selection::External(invocation) = select(
        &project.path("src/a.ts"),
        Some(&project.root()),
        None,
        &Preference::Auto,
    ) else {
        panic!("expected a project formatter");
    };

    let format = |text: &str| {
        futures_lite::future::block_on(format_with_in_background(
            invocation.clone(),
            text.to_owned(),
            None,
            Limits::default(),
        ))
    };
    assert!(matches!(
        format("keep\nchange\n"),
        FormatOutcome::Formatted { tool: Tool::Prettier, text, edits }
            if text == "KEEP\nCHANGE\n" && edits.len() == 1
    ));
    assert_eq!(
        format("DONE\n"),
        FormatOutcome::Unchanged {
            tool: Tool::Prettier
        }
    );
}

#[test]
fn a_failure_is_reported_once_per_file() {
    let reported = ReportedFailures::default();
    let (first, second) = (Path::new("/p/a.ts"), Path::new("/p/b.ts"));

    assert!(reported.record(first));
    assert!(!reported.record(first));
    assert!(reported.record(second));
    assert!(!reported.record(first));
}
