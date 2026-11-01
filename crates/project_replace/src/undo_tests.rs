use super::*;
use crate::apply::{AppliedBufferEdits, ReplaceReport, replace_in_files};
use crate::plan::apply_edits;
use crate::test_support::{Project, literal, open_buffer};

fn undo(project: &Project, open_buffers: &[OpenBuffer]) -> Option<UndoOutcome> {
    undo_latest(
        &project.journal,
        open_buffers,
        &|_| {},
        &AtomicBool::new(false),
    )
    .unwrap()
}

fn undo_all(project: &Project) -> UndoReport {
    undo(project, &[])
        .expect("there is a replace to undo")
        .finish(&[], Vec::new())
}

fn refused(report: &UndoReport) -> Vec<(String, RefusalReason)> {
    report
        .refused
        .iter()
        .map(|refused| {
            let name = refused.path.file_name().unwrap().to_str().unwrap();
            (name.to_string(), refused.reason.clone())
        })
        .collect()
}

/// Replaces "foo" with "quux" in `files` and closes the replace.
fn replace_foo(project: &Project, files: &[&str]) -> ReplaceReport {
    project
        .replace_literal("foo", "quux", files)
        .finish(Vec::new(), Vec::new())
}

/// Replaces "foo" with "quux" in a file that is open in an editor holding `editor_text`, and
/// returns the text the editor holds afterwards.
fn replace_foo_in_editor(project: &Project, file: &str, editor_text: &str, dirty: bool) -> String {
    let path = project.path(file);
    let spec = literal("foo");
    let targets = project.search_all(&spec, &[file]);
    let mut request = project.request(&spec, "quux", targets);
    request.open_buffers = vec![open_buffer(&path, editor_text, dirty)];
    let outcome = project.replace(&request);
    assert_eq!(outcome.buffer_edits.len(), 1, "{:?}", outcome.skipped);

    let text_after = apply_edits(editor_text, &outcome.buffer_edits[0].edits);
    let occurrences = outcome.buffer_edits[0].edits.len();
    outcome.finish(
        vec![AppliedBufferEdits {
            path,
            text_before: editor_text.to_string(),
            text_after: text_after.clone(),
            occurrences,
            save_error: None,
        }],
        Vec::new(),
    );
    text_after
}

#[test]
fn undoing_puts_every_file_back_byte_for_byte() {
    let project = Project::new();
    let originals: [(&str, Vec<u8>); 4] = [
        ("unix.txt", b"foo\nbar foo\n".to_vec()),
        ("windows.txt", b"foo\r\nbar foo".to_vec()),
        ("bom.txt", [&[0xEF, 0xBB, 0xBF][..], b"foo\r\n"].concat()),
        ("sub/nested.txt", "foo é 🦀\n".as_bytes().to_vec()),
    ];
    for (name, content) in &originals {
        project.write(name, content);
    }
    let files: Vec<&str> = originals.iter().map(|(name, _)| *name).collect();

    let report = replace_foo(&project, &files);
    assert_eq!(report.replaced.len(), 4);
    for (name, content) in &originals {
        assert_ne!(&project.read(name), content, "{name}");
    }

    let report = undo_all(&project);
    assert_eq!(report.restored.len(), 4);
    assert!(report.refused.is_empty());
    for (name, content) in &originals {
        assert_eq!(&project.read(name), content, "{name}");
    }

    // The journal is cleared once everything was put back.
    assert!(project.journal.latest().unwrap().is_none());
    assert!(undo(&project, &[]).is_none());
}

#[cfg(unix)]
#[test]
fn undoing_keeps_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let project = Project::new();
    let script = project.write("run.sh", "echo foo\n");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o750)).unwrap();

    replace_foo(&project, &["run.sh"]);
    undo_all(&project);

    assert_eq!(project.read_text("run.sh"), "echo foo\n");
    let mode = fs::metadata(&script).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o750);
}

