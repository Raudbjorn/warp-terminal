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
fn sanitize_token_limits_utf8_bytes_at_character_boundaries() {
    assert_eq!(sanitize_token(&"a".repeat(65)), "a".repeat(64));
    assert_eq!(sanitize_token(&"😀".repeat(64)), "😀".repeat(16));
    // 64 is not a character boundary for three-byte characters or mixed widths.
    assert_eq!(sanitize_token(&"界".repeat(22)), "界".repeat(21));
    assert_eq!(
        sanitize_token(&format!("{}😀z", "a".repeat(62))),
        "a".repeat(62)
    );
    assert_eq!(
        sanitize_token(&format!("{}😀z", "a".repeat(60))),
        format!("{}😀", "a".repeat(60))
    );
}

#[test]
fn default_session_filename_with_multibyte_name_fits_filesystem_component() {
    let tokens = PatternTokens {
        session_name: Some("😀".repeat(64)),
        ..Default::default()
    };
    let name = expand_pattern("{session-or-command}_{timestamp}.log", &tokens);
    assert!(name.starts_with(&format!("{}_", "😀".repeat(16))));
    assert!(name.ends_with(".log"));
    assert!(name.len() <= 255, "filename has {} UTF-8 bytes", name.len());

    let directory = TempDir::new().unwrap();
    write_snapshot(&directory.path().join(name), "captured output").unwrap();
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
fn expand_pattern_does_not_expand_tokens_in_substituted_values() {
    let tokens = PatternTokens {
        command: Some("echo {timestamp} {session-or-command}".to_owned()),
        session_name: Some("tab {command} {finished-timestamp}".to_owned()),
        ..Default::default()
    };
    assert_eq!(
        expand_pattern("{command}_{session-or-command}.log", &tokens),
        "echo_{timestamp}_{session-or-command}_tab_{command}_{finished-timestamp}.log"
    );
    assert_eq!(
        expand_pattern("íslenska_{unknown}_{command}_{", &tokens),
        "íslenska_{unknown}_echo_{timestamp}_{session-or-command}_{"
    );
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

#[test]
fn update_and_commit_keeps_already_written_later_output() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("session.log");
    let mut stream =
        FileStream::create(&path, "one\ntwo\ntail", Some((0, "one\ntwo\ntail".into()))).unwrap();
    stream.update_and_commit("one\ntwo\ntail", 4).unwrap();
    assert_eq!(read(&path), "one\ntwo\ntail");
    stream.update_and_commit("two\ntail final", 4).unwrap();
    assert_eq!(read(&path), "one\ntwo\ntail final");
}

fn wait_for_capture_close(path: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let file = OpenOptions::new().write(true).open(path).unwrap();
        if file.try_lock().is_ok() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "capture did not close"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
#[serial_test::serial(file_capture)]
fn async_capture_drains_final_snapshot_after_stalled_write_without_view_callback() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("session.log");
    let stream = futures_lite::future::block_on(
        AsyncFileStream::create(
            path.clone(),
            "one\ntail".into(),
            Some((0, "one\ntail".into())),
        )
        .unwrap(),
    )
    .unwrap();
    let finish = stream.finish_handle();
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let operation = stream.run(move |stream| {
        started_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        stream.update_and_commit("one\ntail", 4)
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    // A final render is relative to the prefix the ordered in-flight write commits.
    let finished = finish.queue("tail final".into(), 0);
    release_tx.send(()).unwrap();
    let (stream, result) = futures_lite::future::block_on(operation);
    result.unwrap();
    futures_lite::future::block_on(finished).unwrap().unwrap();
    assert_eq!(read(&path), "one\ntail final");
    drop(stream);
    wait_for_capture_close(&path);
}

#[test]
#[serial_test::serial(file_capture)]
fn async_capture_drop_drains_late_final_snapshot_and_releases_lock() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("session.log");
    let stream = futures_lite::future::block_on(
        AsyncFileStream::create(path.clone(), "initial".into(), Some((0, "initial".into())))
            .unwrap(),
    )
    .unwrap();
    let finish = stream.finish_handle();
    let finished = finish.queue("final".into(), 0);
    drop(stream);
    futures_lite::future::block_on(finished).unwrap().unwrap();
    wait_for_capture_close(&path);
    assert_eq!(read(&path), "final");
}

#[test]
#[serial_test::serial(file_capture)]
fn async_capture_propagates_open_and_update_errors() {
    let directory = TempDir::new().unwrap();
    let invalid = directory.path().join("missing").join("capture");
    assert!(
        futures_lite::future::block_on(
            AsyncFileStream::create(invalid, String::new(), None).unwrap()
        )
        .is_err()
    );
    let path = directory.path().join("session.log");
    let stream = futures_lite::future::block_on(
        AsyncFileStream::create(path.clone(), "initial".into(), Some((0, "initial".into())))
            .unwrap(),
    )
    .unwrap();
    let (stream, result) = futures_lite::future::block_on(
        stream.run(|_| Err(Error::new(ErrorKind::WriteZero, "injected write failure"))),
    );
    assert_eq!(result.unwrap_err().kind(), ErrorKind::WriteZero);
    drop(stream);
    wait_for_capture_close(&path);
}

#[test]
#[serial_test::serial(file_capture)]
fn async_capture_caps_owners_including_stalled_operations_and_releases_permits() {
    let directory = TempDir::new().unwrap();
    // Other serial tests may have just queued off-thread close jobs.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while ACTIVE_CAPTURES.load(Ordering::Acquire) != 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let mut owners = Vec::new();
    for index in 0..MAX_ACTIVE_CAPTURES {
        owners.push(
            futures_lite::future::block_on(
                AsyncFileStream::create(
                    directory.path().join(index.to_string()),
                    String::new(),
                    Some((0, String::new())),
                )
                .unwrap(),
            )
            .unwrap(),
        );
    }
    let result = AsyncFileStream::create(directory.path().join("overflow"), String::new(), None);
    assert!(matches!(result, Err(error) if error.kind() == ErrorKind::WouldBlock));
    assert!(!directory.path().join("overflow").exists());
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let operation = owners.pop().unwrap().run(move |_| {
        started_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        Ok(())
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    // Dropping the awaiter must not free a permit while its syscall/job still runs.
    drop(operation);
    assert_eq!(ACTIVE_CAPTURES.load(Ordering::Acquire), MAX_ACTIVE_CAPTURES);
    release_tx.send(()).unwrap();
    drop(owners);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while ACTIVE_CAPTURES.load(Ordering::Acquire) != 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
#[serial_test::serial(file_capture)]
fn async_capture_rejects_late_final_snapshot_after_failed_commit() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("session.log");
    let stream = futures_lite::future::block_on(
        AsyncFileStream::create(
            path.clone(),
            "one\ntail".into(),
            Some((0, "one\ntail".into())),
        )
        .unwrap(),
    )
    .unwrap();
    let finish = stream.finish_handle();
    let (stream, result) = futures_lite::future::block_on(
        stream.run(|_| Err(Error::other("injected failure before prefix commit"))),
    );
    assert!(result.is_err());
    // The UI had advanced its logical prefix when scheduling the failed write.
    // A final tail relative to that prefix must never be applied at the old file offset.
    let finished = finish.queue("tail final".into(), 0);
    assert!(futures_lite::future::block_on(finished).unwrap().is_err());
    drop(stream);
    wait_for_capture_close(&path);
    assert_eq!(read(&path), "one\ntail");
}

#[test]
#[serial_test::serial(file_capture)]
fn async_capture_final_write_error_reaches_completion_receiver() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("session.log");
    let stream = futures_lite::future::block_on(
        AsyncFileStream::create(path.clone(), "initial".into(), Some((0, "initial".into())))
            .unwrap(),
    )
    .unwrap();
    let finish = stream.finish_handle();
    // Reject an invalid UTF-8 prefix before touching the existing file.
    let finished = finish.queue("é".into(), 1);
    drop(stream);
    assert_eq!(
        futures_lite::future::block_on(finished)
            .unwrap()
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );
    wait_for_capture_close(&path);
    assert_eq!(read(&path), "initial");
}
