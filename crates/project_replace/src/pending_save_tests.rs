use crate::test_support::{Project, literal, open_buffer};
use crate::undo::undo_latest;
use file_write_guard::WriteGuard;
use std::sync::atomic::AtomicBool;

#[test]
fn pending_saves_block_only_their_own_replacement_and_undo_targets() {
    let project = Project::new();
    let pending_path = project.write("pending.txt", "foo\n");
    project.write("other.txt", "foo\n");
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["pending.txt", "other.txt"]);
    let guard =
        WriteGuard::acquire(pending_path.clone(), project.root().join("pending-journal")).unwrap();
    for open in [false, true] {
        let mut request = project.request(&spec, "bar", targets.clone());
        if open {
            request.open_buffers = vec![open_buffer(&pending_path, "foo\n", false)];
        }
        let outcome = project.replace(&request);
        assert!(outcome.buffer_edits.is_empty());
        assert!(outcome.skipped.iter().any(|file| file.path == pending_path));
        assert_eq!(project.read_text("pending.txt"), "foo\n");
        outcome.finish(Vec::new(), Vec::new());
    }
    assert_eq!(project.read_text("other.txt"), "bar\n");
    guard.cancel_before_dispatch();
    project.write("other.txt", "foo\n");
    let targets = project.search_all(&spec, &["pending.txt", "other.txt"]);
    let outcome = project.replace(&project.request(&spec, "bar", targets));
    let transaction = outcome.transaction_directory();
    outcome.finish(Vec::new(), Vec::new());
    let guard = WriteGuard::acquire(pending_path.clone(), transaction).unwrap();
    let outcome = undo_latest(&project.journal, &[], &|_| {}, &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    assert_eq!(outcome.restored, vec![project.path("other.txt")]);
    assert!(outcome.refused.iter().any(|file| file.path == pending_path));
    outcome.finish(&[], Vec::new());
    assert_eq!(project.read_text("pending.txt"), "bar\n");
    assert!(project.journal.latest().unwrap().is_some());
    guard.cancel_before_dispatch();
}

#[cfg(any(unix, windows))]
#[test]
fn clean_open_buffers_with_hard_link_aliases_cannot_start_background_saves() {
    let project = Project::new();
    let path = project.write("file.txt", "foo\n");
    std::fs::hard_link(&path, project.path("alias.txt")).unwrap();
    let spec = literal("foo");
    let mut request = project.request(&spec, "bar", project.search_all(&spec, &["file.txt"]));
    request.open_buffers = vec![open_buffer(&path, "foo\n", false)];
    let outcome = project.replace(&request);
    assert!(outcome.buffer_edits.is_empty());
    assert_eq!(
        outcome.skipped[0].reason,
        crate::report::SkipReason::HardLinked
    );
    assert_eq!(project.read_text("file.txt"), "foo\n");
}
