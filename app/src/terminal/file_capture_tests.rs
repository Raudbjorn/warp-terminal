use std::fs;

use chrono::{Local, TimeZone};
use tempfile::TempDir;

use super::*;

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("capture file should be readable")
}

#[test]
fn sanitize_token_strips_path_separators() {
    // A command like `git log > /tmp/x` must not be able to produce a path.
    let out = sanitize_token("git log --oneline > /tmp/x");
    assert!(!out.contains('/'), "got {out}");
    assert!(!out.contains('\\'), "got {out}");
    assert!(!out.contains('>'), "got {out}");
}

#[test]
fn sanitize_token_collapses_separator_runs() {
    assert_eq!(sanitize_token("a   ///   b"), "a_b");
}

#[test]
fn sanitize_token_falls_back_when_nothing_survives() {
    assert_eq!(sanitize_token("///"), "untitled");
    assert_eq!(sanitize_token("   "), "untitled");
}

#[test]
fn expand_pattern_keeps_the_extension_dot() {
    // Sanitising the finished string instead of each token would eat this dot.
    let tokens = PatternTokens {
        command: Some("cargo build".to_owned()),
        ..Default::default()
    };
    let name = expand_pattern("{command}_{timestamp}.log", &tokens);
    assert!(name.ends_with(".log"), "got {name}");
    assert!(name.starts_with("cargo_build_"), "got {name}");
}

#[test]
fn expand_pattern_prefers_session_name_then_falls_back_to_command() {
    let with_name = PatternTokens {
        command: Some("ls".to_owned()),
        session_name: Some("my session".to_owned()),
        ..Default::default()
    };
    assert_eq!(
        expand_pattern("{session-or-command}.log", &with_name),
        "my_session.log"
    );

    // An auto-generated (absent) name falls back to the command.
    let without_name = PatternTokens {
        command: Some("ls".to_owned()),
        ..Default::default()
    };
    assert_eq!(
        expand_pattern("{session-or-command}.log", &without_name),
        "ls.log"
    );

    // A blank name is treated as absent rather than producing "untitled".
    let blank_name = PatternTokens {
        command: Some("ls".to_owned()),
        session_name: Some("   ".to_owned()),
        ..Default::default()
    };
    assert_eq!(
        expand_pattern("{session-or-command}.log", &blank_name),
        "ls.log"
    );
}

#[test]
fn expand_pattern_uses_finish_time_when_present() {
    let finished = Local.with_ymd_and_hms(2026, 8, 9, 13, 5, 1).unwrap();
    let tokens = PatternTokens {
        finished: Some(finished),
        ..Default::default()
    };
    assert_eq!(
        expand_pattern("{finished-timestamp}.log", &tokens),
        "20260809130501.log"
    );
}

#[test]
fn expand_pattern_leaves_unknown_tokens_visible() {
    let name = expand_pattern("{nope}.log", &PatternTokens::default());
    assert_eq!(name, "{nope}.log");
}

#[test]
fn update_tail_appends_when_output_only_grows() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("capture.log");

    let mut stream =
        FileStream::create(&path, "line one\n", Some((0, "line one\n".to_owned()))).unwrap();
    stream.update_tail("line one\nline two\n").unwrap();

    assert_eq!(read(&path), "line one\nline two\n");
}

#[test]
fn update_tail_rewrites_when_the_block_redraws() {
    // A progress bar redrawing over itself must replace its section, not
    // accumulate every frame.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("capture.log");

    let mut stream =
        FileStream::create(&path, "[..........]", Some((0, "[..........]".to_owned()))).unwrap();
    stream.update_tail("[#####.....]").unwrap();
    stream.update_tail("[##########]").unwrap();

    assert_eq!(read(&path), "[##########]");
}

#[test]
fn update_tail_rewrite_preserves_earlier_blocks() {
    // The rewind must stop at the tailed block's offset, never clobbering
    // blocks already written above it.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("capture.log");

    let initial = "first block\nsecond: [....]";
    let mut stream =
        FileStream::create(&path, initial, Some((1, "second: [....]".to_owned()))).unwrap();
    stream.update_tail("second: [##]").unwrap();

    assert_eq!(read(&path), "first block\nsecond: [##]");
}

