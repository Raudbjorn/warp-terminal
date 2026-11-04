use super::*;

/// A stand-in status line command for the tests that only check where it goes in the settings.
const COMMAND: &str = "store-the-input";

fn codex_line(primary: (f64, f64, i64), secondary: Option<(f64, f64, i64)>) -> String {
    let window = |(minutes, used, reset): (f64, f64, i64)| json!({"used_percent": used, "window_minutes": minutes, "resets_at": reset});
    let mut limits = json!({"limit_id": "codex", "primary": window(primary), "plan_type": "plus"});
    if let Some(secondary) = secondary {
        limits["secondary"] = window(secondary);
    }
    json!({"type": "event_msg", "payload": {"type": "token_count", "rate_limits": limits}})
        .to_string()
}

#[test]
fn codex_windows_are_placed_by_their_length() {
    // As logged on 21.10.2026: primary is the 5 hours, secondary the week.
    let entry = codex_entry(&[codex_line((300., 12., 1000), Some((10080., 19., 2000)))]).unwrap();
    assert_eq!(
        (entry["used5h"].as_f64(), entry["usedWeek"].as_f64()),
        (Some(12.), Some(19.))
    );
    // Mid 2026 the week came as primary and there was no 5 hour window.
    let entry = codex_entry(&[codex_line((10080., 40., 2000), None)]).unwrap();
    assert_eq!(entry["usedWeek"].as_f64(), Some(40.));
    assert!(entry.get("used5h").is_none());
    assert_eq!(entry["tier"], "plus");
}

#[test]
fn the_newest_codex_reading_wins() {
    let lines = [
        codex_line((300., 50., 1000), Some((10080., 30., 2000))),
        "not json".to_owned(),
        codex_line((300., 60., 1000), None),
    ];
    let entry = codex_entry(&lines).unwrap();
    assert_eq!(entry["used5h"].as_f64(), Some(60.));
    // A window the newest line leaves out keeps its older value.
    assert_eq!(entry["usedWeek"].as_f64(), Some(30.));
    assert_eq!(codex_entry(&["{}".to_owned()]), None);
}

#[test]
fn claude_reads_its_status_line_limits() {
    let input = r#"{"session_id":"s","model":{"id":"x"},"rate_limits":{"five_hour":{"used_percentage":4.0,"resets_at":1792590000},"seven_day":{"used_percentage":72,"resets_at":1792980000}}}"#;
    let entry = claude_entry(input).unwrap();
    assert_eq!(entry["name"], "Claude");
    assert_eq!(
        (entry["used5h"].as_f64(), entry["resetWeek"].as_f64()),
        (Some(4.), Some(1_792_980_000.))
    );
}

