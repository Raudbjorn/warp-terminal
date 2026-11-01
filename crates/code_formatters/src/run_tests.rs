use std::path::PathBuf;

use super::*;
use crate::plan::Arg;
use crate::registry::Tool;
use crate::test_support::{Project, UPPERCASE};

/// An invocation of a stand-in formatter script, run from the project root.
fn invocation(project: &Project, script: &str, args: Vec<Arg>) -> Invocation {
    Invocation {
        tool: Tool::Prettier,
        program: project.script("bin/formatter", script),
        args,
        cwd: project.root(),
        file: project.path("src/input.txt"),
    }
}

fn format(project: &Project, script: &str, input: &str) -> Result<String, RunError> {
    run(
        &invocation(project, script, Vec::new()),
        input,
        None,
        &Limits::default(),
    )
}

fn short_timeout() -> Limits {
    Limits {
        timeout: Duration::from_millis(300),
        ..Limits::default()
    }
}

fn process_is_gone(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == -1 }
}

fn read_pid(path: PathBuf) -> i32 {
    fs::read_to_string(path)
        .expect("pid file")
        .trim()
        .parse()
        .expect("pid")
}

#[test]
fn a_configured_timeout_is_kept_within_bounds() {
    let limits = |millis| Limits::with_timeout(Duration::from_millis(millis));
    assert_eq!(limits(2_500).timeout, Duration::from_millis(2_500));
    assert_eq!(limits(0).timeout, Duration::from_millis(100));
    assert_eq!(limits(3_600_000).timeout, Duration::from_secs(60));
    assert_eq!(
        limits(2_500).max_input_bytes,
        Limits::default().max_input_bytes
    );
}

#[test]
fn pipes_the_buffer_through_stdin_and_stdout() {
    let project = Project::new();
    assert_eq!(
        format(&project, UPPERCASE, "let a = 1;\n").unwrap(),
        "LET A = 1;\n"
    );
}