#[test]
fn update_tail_is_a_no_op_when_nothing_changed() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("capture.log");

    let mut stream = FileStream::create(&path, "same", Some((0, "same".to_owned()))).unwrap();
    stream.update_tail("same").unwrap();

    assert_eq!(read(&path), "same");
}

#[test]
fn advance_to_finalises_then_tails_the_next_block() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("capture.log");

    let mut stream = FileStream::create(&path, "one", Some((0, "one".to_owned()))).unwrap();
    stream.advance_to("one done", 1, BLOCK_SEPARATOR).unwrap();
    assert_eq!(stream.block_index(), 1);

    // The new block starts empty, so its first render appends rather than
    // rewriting the previous block.
    stream.update_tail("two").unwrap();
    assert_eq!(read(&path), format!("one done{BLOCK_SEPARATOR}two"));

    // And a redraw of the new block still leaves the finished one intact.
    stream.update_tail("TWO").unwrap();
    assert_eq!(read(&path), format!("one done{BLOCK_SEPARATOR}TWO"));
}

#[test]
fn write_snapshot_replaces_existing_contents() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("snapshot.log");

    write_snapshot(&path, "first").unwrap();
    write_snapshot(&path, "second").unwrap();

    assert_eq!(read(&path), "second");
}

#[test]
fn invalid_tail_preserves_the_existing_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("capture.log");
    fs::write(&path, "existing capture").unwrap();

    for tail in ["different", "longer than the snapshot"] {
        let result = FileStream::create(&path, "snapshot", Some((0, tail.to_owned())));
        assert!(matches!(result, Err(error) if error.kind() == ErrorKind::InvalidInput));
        assert_eq!(read(&path), "existing capture");
    }
}

#[test]
fn unicode_tail_rewrite_and_append_preserve_the_prefix() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("capture.log");
    let mut stream =
        FileStream::create(&path, "fyrri: á\nnú: 🦀", Some((1, "nú: 🦀".to_owned()))).unwrap();

    stream.update_tail("ó").unwrap();
    stream.update_tail("ó\nfinished").unwrap();
    assert_eq!(read(&path), "fyrri: á\nó\nfinished");
    stream.update_tail("").unwrap();
    assert_eq!(read(&path), "fyrri: á\n");
}

#[cfg(unix)]
#[test]
fn new_capture_files_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let snapshot = dir.path().join("snapshot.log");
    let stream = dir.path().join("stream.log");
    write_snapshot(&snapshot, "secret").unwrap();
    FileStream::create(&stream, "secret", None)
        .unwrap()
        .finish();

    for path in [snapshot, stream] {
        assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
    }
}

#[test]
fn capture_creation_reports_invalid_destinations() {
    let dir = TempDir::new().unwrap();
    let missing_parent = dir.path().join("missing/capture.log");
    assert!(write_snapshot(&missing_parent, "output").is_err());
    assert!(FileStream::create(&missing_parent, "output", None).is_err());
    assert!(write_snapshot(dir.path(), "output").is_err());
    assert!(FileStream::create(dir.path(), "output", None).is_err());
}

#[test]
fn active_capture_rejects_other_writers_before_truncating() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("capture.log");
    let alias = dir.path().join("alias.log");
    let mut stream = FileStream::create(&path, "original", Some((0, "original".into()))).unwrap();
    fs::hard_link(&path, &alias).unwrap();

    for destination in [&path, &alias] {
        assert!(write_snapshot(destination, "replacement").is_err());
        assert!(FileStream::create(destination, "replacement", None).is_err());
        assert_eq!(read(&path), "original");
    }
    stream.update_tail("original continued").unwrap();
    assert_eq!(read(&path), "original continued");

    stream.finish();
    write_snapshot(&alias, "after closing").unwrap();
    assert_eq!(read(&path), "after closing");
}