#[test]
fn a_file_edited_since_the_replace_is_refused_and_can_be_retried() {
    let project = Project::new();
    project.write("kept.txt", "foo\n");
    project.write("edited.txt", "foo\n");
    replace_foo(&project, &["kept.txt", "edited.txt"]);

    project.write("edited.txt", "quux\nand a line typed after the replace\n");
    let report = undo_all(&project);

    assert_eq!(report.restored, [project.path("kept.txt")]);
    assert_eq!(
        refused(&report),
        [("edited.txt".to_string(), RefusalReason::EditedSinceReplace)]
    );
    assert_eq!(project.read_text("kept.txt"), "foo\n");
    assert_eq!(
        project.read_text("edited.txt"),
        "quux\nand a line typed after the replace\n"
    );

    // The refused file stays in the journal, so undoing works once the edit is taken back.
    let transaction = project.journal.latest().unwrap().unwrap();
    assert_eq!(transaction.entries.len(), 1);
    project.write("edited.txt", "quux\n");
    let report = undo_all(&project);
    assert_eq!(report.restored, [project.path("edited.txt")]);
    assert_eq!(project.read_text("edited.txt"), "foo\n");
    assert!(project.journal.latest().unwrap().is_none());
}

#[test]
fn a_deleted_file_is_refused() {
    let project = Project::new();
    let path = project.write("gone.txt", "foo\n");
    replace_foo(&project, &["gone.txt"]);
    fs::remove_file(&path).unwrap();

    let report = undo_all(&project);
    assert_eq!(
        refused(&report),
        [("gone.txt".to_string(), RefusalReason::Missing)]
    );
    assert!(!path.exists());
}

#[test]
fn a_file_that_already_holds_its_original_content_counts_as_restored() {
    let project = Project::new();
    project.write("file.txt", "foo\n");
    replace_foo(&project, &["file.txt"]);

    // As after a crash between writing the journal and writing the file, or a manual revert.
    project.write("file.txt", "foo\n");
    let report = undo_all(&project);
    assert_eq!(report.restored, [project.path("file.txt")]);
    assert!(report.refused.is_empty());
    assert!(project.journal.latest().unwrap().is_none());
}

#[test]
fn a_damaged_copy_is_refused_rather_than_written() {
    let project = Project::new();
    project.write("file.txt", "foo\n");
    replace_foo(&project, &["file.txt"]);

    let transaction = project.journal.latest().unwrap().unwrap();
    let journal_root = project.root().parent().unwrap().join("journal");
    fs::write(
        journal_root.join(&transaction.id).join("0.pre"),
        b"tampered",
    )
    .unwrap();

    let report = undo_all(&project);
    assert!(matches!(
        report.refused[0].reason,
        RefusalReason::JournalDamaged(_)
    ));
    assert_eq!(project.read_text("file.txt"), "quux\n");
}