#[test]
fn claude_without_limits_has_no_ring() {
    assert_eq!(
        claude_entry(r#"{"session_id":"s","model":{"id":"x"}}"#),
        None
    );
    assert_eq!(claude_entry(r#"{"rate_limits":{}}"#), None);
    assert_eq!(claude_entry("not json"), None);
}

#[test]
fn the_status_line_goes_in_without_touching_the_rest() {
    let settings = "{\n  \"model\": \"opus\",\n  \"hooks\": {}\n}\n";
    let updated = with_status_line(Some(settings), COMMAND).unwrap();
    let parsed: Value = serde_json::from_str(&updated).unwrap();
    assert_eq!(parsed["statusLine"]["command"], COMMAND);
    assert_eq!(parsed["model"], "opus");
    assert!(updated.ends_with("  \"model\": \"opus\",\n  \"hooks\": {}\n}\n"));
    // Empty object, no file, an empty file.
    for settings in [Some("{}"), None, Some("")] {
        let updated = with_status_line(settings, COMMAND).unwrap();
        let parsed: Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(parsed["statusLine"]["type"], "command");
    }
}

#[test]
fn a_status_line_of_the_user_and_broken_settings_stay_as_they_are() {
    assert_eq!(
        with_status_line(
            Some(r#"{"statusLine": {"type": "command", "command": "x"}}"#),
            COMMAND
        ),
        None
    );
    assert_eq!(with_status_line(Some("{ not json"), COMMAND), None);
    assert_eq!(with_status_line(Some("[]"), COMMAND), None);
}

#[test]
fn newest_rollouts_walk_the_date_folders() {
    let dir = std::env::temp_dir().join(format!("harp-rollouts-{}", std::process::id()));
    for (day, name) in [
        ("2026/09/30", "rollout-2026-09-30T10-00-00-a.jsonl"),
        ("2026/10/21", "rollout-2026-10-21T07-00-00-b.jsonl"),
        ("2026/10/21", "rollout-2026-10-21T09-00-00-c.jsonl"),
        ("2026/10/21", "notes.txt"),
    ] {
        std::fs::create_dir_all(dir.join(day)).unwrap();
        std::fs::write(dir.join(day).join(name), "").unwrap();
    }
    let names: Vec<String> = newest_rollouts(&dir, 3)
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(
        names,
        [
            "rollout-2026-09-30T10-00-00-a.jsonl",
            "rollout-2026-10-21T07-00-00-b.jsonl",
            "rollout-2026-10-21T09-00-00-c.jsonl"
        ]
    );
}

#[test]
fn the_codex_reading_follows_a_change_to_an_older_rollout_file() {
    let root = tempfile::tempdir().unwrap();
    let day = root.path().join("2026/10/21");
    std::fs::create_dir_all(&day).unwrap();
    let older = day.join("rollout-2026-10-21T07-00-00-a.jsonl");
    let newer = day.join("rollout-2026-10-21T09-00-00-b.jsonl");
    std::fs::write(&older, codex_line((300., 10., 1000), None)).unwrap();
    std::fs::write(&newer, "").unwrap();

    let mut cache = Cache::default();
    let first = codex(root.path(), &mut cache, 1.).unwrap();
    assert_eq!(first["used5h"].as_f64(), Some(10.));

    // The older session goes on reporting while the newest one sits idle, untouched.
    std::fs::write(&older, codex_line((300., 50., 1000), None)).unwrap();
    let later = SystemTime::now() + std::time::Duration::from_secs(10);
    std::fs::File::options()
        .write(true)
        .open(&older)
        .unwrap()
        .set_modified(later)
        .unwrap();

    let second = codex(root.path(), &mut cache, 2.).unwrap();
    assert_eq!(second["used5h"].as_f64(), Some(50.));
}

/// A home with `~/.claude/settings.json` holding `settings`, and a separate state dir that does
/// not exist yet (the status line command and the installer must create it).
fn claude_home(settings: Option<&str>) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    if let Some(settings) = settings {
        std::fs::write(home.join(".claude").join("settings.json"), settings).unwrap();
    }
    // A space, so the command's quoting is exercised.
    let state_dir = root.path().join("state dir").join("warp-oss");
    (root, home, state_dir)
}

#[test]
#[cfg(unix)]
fn the_status_line_command_writes_into_the_state_dir_via_a_temp_file() {
    let root = tempfile::tempdir().unwrap();
    // A quote and a space, so the command's quoting is exercised.
    let state_dir = root.path().join("it's state").join("warp-oss");
    let command = claude_status_line_command(&state_dir).unwrap();
    assert!(command.contains("mkdir -p"), "{command}");
    assert!(command.contains(".$$"), "{command}");
    assert!(command.contains("mv -f"), "{command}");
    assert!(
        !command.contains("$HOME") && !command.contains(".harp"),
        "{command}"
    );

    // Run it the way Claude Code does: through a shell, the status line input on stdin.
    let input = r#"{"rate_limits":{"five_hour":{"used_percentage":4,"resets_at":1}}}"#;
    let mut child = command::blocking::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), input.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    // An empty status line: the command prints nothing.
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read_to_string(claude_status_file(&state_dir)).unwrap(),
        input
    );
    // The per-process temp file was renamed away, not left behind.
    let names: Vec<_> = std::fs::read_dir(&state_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, [std::ffi::OsString::from("claude-statusline.json")]);
}

#[test]
fn a_relative_state_dir_gives_no_status_line_command() {
    // `state_dir()` is empty when the platform has no home; a relative path would make Claude
    // Code write into whatever directory it runs in.
    assert_eq!(claude_status_line_command(Path::new("")), None);
    assert_eq!(claude_status_line_command(Path::new("warp-oss")), None);
}

#[test]
fn installing_points_claude_at_the_state_dir_and_marks_it_there() {
    let (_root, home, state_dir) = claude_home(Some("{\n  \"model\": \"opus\"\n}\n"));
    assert_eq!(
        ensure_claude_status_line(&home, &state_dir).unwrap(),
        StatusLineInstall::Installed
    );
    let settings = std::fs::read_to_string(home.join(".claude").join("settings.json")).unwrap();
    let parsed: Value = serde_json::from_str(&settings).unwrap();
    assert_eq!(
        parsed["statusLine"]["command"],
        claude_status_line_command(&state_dir).unwrap()
    );
    assert_eq!(parsed["model"], "opus");
    assert!(state_dir.join("claude-statusline.installed").is_file());
    assert!(!home.join(".harp").exists());
    assert!(!home.join(".claude").join("settings.json.harp-tmp").exists());
    // Once marked, a status line the user removes again stays removed.
    std::fs::write(home.join(".claude").join("settings.json"), "{}").unwrap();
    assert_eq!(
        ensure_claude_status_line(&home, &state_dir).unwrap(),
        StatusLineInstall::AlreadyInstalled
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".claude").join("settings.json")).unwrap(),
        "{}"
    );
}