#[test]
fn passes_arguments_working_directory_and_path() {
    let project = Project::new();
    let cwd = project.mkdir("config/dir");
    let script =
        "#!/bin/sh\ncat >/dev/null\nprintf '%s|%s|%s|%s\\n' \"$1\" \"$2\" \"$PWD\" \"$PATH\"\n";
    let mut invocation = invocation(
        &project,
        script,
        vec!["--stdin-filepath".into(), "a b.ts".into()],
    );
    invocation.cwd = cwd.clone();

    let output = run(
        &invocation,
        "x\n",
        Some("/usr/bin:/bin:/custom/bin"),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(
        output,
        format!(
            "--stdin-filepath|a b.ts|{}|/usr/bin:/bin:/custom/bin\n",
            cwd.display()
        )
    );
}

#[test]
fn large_buffers_round_trip_without_deadlocking() {
    let project = Project::new();
    let input = "the quick brown fox jumps over the lazy dog\n".repeat(20_000);
    assert_eq!(format(&project, "#!/bin/sh\ncat\n", &input).unwrap(), input);
}

#[test]
fn a_failing_formatter_reports_the_first_line_of_stderr() {
    let project = Project::new();
    let script = "#!/bin/sh\ncat >/dev/null\nprintf '\\n\\033[31m[error]\\033[39m src/a.ts: SyntaxError: Unexpected token (1:5)\\n  1 | let = ;\\n' >&2\nprintf 'partial output'\nexit 2\n";
    let error = format(&project, script, "let = ;\n").unwrap_err();

    assert!(matches!(error, RunError::Exited { .. }), "{error:?}");
    assert_eq!(
        error.to_string(),
        "[error] src/a.ts: SyntaxError: Unexpected token (1:5)"
    );
}

#[test]
fn a_failure_without_stderr_falls_back_to_stdout_then_the_exit_status() {
    let project = Project::new();
    let on_stdout = format(
        &project,
        "#!/bin/sh\ncat >/dev/null\necho 'error: bad config'\nexit 1\n",
        "x\n",
    )
    .unwrap_err();
    assert_eq!(on_stdout.to_string(), "error: bad config");

    let silent = format(&project, "#!/bin/sh\ncat >/dev/null\nexit 3\n", "x\n").unwrap_err();
    assert_eq!(silent.to_string(), "failed (exit status: 3)");
}

#[test]
fn a_formatter_that_ignores_its_input_and_fails_is_still_an_error() {
    let project = Project::new();
    let input = "x\n".repeat(200_000);
    let error = format(&project, "#!/bin/sh\necho nope >&2\nexit 1\n", &input).unwrap_err();
    assert_eq!(error.to_string(), "nope");
}

#[test]
fn output_that_is_not_utf8_is_rejected() {
    let project = Project::new();
    let error = format(
        &project,
        "#!/bin/sh\ncat >/dev/null\nprintf '\\377\\376garbage\\n'\n",
        "x\n",
    )
    .unwrap_err();
    assert!(matches!(error, RunError::InvalidUtf8), "{error:?}");
}

#[test]
fn empty_output_for_a_non_empty_buffer_is_rejected() {
    let project = Project::new();
    let swallow = "#!/bin/sh\ncat >/dev/null\n";
    let error = format(&project, swallow, "let a = 1;\n").unwrap_err();
    assert!(matches!(error, RunError::EmptyOutput), "{error:?}");

    let blank = format(
        &project,
        "#!/bin/sh\ncat >/dev/null\nprintf '\\n\\n'\n",
        "x\n",
    );
    assert!(matches!(blank, Err(RunError::EmptyOutput)), "{blank:?}");

    assert_eq!(format(&project, swallow, "\n\n").unwrap(), "");
    assert_eq!(format(&project, swallow, "").unwrap(), "");
}

#[test]
fn a_missing_executable_is_a_spawn_error() {
    let project = Project::new();
    let mut invocation = invocation(&project, UPPERCASE, Vec::new());
    invocation.program = project.path("bin/does-not-exist");

    let error = run(&invocation, "x\n", None, &Limits::default()).unwrap_err();
    assert!(matches!(error, RunError::Spawn(_)), "{error:?}");
    assert!(error.to_string().starts_with("could not be started: "));
}

#[test]
fn a_timeout_kills_the_formatter_and_everything_it_started() {
    let project = Project::new();
    let script = "#!/bin/sh\n[ \"$1\" = warm-up ] && exit 0\necho $$ > formatter.pid\nsleep 30 &\necho $! > helper.pid\nwait\n";
    let mut invocation = invocation(&project, script, vec!["warm-up".into()]);
    // The operating system can take longer than the timeout to vet a new executable the first
    // time it runs, so that cost is paid before the timed run.
    let _ = run(&invocation, "x\n", None, &Limits::default());
    invocation.args.clear();

    let started = Instant::now();
    let error = run(&invocation, "x\n", None, &short_timeout()).unwrap_err();

    assert!(matches!(error, RunError::TimedOut(_)), "{error:?}");
    assert_eq!(error.to_string(), "timed out after 300ms");
    assert!(started.elapsed() < Duration::from_secs(10));

    // The formatter was reaped before `run` returned, so it is gone rather than a zombie.
    assert!(process_is_gone(read_pid(project.path("formatter.pid"))));

    // The orphaned helper is reaped by the system, which can lag slightly behind the kill.
    let helper = read_pid(project.path("helper.pid"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !process_is_gone(helper) {
        assert!(Instant::now() < deadline, "helper {helper} survived");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_formatter_that_never_reads_or_writes_still_times_out() {
    let project = Project::new();
    let input = "x\n".repeat(200_000);
    let error = run(
        &invocation(&project, "#!/bin/sh\nexec sleep 30\n", Vec::new()),
        &input,
        None,
        &short_timeout(),
    )
    .unwrap_err();
    assert!(matches!(error, RunError::TimedOut(_)), "{error:?}");
}

#[test]
fn runaway_output_is_cut_off() {
    let project = Project::new();
    let limits = Limits {
        max_output_bytes: 1024,
        ..Limits::default()
    };
    let started = Instant::now();
    let error = run(
        &invocation(&project, "#!/bin/sh\nexec yes formatted\n", Vec::new()),
        "x\n",
        None,
        &limits,
    )
    .unwrap_err();

    assert!(matches!(error, RunError::OutputTooLarge), "{error:?}");
    assert!(started.elapsed() < limits.timeout);
}

#[test]
fn oversized_buffers_are_not_sent_to_the_formatter() {
    let project = Project::new();
    let limits = Limits {
        max_input_bytes: 8,
        ..Limits::default()
    };
    let error = run(
        &invocation(&project, "#!/bin/sh\ntouch ran\ncat\n", Vec::new()),
        "123456789",
        None,
        &limits,
    )
    .unwrap_err();

    assert!(
        matches!(error, RunError::InputTooLarge { bytes: 9 }),
        "{error:?}"
    );
    assert!(!project.path("ran").exists());
}

#[test]
fn noisy_stderr_does_not_stall_a_successful_run() {
    let project = Project::new();
    let script = "#!/bin/sh\nyes warning | head -c 500000 >&2\ntr '[:lower:]' '[:upper:]'\n";
    assert_eq!(format(&project, script, "ok\n").unwrap(), "OK\n");
}

#[test]
fn a_background_process_holding_stdout_does_not_hang_the_run() {
    let project = Project::new();
    // Keep a descendant alive after the leader exits to exercise group cleanup after EOF times out.
    let script = "#!/bin/sh\ncat >/dev/null\nsleep 30 &\necho $! > child.pid\necho done\n";
    let started = Instant::now();
    let error = format(&project, script, "x\n").unwrap_err();

    assert!(matches!(error, RunError::OutputLeftOpen), "{error:?}");
    assert!(started.elapsed() < Limits::default().timeout);
    let pid = read_pid(project.path("child.pid"));
    // The orphan may briefly remain a zombie until its new parent reaps it.
    for _ in 0..100 {
        if process_is_gone(pid) {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        assert!(
            stat.is_empty()
                || stat
                    .rsplit_once(") ")
                    .is_some_and(|(_, rest)| rest.starts_with('Z')),
            "formatter descendant still running: {stat}"
        );
    }
    #[cfg(not(target_os = "linux"))]
    assert!(process_is_gone(pid), "formatter descendant still running");
}

#[test]
fn crlf_buffers_stay_crlf() {
    let project = Project::new();
    let to_lf = "#!/bin/sh\ntr -d '\\r' | tr '[:lower:]' '[:upper:]'\n";
    assert_eq!(format(&project, to_lf, "a\r\nb\r\n").unwrap(), "A\r\nB\r\n");
}

#[test]
fn lf_buffers_stay_lf_when_the_formatter_emits_crlf() {
    let project = Project::new();
    let to_crlf = "#!/bin/sh\ncat >/dev/null\nprintf 'A\\r\\nB\\r\\n'\n";
    assert_eq!(format(&project, to_crlf, "a\nb\n").unwrap(), "A\nB\n");
}

#[test]
fn in_place_tools_rewrite_a_temporary_copy_and_never_the_original() {
    let project = Project::new();
    let original = project.write("src/input.txt", "what is saved on disk\n");
    // Rewrites the file named by its last argument, like `pint` or `php-cs-fixer fix`.
    let script = "#!/bin/sh\nfor target; do :; done\nprintf '%s' \"$target\" > target.path\ntr '[:lower:]' '[:upper:]' < \"$target\" > \"$target.tmp\" && mv \"$target.tmp\" \"$target\"\n";
    let invocation = invocation(&project, script, vec!["--quiet".into(), Arg::TempCopy]);

    let output = run(
        &invocation,
        "unsaved buffer text\n",
        None,
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(output, "UNSAVED BUFFER TEXT\n");
    assert_eq!(
        fs::read_to_string(&original).unwrap(),
        "what is saved on disk\n"
    );

    let copy = PathBuf::from(fs::read_to_string(project.path("target.path")).unwrap());
    assert_ne!(copy, original);
    assert_eq!(copy.file_name(), original.file_name());
    assert!(!copy.starts_with(project.root()));
    assert!(!copy.exists(), "the temporary copy is removed afterwards");
}

#[test]
fn a_failing_in_place_tool_is_an_error_and_leaves_the_original_alone() {
    let project = Project::new();
    let original = project.write("src/input.txt", "on disk\n");
    let script = "#!/bin/sh\necho 'PHP Parse error: syntax error' >&2\nexit 1\n";
    let invocation = invocation(&project, script, vec![Arg::TempCopy]);

    let error = run(&invocation, "buffer\n", None, &Limits::default()).unwrap_err();
    assert_eq!(error.to_string(), "PHP Parse error: syntax error");
    assert_eq!(fs::read_to_string(&original).unwrap(), "on disk\n");
}