#[test]
fn a_cancelled_replace_is_undone_completely() {
    let project = Project::new();
    let files = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"];
    for file in files {
        project.write(file, format!("{file} foo\n"));
    }
    let spec = literal("foo");
    let targets = project.search_all(&spec, &files);

    let cancel = AtomicBool::new(false);
    let outcome = replace_in_files(
        &project.request(&spec, "quux", targets),
        &project.journal,
        &|progress| {
            if progress.done == 3 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        &cancel,
    )
    .unwrap();
    assert_eq!(outcome.not_attempted, 2);
    let report = outcome.finish(Vec::new(), Vec::new());
    assert_eq!(report.replaced.len(), 3);
    assert_eq!(project.read_text("c.txt"), "c.txt quux\n");
    assert_eq!(project.read_text("d.txt"), "d.txt foo\n");

    let report = undo_all(&project);
    assert_eq!(report.restored.len(), 3);
    assert!(report.refused.is_empty());
    for file in files {
        assert_eq!(project.read_text(file), format!("{file} foo\n"));
    }
    assert!(project.journal.latest().unwrap().is_none());
}

#[test]
fn a_cancelled_undo_is_finished_by_the_next_one() {
    let project = Project::new();
    let files = ["a.txt", "b.txt", "c.txt"];
    for file in files {
        project.write(file, "foo\n");
    }
    replace_foo(&project, &files);

    let cancel = AtomicBool::new(false);
    let outcome = undo_latest(
        &project.journal,
        &[],
        &|progress| {
            if progress.done == 1 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        &cancel,
    )
    .unwrap()
    .unwrap();
    assert_eq!(outcome.not_attempted, 2);
    let report = outcome.finish(&[], Vec::new());
    assert_eq!(report.restored, [project.path("c.txt")]);
    assert_eq!(project.read_text("a.txt"), "quux\n");

    let report = undo_all(&project);
    assert_eq!(report.restored.len(), 2);
    for file in files {
        assert_eq!(project.read_text(file), "foo\n");
    }
}

#[test]
fn replaces_are_undone_most_recent_first() {
    let project = Project::new();
    project.write("file.txt", "foo\n");
    replace_foo(&project, &["file.txt"]);
    project
        .replace_literal("quux", "third", &["file.txt"])
        .finish(Vec::new(), Vec::new());
    assert_eq!(project.read_text("file.txt"), "third\n");

    let outcome = undo(&project, &[]).unwrap();
    assert_eq!(outcome.description.pattern, "quux");
    outcome.finish(&[], Vec::new());
    assert_eq!(project.read_text("file.txt"), "quux\n");

    let outcome = undo(&project, &[]).unwrap();
    assert_eq!(outcome.description.pattern, "foo");
    outcome.finish(&[], Vec::new());
    assert_eq!(project.read_text("file.txt"), "foo\n");

    assert!(undo(&project, &[]).is_none());
}

#[test]
fn an_older_replace_is_not_undone_over_a_newer_one() {
    let project = Project::new();
    project.write("file.txt", "foo\n");
    replace_foo(&project, &["file.txt"]);
    project
        .replace_literal("quux", "third", &["file.txt"])
        .finish(Vec::new(), Vec::new());

    // With the newer replace undone by hand in another way, its journal entry is refused, and
    // nothing older is touched in the same undo.
    project.write("file.txt", "something else\n");
    let report = undo_all(&project);
    assert_eq!(
        refused(&report),
        [("file.txt".to_string(), RefusalReason::EditedSinceReplace)]
    );
    assert_eq!(project.read_text("file.txt"), "something else\n");
}

#[test]
fn an_editor_that_still_holds_the_replaced_text_gets_its_text_back() {
    let project = Project::new();
    let path = project.write("open.txt", "foo saved\n");
    let unsaved = "foo saved\ntyped but not saved\n";
    let replaced = replace_foo_in_editor(&project, "open.txt", unsaved, true);
    assert_eq!(replaced, "quux saved\ntyped but not saved\n");
    assert_eq!(project.read_text("open.txt"), "foo saved\n");

    let outcome = undo(&project, &[open_buffer(&path, &replaced, true)]).unwrap();
    assert!(outcome.restored.is_empty());
    assert!(outcome.refused.is_empty());
    assert_eq!(outcome.buffer_restores.len(), 1);
    let restore = &outcome.buffer_restores[0];
    assert_eq!(restore.buffer_path, path);
    assert_eq!(restore.current_text, replaced);
    assert_eq!(restore.restored_text, unsaved);
    assert!(!restore.save, "intentionally dirty buffers stay unsaved");

    // Until the editor reports back, the entry stays in the journal.
    assert!(project.journal.latest().unwrap().is_some());
    let report = outcome.finish(std::slice::from_ref(&path), Vec::new());
    assert_eq!(report.restored, [path]);
    assert!(project.journal.latest().unwrap().is_none());
    assert_eq!(project.read_text("open.txt"), "foo saved\n");
}

#[test]
fn an_editor_that_could_not_restore_keeps_its_journal_entry() {
    let project = Project::new();
    let path = project.write("open.txt", "foo\n");
    let replaced = replace_foo_in_editor(&project, "open.txt", "foo\n", false);

    let outcome = undo(&project, &[open_buffer(&path, &replaced, false)]).unwrap();
    let report = outcome.finish(
        &[],
        vec![RefusedFile {
            path: path.clone(),
            reason: RefusalReason::EditedSinceReplace,
        }],
    );
    assert!(report.restored.is_empty());
    assert_eq!(report.refused.len(), 1);
    assert_eq!(project.journal.latest().unwrap().unwrap().entries.len(), 1);
}

#[test]
fn a_failed_buffer_save_is_reported_and_keeps_a_retryable_undo_copy() {
    let project = Project::new();
    let path = project.write("open.txt", "foo\n");
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["open.txt"]);
    let mut request = project.request(&spec, "quux", targets);
    request.open_buffers = vec![open_buffer(&path, "foo\n", false)];
    let outcome = project.replace(&request);
    let report = outcome.finish(
        vec![AppliedBufferEdits {
            path: path.clone(),
            text_before: "foo\n".into(),
            text_after: "quux\n".into(),
            occurrences: 1,
            save_error: Some("disk full".into()),
        }],
        Vec::new(),
    );
    assert!(report.replaced.is_empty());
    assert!(matches!(
        &report.skipped[0].reason,
        SkipReason::WriteFailed(_)
    ));
    assert!(report.can_undo);

    let outcome = undo(&project, &[open_buffer(&path, "quux\n", true)]).unwrap();
    assert!(outcome.buffer_restores[0].save);
    // Mutation succeeded but persistence failed: keep the entry even after the buffer
    // contains the original text, and retry the save on the next undo.
    let report = outcome.finish(
        &[],
        vec![RefusedFile {
            path: path.clone(),
            reason: RefusalReason::WriteFailed("disk full".into()),
        }],
    );
    assert!(report.restored.is_empty());
    let retry = undo(&project, &[open_buffer(&path, "foo\n", true)]).unwrap();
    assert!(retry.restored.is_empty());
    assert_eq!(retry.buffer_restores.len(), 1);
    assert!(retry.buffer_restores[0].save);
    assert_eq!(retry.buffer_restores[0].current_text, "foo\n");
    assert_eq!(retry.buffer_restores[0].restored_text, "foo\n");
    retry.finish(std::slice::from_ref(&path), Vec::new());
    assert!(project.journal.latest().unwrap().is_none());
}

#[test]
fn an_editor_edited_since_the_replace_is_refused() {
    let project = Project::new();
    let path = project.write("open.txt", "foo\n");
    replace_foo_in_editor(&project, "open.txt", "foo\n", false);

    let edited = open_buffer(&path, "quux\nand more\n", true);
    let outcome = undo(&project, &[edited]).unwrap();
    assert!(outcome.buffer_restores.is_empty());
    let report = outcome.finish(&[], Vec::new());
    assert_eq!(
        refused(&report),
        [("open.txt".to_string(), RefusalReason::EditedSinceReplace)]
    );
}

#[test]
fn an_editor_already_holding_the_original_text_counts_as_restored() {
    let project = Project::new();
    let path = project.write("open.txt", "foo\n");
    replace_foo_in_editor(&project, "open.txt", "foo\n", false);

    let outcome = undo(&project, &[open_buffer(&path, "foo\n", false)]).unwrap();
    assert!(outcome.buffer_restores.is_empty());
    assert_eq!(outcome.restored, [path]);
    outcome.finish(&[], Vec::new());
    assert!(project.journal.latest().unwrap().is_none());
}

#[test]
fn a_file_saved_and_closed_after_an_editor_replace_is_restored_on_disk() {
    let project = Project::new();
    project.write("unix.txt", "foo\nbar foo\n");
    project.write("windows.txt", "foo\r\nbar foo\r\n");

    // The editor holds both with `\n` and saved them with their own line endings.
    let replaced = replace_foo_in_editor(&project, "unix.txt", "foo\nbar foo\n", false);
    project.write("unix.txt", &replaced);
    let replaced = replace_foo_in_editor(&project, "windows.txt", "foo\nbar foo\n", false);
    project.write("windows.txt", replaced.replace('\n', "\r\n"));

    // Two replaces were made, so two undos take them back.
    let report = undo_all(&project);
    assert_eq!(report.restored, [project.path("windows.txt")]);
    let report = undo_all(&project);
    assert_eq!(report.restored, [project.path("unix.txt")]);

    assert_eq!(project.read_text("unix.txt"), "foo\nbar foo\n");
    assert_eq!(project.read_text("windows.txt"), "foo\r\nbar foo\r\n");
}

#[test]
fn a_file_closed_without_saving_after_an_editor_replace_has_nothing_to_undo() {
    let project = Project::new();
    project.write("open.txt", "foo saved\n");
    replace_foo_in_editor(&project, "open.txt", "foo saved\nunsaved line\n", true);

    // The editor was closed and its text, replacement included, thrown away.
    let report = undo_all(&project);
    assert_eq!(
        refused(&report),
        [("open.txt".to_string(), RefusalReason::EditedSinceReplace)]
    );
    assert_eq!(project.read_text("open.txt"), "foo saved\n");
}

#[test]
fn a_file_opened_after_the_replace_is_restored_unless_it_has_unsaved_changes() {
    let project = Project::new();
    let clean = project.write("clean.txt", "foo\n");
    let dirty = project.write("dirty.txt", "foo\n");
    replace_foo(&project, &["clean.txt", "dirty.txt"]);

    let open_buffers = [
        open_buffer(&clean, "quux\n", false),
        open_buffer(&dirty, "quux\ntyped\n", true),
    ];
    let outcome = undo(&project, &open_buffers).unwrap();
    assert!(outcome.buffer_restores.is_empty());
    let report = outcome.finish(&[], Vec::new());

    assert_eq!(report.restored, [clean]);
    assert_eq!(
        refused(&report),
        [("dirty.txt".to_string(), RefusalReason::UnsavedChanges)]
    );
    assert_eq!(project.read_text("clean.txt"), "foo\n");
    assert_eq!(project.read_text("dirty.txt"), "quux\n");
}

#[test]
fn progress_is_reported_for_every_file() {
    let project = Project::new();
    let files = ["a.txt", "b.txt"];
    for file in files {
        project.write(file, "foo\n");
    }
    replace_foo(&project, &files);

    let reported = std::cell::RefCell::new(Vec::new());
    undo_latest(
        &project.journal,
        &[],
        &|progress| reported.borrow_mut().push((progress.done, progress.total)),
        &AtomicBool::new(false),
    )
    .unwrap()
    .unwrap()
    .finish(&[], Vec::new());
    assert_eq!(reported.into_inner(), [(1, 2), (2, 2)]);
}

#[test]
fn undo_refuses_a_file_that_grew_beyond_the_read_budget() {
    for was_open in [false, true] {
        let project = Project::new();
        let path = project.write("file.txt", "foo\n");
        if was_open {
            replace_foo_in_editor(&project, "file.txt", "foo\n", false);
        } else {
            replace_foo(&project, &["file.txt"]);
        }
        let huge_size = crate::journal::JournalLimits::default().max_transaction_bytes + 1;
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(huge_size)
            .unwrap();
        let report = undo_all(&project);
        assert!(report.restored.is_empty());
        assert_eq!(report.refused.len(), 1);
        assert_eq!(fs::metadata(path).unwrap().len(), huge_size);
    }
}

#[cfg(any(unix, windows))]
#[test]
fn undo_does_not_dispatch_a_buffer_save_after_a_hard_link_was_added() {
    for originally_dirty in [false, true] {
        let project = Project::new();
        let path = project.write("file.txt", "foo\n");
        let replaced = replace_foo_in_editor(&project, "file.txt", "foo\n", originally_dirty);
        std::fs::write(&path, &replaced).unwrap();
        std::fs::hard_link(&path, project.path("alias.txt")).unwrap();
        let outcome = undo(&project, &[open_buffer(&path, &replaced, false)]).unwrap();
        assert!(outcome.buffer_restores.is_empty());
        assert_eq!(outcome.refused.len(), 1);
        outcome.finish(&[], Vec::new());
        assert_eq!(project.read_text("alias.txt"), replaced);
        assert!(project.journal.latest().unwrap().is_some());
    }
}