#[test]
#[cfg(unix)]
fn a_symlinked_settings_file_stays_a_link_with_its_permissions() {
    use std::os::unix::fs::PermissionsExt as _;

    let (root, home, state_dir) = claude_home(None);
    // Settings kept in a dotfiles checkout and linked into ~/.claude.
    let dotfiles = root.path().join("dotfiles").join("settings.json");
    std::fs::create_dir_all(dotfiles.parent().unwrap()).unwrap();
    std::fs::write(&dotfiles, "{\"model\": \"opus\"}\n").unwrap();
    std::fs::set_permissions(&dotfiles, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = home.join(".claude").join("settings.json");
    std::os::unix::fs::symlink(&dotfiles, &link).unwrap();

    assert_eq!(
        ensure_claude_status_line(&home, &state_dir).unwrap(),
        StatusLineInstall::Installed
    );
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    assert!(
        std::fs::read_to_string(&dotfiles)
            .unwrap()
            .contains("statusLine")
    );
    let mode = std::fs::metadata(&dotfiles).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn an_existing_status_line_is_left_alone() {
    let mine = r#"{"statusLine": {"type": "command", "command": "my-prompt"}}"#;
    let (_root, home, state_dir) = claude_home(Some(mine));
    assert_eq!(
        ensure_claude_status_line(&home, &state_dir).unwrap(),
        StatusLineInstall::KeptExisting
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".claude").join("settings.json")).unwrap(),
        mine
    );
    assert!(!state_dir.exists());
}

#[test]
fn without_claude_code_nothing_is_written() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let state_dir = root.path().join("state");
    assert_eq!(
        ensure_claude_status_line(&home, &state_dir).unwrap(),
        StatusLineInstall::NoClaudeSetup
    );
    assert!(!home.join(".claude").exists());
    assert!(!state_dir.exists());
}

#[test]
fn the_claude_ring_is_read_from_the_state_dir() {
    let root = tempfile::tempdir().unwrap();
    let state_dir = root.path().join("warp-oss");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(
        claude_status_file(&state_dir),
        r#"{"rate_limits":{"five_hour":{"used_percentage":4,"resets_at":1792590000}}}"#,
    )
    .unwrap();
    let providers = providers(Some(&state_dir), None, &mut Cache::default(), 5.);
    assert_eq!(providers["claude"]["used5h"].as_f64(), Some(4.));
    assert_eq!(providers["claude"]["_ts"].as_f64(), Some(5.));
}

#[test]
fn legacy_harp_status_file_is_read_when_it_is_the_only_one() {
    let root = tempfile::tempdir().unwrap();
    let state_dir = root.path().join("state");
    let legacy = legacy_claude_status_file(root.path());
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "{}").unwrap();

    let (path, _) = newest_status_file([claude_status_file(&state_dir), legacy.clone()]).unwrap();
    assert_eq!(path, legacy);
}

#[test]
fn the_newer_status_file_wins() {
    let root = tempfile::tempdir().unwrap();
    let state_dir = root.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let current = claude_status_file(&state_dir);
    let legacy = legacy_claude_status_file(root.path());
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "{}").unwrap();
    std::fs::write(&current, "{}").unwrap();
    let older = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(&legacy)
        .unwrap()
        .set_modified(older)
        .unwrap();

    let (path, _) = newest_status_file([current.clone(), legacy]).unwrap();
    assert_eq!(path, current);
}

#[test]
fn no_status_file_gives_none() {
    let root = tempfile::tempdir().unwrap();
    assert!(newest_status_file([claude_status_file(root.path())]).is_none());
}
