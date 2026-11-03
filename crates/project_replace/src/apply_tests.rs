use std::cell::RefCell;

use super::*;
use crate::journal::JournalLimits;
use crate::selection::{Dismissals, Selection};
use crate::test_support::{Project, literal, open_buffer, regex};

fn skipped(outcome: &ReplaceOutcome) -> Vec<(String, SkipReason)> {
    outcome
        .skipped
        .iter()
        .map(|skipped| {
            let name = skipped.path.file_name().unwrap().to_str().unwrap();
            (name.to_string(), skipped.reason.clone())
        })
        .collect()
}

fn finish(outcome: ReplaceOutcome) -> ReplaceReport {
    outcome.finish(Vec::new(), Vec::new())
}

#[test]
fn every_match_in_every_file_is_replaced() {
    let project = Project::new();
    project.write("a.txt", "foo one\ntwo foo foo\n");
    project.write("sub/b.txt", "nothing here\nfoo\n");
    project.write("c.txt", "no match\n");

    let outcome = project.replace_literal("foo", "quux", &["a.txt", "sub/b.txt", "c.txt"]);
    assert!(outcome.skipped.is_empty());
    assert!(outcome.buffer_edits.is_empty());
    assert_eq!(outcome.not_attempted, 0);

    assert_eq!(project.read_text("a.txt"), "quux one\ntwo quux quux\n");
    assert_eq!(project.read_text("sub/b.txt"), "nothing here\nquux\n");
    assert_eq!(project.read_text("c.txt"), "no match\n");

    let report = finish(outcome);
    assert_eq!(
        report.counts(),
        ReplaceCounts {
            occurrences: 4,
            files: 2
        }
    );
    assert!(report.can_undo);
    assert!(!report.was_cancelled());
    assert_eq!(project.file_names(), ["a.txt", "c.txt", "sub"]);
}

#[test]
fn a_file_changed_since_the_search_is_left_alone() {
    let project = Project::new();
    project.write("stale.txt", "foo\nbar foo\n");
    project.write("fresh.txt", "foo\n");
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["stale.txt", "fresh.txt"]);

    // A line added above the matches moves them, so they no longer line up.
    project.write("stale.txt", "added\nfoo\nbar foo\n");
    let outcome = project.replace(&project.request(&spec, "quux", targets));

    assert_eq!(
        skipped(&outcome),
        [("stale.txt".to_string(), SkipReason::ChangedSinceSearch)]
    );
    assert_eq!(project.read_text("stale.txt"), "added\nfoo\nbar foo\n");
    assert_eq!(project.read_text("fresh.txt"), "quux\n");
}

#[test]
fn every_kind_of_drift_since_the_search_is_caught() {
    let original = "alpha foo\nbeta\ngamma foo\n";
    let drifted = [
        // A match was removed.
        "alpha foo\nbeta\ngamma\n",
        // A match was added.
        "alpha foo\nbeta foo\ngamma foo\n",
        // A match moved along its line.
        "alpha  foo\nbeta\ngamma foo\n",
        // A match moved to another line.
        "alpha foo\ngamma foo\nbeta\n",
        // Every match is gone.
        "alpha\nbeta\ngamma\n",
        "",
    ];
    for content in drifted {
        let project = Project::new();
        project.write("file.txt", original);
        let spec = literal("foo");
        let targets = project.search_all(&spec, &["file.txt"]);

        project.write("file.txt", content);
        let outcome = project.replace(&project.request(&spec, "quux", targets));
        assert_eq!(
            skipped(&outcome),
            [("file.txt".to_string(), SkipReason::ChangedSinceSearch)],
            "content {content:?}"
        );
        assert_eq!(project.read_text("file.txt"), content);
        assert!(!finish(outcome).can_undo);
    }
}

#[test]
fn edits_that_leave_the_matches_in_place_do_not_block_the_replace() {
    let project = Project::new();
    project.write("file.txt", "alpha foo tail\nbeta\n");
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["file.txt"]);

    project.write("file.txt", "alpha foo changed tail\nbeta\nappended line\n");
    let outcome = project.replace(&project.request(&spec, "quux", targets));
    assert!(outcome.skipped.is_empty());
    assert_eq!(
        project.read_text("file.txt"),
        "alpha quux changed tail\nbeta\nappended line\n"
    );
}

#[test]
fn matched_text_that_changed_case_is_drift() {
    let project = Project::new();
    project.write("file.txt", "Foo here\n");
    let spec = SearchSpec {
        pattern: "foo".to_string(),
        ..Default::default()
    };
    let targets = project.search_all(&spec, &["file.txt"]);

    project.write("file.txt", "FOO here\n");
    let outcome = project.replace(&project.request(&spec, "quux", targets));
    assert_eq!(
        skipped(&outcome),
        [("file.txt".to_string(), SkipReason::ChangedSinceSearch)]
    );
    assert_eq!(project.read_text("file.txt"), "FOO here\n");
}

#[test]
fn line_endings_and_the_last_line_are_kept_as_they_are() {
    let cases = [
        ("unix.txt", "foo\nbar foo\n", "quux\nbar quux\n"),
        ("windows.txt", "foo\r\nbar foo\r\n", "quux\r\nbar quux\r\n"),
        (
            "mixed.txt",
            "foo\r\nbar foo\nfoo\r",
            "quux\r\nbar quux\nquux\r",
        ),
        ("no-newline.txt", "foo\nbar foo", "quux\nbar quux"),
        ("windows-no-newline.txt", "foo\r\nfoo", "quux\r\nquux"),
        ("blank-lines.txt", "\n\nfoo\n\n\n", "\n\nquux\n\n\n"),
        ("only.txt", "foo", "quux"),
    ];
    let project = Project::new();
    let names: Vec<&str> = cases.iter().map(|(name, _, _)| *name).collect();
    for (name, before, _) in cases {
        project.write(name, before);
    }

    let outcome = project.replace_literal("foo", "quux", &names);
    assert!(outcome.skipped.is_empty());
    for (name, _, after) in cases {
        assert_eq!(project.read_text(name), after, "{name}");
    }
}

#[test]
fn the_byte_order_mark_and_the_encoding_are_kept() {
    let text = "foo first\r\nsecond foo é\r\n";
    let replaced = "quux first\r\nsecond quux é\r\n";
    let utf16 = |text: &str, bom: [u8; 2], bytes: fn(u16) -> [u8; 2]| {
        let mut encoded = bom.to_vec();
        encoded.extend(text.encode_utf16().flat_map(bytes));
        encoded
    };

    let project = Project::new();
    project.write(
        "utf8-bom.txt",
        [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
    );
    project.write("utf16-le.txt", utf16(text, [0xFF, 0xFE], u16::to_le_bytes));
    project.write("utf16-be.txt", utf16(text, [0xFE, 0xFF], u16::to_be_bytes));

    let outcome = project.replace_literal(
        "foo",
        "quux",
        &["utf8-bom.txt", "utf16-le.txt", "utf16-be.txt"],
    );
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    assert_eq!(outcome.replaced.len(), 3);

    assert_eq!(
        project.read("utf8-bom.txt"),
        [&[0xEF, 0xBB, 0xBF][..], replaced.as_bytes()].concat()
    );
    assert_eq!(
        project.read("utf16-le.txt"),
        utf16(replaced, [0xFF, 0xFE], u16::to_le_bytes)
    );
    assert_eq!(
        project.read("utf16-be.txt"),
        utf16(replaced, [0xFE, 0xFF], u16::to_be_bytes)
    );
}

#[cfg(unix)]
#[test]
fn permissions_are_kept() {
    use std::os::unix::fs::PermissionsExt;

    let project = Project::new();
    let script = project.write("run.sh", "#!/bin/sh\necho foo\n");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o750)).unwrap();

    let outcome = project.replace_literal("foo", "quux", &["run.sh"]);
    assert!(outcome.skipped.is_empty());
    assert_eq!(project.read_text("run.sh"), "#!/bin/sh\necho quux\n");
    let mode = fs::metadata(&script).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o750);
}

#[test]
fn binary_files_are_left_alone() {
    let project = Project::new();
    project.write("image.bin", "foo\n");
    project.write(
        "latin1.txt",
        [b"foo\ncaf".as_slice(), &[0xE9], b"\n"].concat(),
    );
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["image.bin", "latin1.txt"]);
    assert_eq!(targets.len(), 2);

    // The search reports nothing for a file with a NUL in it, so one only gets here by
    // turning binary after the search.
    project.write("image.bin", b"foo\0\x01\x02 foo\n");
    let outcome = project.replace(&project.request(&spec, "quux", targets));
    assert_eq!(
        skipped(&outcome),
        [
            ("image.bin".to_string(), SkipReason::Binary),
            ("latin1.txt".to_string(), SkipReason::UnsupportedEncoding),
        ]
    );
    assert_eq!(project.read("image.bin"), b"foo\0\x01\x02 foo\n");
    assert!(outcome.replaced.is_empty());
}

#[cfg(unix)]
#[test]
fn symbolic_links_and_hard_links_are_left_alone() {
    let project = Project::new();
    let real = project.write("real.txt", "foo\n");
    let shared = project.write("shared.txt", "foo\n");
    std::os::unix::fs::symlink(&real, project.path("link.txt")).unwrap();
    fs::hard_link(&shared, project.path("other-name.txt")).unwrap();

    let outcome = project.replace_literal("foo", "quux", &["link.txt", "shared.txt"]);
    assert_eq!(
        skipped(&outcome),
        [
            ("link.txt".to_string(), SkipReason::Symlink),
            ("shared.txt".to_string(), SkipReason::HardLinked),
        ]
    );
    assert_eq!(project.read_text("real.txt"), "foo\n");
    assert_eq!(project.read_text("other-name.txt"), "foo\n");
}

#[test]
fn files_over_the_size_limit_are_left_alone() {
    let project = Project::new();
    project.write("big.txt", "foo\n".repeat(100));
    project.write("small.txt", "foo\n");
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["big.txt", "small.txt"]);

    let mut request = project.request(&spec, "quux", targets);
    request.max_file_bytes = 399;
    let outcome = project.replace(&request);

    assert_eq!(
        skipped(&outcome),
        [(
            "big.txt".to_string(),
            SkipReason::TooLarge {
                bytes: 400,
                limit: 399
            }
        )]
    );
    assert_eq!(project.read_text("big.txt"), "foo\n".repeat(100));
    assert_eq!(project.read_text("small.txt"), "quux\n");
}

#[test]
fn read_only_and_missing_files_are_left_alone() {
    let project = Project::new();
    let locked = project.write("locked.txt", "foo\n");
    let gone = project.write("gone.txt", "foo\n");
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["locked.txt", "gone.txt"]);

    let mut permissions = fs::metadata(&locked).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&locked, permissions).unwrap();
    fs::remove_file(&gone).unwrap();

    let outcome = project.replace(&project.request(&spec, "quux", targets));
    assert_eq!(
        skipped(&outcome),
        [
            ("locked.txt".to_string(), SkipReason::ReadOnly),
            ("gone.txt".to_string(), SkipReason::Missing),
        ]
    );
    assert_eq!(project.read_text("locked.txt"), "foo\n");
}

#[test]
fn files_outside_the_searched_folders_are_never_touched() {
    let project = Project::new();
    project.write("inside.txt", "foo\n");
    let outside_directory = tempfile::tempdir().unwrap();
    let outside = outside_directory.path().join("outside.txt");
    fs::write(&outside, "foo\n").unwrap();

    let spec = literal("foo");
    let mut targets = project.search_all(&spec, &["inside.txt"]);
    let mut stray = targets[0].clone();
    stray.path = outside.clone();
    targets.push(stray);

    let outcome = project.replace(&project.request(&spec, "quux", targets));
    assert_eq!(
        skipped(&outcome),
        [("outside.txt".to_string(), SkipReason::OutOfScope)]
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), "foo\n");
    assert_eq!(project.read_text("inside.txt"), "quux\n");
}

#[test]
fn include_and_exclude_patterns_limit_what_is_replaced() {
    let project = Project::new();
    let files = [
        "src/a.ts",
        "src/deep/b.ts",
        "src/c.js",
        "src/dist/d.ts",
        "lib/e.ts",
    ];
    for file in files {
        project.write(file, "foo\n");
    }
    let spec = literal("foo");
    let targets = project.search_all(&spec, &files);

    let mut request = project.request(&spec, "quux", targets);
    request.scope = ScopeGlobs {
        include: "src/**/*.ts".to_string(),
        exclude: "**/dist/**".to_string(),
    };
    let outcome = project.replace(&request);

    let replaced: Vec<_> = files
        .iter()
        .filter(|file| project.read_text(file) == "quux\n")
        .collect();
    assert_eq!(replaced, [&"src/a.ts", &"src/deep/b.ts"]);
    assert_eq!(outcome.skipped.len(), 3);
    assert!(
        outcome
            .skipped
            .iter()
            .all(|skipped| skipped.reason == SkipReason::OutOfScope)
    );
}

#[test]
fn an_invalid_pattern_or_scope_replaces_nothing() {
    let project = Project::new();
    project.write("file.txt", "foo\n");
    let targets = project.search_all(&literal("foo"), &["file.txt"]);

    let request = project.request(&regex("foo(?=bar)"), "quux", targets.clone());
    let result = replace_in_files(&request, &project.journal, &|_| {}, &AtomicBool::new(false));
    assert!(matches!(result, Err(ReplaceError::Pattern(_))));

    let mut request = project.request(&literal("foo"), "quux", targets);
    request.scope.include = "src/[z-a]".to_string();
    let result = replace_in_files(&request, &project.journal, &|_| {}, &AtomicBool::new(false));
    assert!(matches!(result, Err(ReplaceError::Scope(_))));

    assert_eq!(project.read_text("file.txt"), "foo\n");
    assert!(project.journal.latest().unwrap().is_none());
}

#[test]
fn matches_set_aside_are_not_replaced() {
    let project = Project::new();
    let kept = project.write("kept.txt", "foo one\nfoo two\nfoo three\n");
    let dismissed = project.write("dismissed.txt", "foo\n");
    let spec = literal("foo");

    let mut dismissals = Dismissals::default();
    dismissals.toggle_match(&kept, 2, 1);
    dismissals.toggle_file(&dismissed);
    let targets = project.search(
        &spec,
        &["kept.txt", "dismissed.txt"],
        &dismissals,
        Selection::All,
    );
    assert_eq!(targets.len(), 1);

    let outcome = project.replace(&project.request(&spec, "quux", targets));
    assert!(outcome.skipped.is_empty());
    assert_eq!(
        project.read_text("kept.txt"),
        "quux one\nfoo two\nquux three\n"
    );
    assert_eq!(project.read_text("dismissed.txt"), "foo\n");
    assert_eq!(finish(outcome).counts().occurrences, 2);
}

#[test]
fn one_match_or_one_file_can_be_replaced_on_its_own() {
    let project = Project::new();
    let first = project.write("first.txt", "foo foo\nfoo\n");
    project.write("second.txt", "foo\n");
    let spec = literal("foo");
    let files = ["first.txt", "second.txt"];

    let one_match = project.search(
        &spec,
        &files,
        &Dismissals::default(),
        Selection::Match {
            path: &first,
            line: 1,
            column: 5,
        },
    );
    let outcome = project.replace(&project.request(&spec, "quux", one_match));
    assert!(outcome.skipped.is_empty());
    assert_eq!(project.read_text("first.txt"), "foo quux\nfoo\n");
    assert_eq!(project.read_text("second.txt"), "foo\n");

    let one_file = project.search(
        &spec,
        &files,
        &Dismissals::default(),
        Selection::File(&first),
    );
    let outcome = project.replace(&project.request(&spec, "quux", one_file));
    assert!(outcome.skipped.is_empty());
    assert_eq!(project.read_text("first.txt"), "quux quux\nquux\n");
    assert_eq!(project.read_text("second.txt"), "foo\n");
}

#[test]
fn capture_groups_whole_words_and_case_carry_through_to_the_file() {
    let project = Project::new();
    project.write("pairs.txt", "width=10 height=20\n");
    project.write("words.txt", "cat concat cat_ (cat) Cat\n");
    project.write("case.txt", "begin Begin BEGIN beginning\n");

    let spec = regex(r"(\w+)=(\d+)");
    let targets = project.search_all(&spec, &["pairs.txt"]);
    project.replace(&project.request(&spec, "$2:$1", targets));
    assert_eq!(project.read_text("pairs.txt"), "10:width 20:height\n");

    let spec = SearchSpec {
        whole_word: true,
        ..literal("cat")
    };
    let targets = project.search_all(&spec, &["words.txt"]);
    project.replace(&project.request(&spec, "dog", targets));
    assert_eq!(
        project.read_text("words.txt"),
        "dog concat cat_ (dog) Cat\n"
    );

    let spec = SearchSpec {
        pattern: "begin".to_string(),
        whole_word: true,
        ..Default::default()
    };
    let targets = project.search_all(&spec, &["case.txt"]);
    let mut request = project.request(&spec, "end", targets);
    request.replace.preserve_case = true;
    project.replace(&request);
    assert_eq!(project.read_text("case.txt"), "end End END beginning\n");
}

#[test]
fn a_dollar_sign_is_literal_outside_regex_mode() {
    let project = Project::new();
    project.write("prices.txt", "cost: PRICE\n");
    let outcome = project.replace_literal("PRICE", "$1.50 ${x} $$", &["prices.txt"]);
    assert!(outcome.skipped.is_empty());
    assert_eq!(project.read_text("prices.txt"), "cost: $1.50 ${x} $$\n");
}

#[test]
fn matches_across_lines_are_replaced() {
    let project = Project::new();
    project.write("file.txt", "start\nfoo\nbar\nmiddle\nfoo\nbar\nend\n");
    let spec = literal("foo\nbar");
    let targets = project.search_all(&spec, &["file.txt"]);
    assert_eq!(targets[0].expectation.matches.len(), 2);

    let outcome = project.replace(&project.request(&spec, "joined", targets));
    assert!(outcome.skipped.is_empty());
    assert_eq!(
        project.read_text("file.txt"),
        "start\njoined\nmiddle\njoined\nend\n"
    );
}

#[test]
fn an_open_file_is_handed_to_its_editor_instead_of_being_written() {
    let project = Project::new();
    let path = project.write("open.txt", "foo saved\nfoo too\n");
    project.write("closed.txt", "foo\n");
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["open.txt", "closed.txt"]);

    let mut request = project.request(&spec, "quux", targets);
    request.open_buffers = vec![open_buffer(&path, "foo saved\nfoo too\n", false)];
    let outcome = project.replace(&request);

    assert!(outcome.skipped.is_empty());
    assert_eq!(project.read_text("open.txt"), "foo saved\nfoo too\n");
    assert_eq!(project.read_text("closed.txt"), "quux\n");
    assert_eq!(outcome.buffer_edits.len(), 1);

    let edits = &outcome.buffer_edits[0];
    assert_eq!(edits.path, path);
    assert_eq!(edits.buffer_path, path);
    let text_after = apply_edits("foo saved\nfoo too\n", &edits.edits);
    assert_eq!(text_after, "quux saved\nquux too\n");

    let report = outcome.finish(
        vec![AppliedBufferEdits {
            path: path.clone(),
            text_before: "foo saved\nfoo too\n".to_string(),
            text_after,
            occurrences: 2,
            save_error: None,
        }],
        Vec::new(),
    );
    assert_eq!(
        report.counts(),
        ReplaceCounts {
            occurrences: 3,
            files: 2
        }
    );
    let transaction = project.journal.latest().unwrap().unwrap();
    assert_eq!(transaction.entries.len(), 2);
    // Buffer copies are now journaled before editing, so entry order follows planning.
    let entry = transaction
        .entries
        .iter()
        .find(|entry| entry.path == path)
        .unwrap();
    // Undo of an originally clean editor must also wait for its disk save.
    assert_eq!(entry.kind, EntryKind::SavedBuffer);
}

#[test]
fn an_open_file_with_unsaved_changes_is_checked_against_its_editor_text() {
    let project = Project::new();
    let path = project.write("open.txt", "foo\nbar foo\n");
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["open.txt"]);

    // The unsaved text moved the matches, so they no longer line up with the search.
    let mut request = project.request(&spec, "quux", targets.clone());
    request.open_buffers = vec![open_buffer(&path, "typed\nfoo\nbar foo\n", true)];
    let outcome = project.replace(&request);
    assert_eq!(
        skipped(&outcome),
        [("open.txt".to_string(), SkipReason::ChangedSinceSearch)]
    );
    assert!(outcome.buffer_edits.is_empty());
    assert_eq!(project.read_text("open.txt"), "foo\nbar foo\n");

    // Unsaved text that leaves the matches in place is edited, still without touching the file.
    let unsaved = "foo\nbar foo\ntyped below\n";
    let mut request = project.request(&spec, "quux", targets);
    request.open_buffers = vec![open_buffer(&path, unsaved, true)];
    let outcome = project.replace(&request);
    assert!(outcome.skipped.is_empty());
    assert_eq!(
        apply_edits(unsaved, &outcome.buffer_edits[0].edits),
        "quux\nbar quux\ntyped below\n"
    );
    assert_eq!(project.read_text("open.txt"), "foo\nbar foo\n");
}

#[test]
fn an_open_file_is_matched_despite_line_endings_and_a_byte_order_mark() {
    let project = Project::new();
    let path = project.write(
        "open.txt",
        [&[0xEF, 0xBB, 0xBF][..], b"foo one\r\nbar foo\r\n"].concat(),
    );
    let spec = regex("foo.*$");
    let targets = project.search_all(&spec, &["open.txt"]);
    // On disk the first match runs up to the carriage return.
    assert_eq!(targets[0].expectation.matches[0].matched, "foo one\r");

    // The editor holds the mark as a character and the lines without carriage returns.
    let editor_text = "\u{feff}foo one\nbar foo\n";
    let mut request = project.request(&spec, "quux", targets);
    request.open_buffers = vec![open_buffer(&path, editor_text, false)];
    let outcome = project.replace(&request);

    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    assert_eq!(
        apply_edits(editor_text, &outcome.buffer_edits[0].edits),
        "\u{feff}quux\nbar quux\n"
    );
}

#[cfg(unix)]
#[test]
fn an_open_file_is_recognized_through_another_path_to_it() {
    let project = Project::new();
    project.write("real/open.txt", "foo\n");
    std::os::unix::fs::symlink(project.path("real"), project.path("alias")).unwrap();
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["real/open.txt"]);

    let editor_path = project.path("alias/open.txt");
    let mut request = project.request(&spec, "quux", targets);
    request.open_buffers = vec![open_buffer(&editor_path, "foo\n", true)];
    let outcome = project.replace(&request);

    assert_eq!(project.read_text("real/open.txt"), "foo\n");
    assert_eq!(outcome.buffer_edits.len(), 1);
    assert_eq!(outcome.buffer_edits[0].buffer_path, editor_path);
    assert_eq!(outcome.buffer_edits[0].path, project.path("real/open.txt"));
}

#[test]
fn an_editor_that_could_not_apply_its_edits_is_reported() {
    let project = Project::new();
    let path = project.write("open.txt", "foo\n");
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["open.txt"]);
    let mut request = project.request(&spec, "quux", targets);
    request.open_buffers = vec![open_buffer(&path, "foo\n", false)];
    let outcome = project.replace(&request);
    assert_eq!(outcome.buffer_edits.len(), 1);

    let report = outcome.finish(
        Vec::new(),
        vec![SkippedFile {
            path: path.clone(),
            reason: SkipReason::EditedWhileReplacing,
        }],
    );
    assert!(report.replaced.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert!(!report.can_undo);
    assert!(project.journal.latest().unwrap().is_none());
}

#[test]
fn progress_is_reported_for_every_file() {
    let project = Project::new();
    let files = ["a.txt", "b.txt", "c.txt"];
    for file in files {
        project.write(file, "foo\n");
    }
    let spec = literal("foo");
    let targets = project.search_all(&spec, &files);

    let reported = RefCell::new(Vec::new());
    replace_in_files(
        &project.request(&spec, "quux", targets),
        &project.journal,
        &|progress| reported.borrow_mut().push((progress.done, progress.total)),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(reported.into_inner(), [(1, 3), (2, 3), (3, 3)]);
}

#[test]
fn cancelling_stops_before_the_next_file() {
    let project = Project::new();
    let files = ["a.txt", "b.txt", "c.txt", "d.txt"];
    for file in files {
        project.write(file, "foo\n");
    }
    let spec = literal("foo");
    let targets = project.search_all(&spec, &files);

    let cancel = AtomicBool::new(false);
    let outcome = replace_in_files(
        &project.request(&spec, "quux", targets),
        &project.journal,
        &|progress| {
            if progress.done == 2 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        &cancel,
    )
    .unwrap();

    assert_eq!(outcome.not_attempted, 2);
    assert!(outcome.skipped.is_empty());
    assert_eq!(project.read_text("a.txt"), "quux\n");
    assert_eq!(project.read_text("b.txt"), "quux\n");
    assert_eq!(project.read_text("c.txt"), "foo\n");
    assert_eq!(project.read_text("d.txt"), "foo\n");

    let report = finish(outcome);
    assert!(report.was_cancelled());
    assert!(report.can_undo);
    assert_eq!(project.journal.latest().unwrap().unwrap().entries.len(), 2);
}

#[test]
fn files_the_journal_has_no_room_for_are_not_replaced() {
    let limits = JournalLimits {
        max_transaction_bytes: 10,
        ..Default::default()
    };
    let project = Project::with_limits(limits);
    let files = ["a.txt", "b.txt", "c.txt"];
    for file in files {
        project.write(file, "foo\n");
    }
    let open = project.write("open.txt", "foo\n");
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["a.txt", "b.txt", "c.txt", "open.txt"]);

    let mut request = project.request(&spec, "quux", targets);
    request.open_buffers = vec![open_buffer(&open, "foo\n", false)];
    let outcome = project.replace(&request);

    // Each copy takes four bytes, so the journal holds two of them.
    assert_eq!(
        skipped(&outcome),
        [
            ("c.txt".to_string(), SkipReason::JournalFull),
            ("open.txt".to_string(), SkipReason::JournalFull),
        ]
    );
    assert_eq!(project.read_text("a.txt"), "quux\n");
    assert_eq!(project.read_text("b.txt"), "quux\n");
    assert_eq!(project.read_text("c.txt"), "foo\n");
    assert!(outcome.buffer_edits.is_empty());
}

#[test]
fn the_copy_is_in_the_journal_before_the_file_changes() {
    let project = Project::new();
    project.write("file.txt", "foo original\n");
    let outcome = project.replace_literal("foo", "quux", &["file.txt"]);
    // The journal is readable while the replace is still open, as it would be after a crash.
    let transaction = project.journal.latest().unwrap().unwrap();
    assert_eq!(transaction.entries.len(), 1);
    assert_eq!(
        transaction.pre_image(&transaction.entries[0]).unwrap(),
        b"foo original\n"
    );
    assert_eq!(
        transaction.entries[0].post_sha256,
        sha256_hex(b"quux original\n")
    );
    drop(outcome);
}

#[test]
fn many_files_are_replaced_without_leaving_anything_behind() {
    let project = Project::new();
    let names: Vec<String> = (0..150)
        .map(|index| format!("file-{index:03}.txt"))
        .collect();
    for name in &names {
        project.write(name, format!("{name} foo\nsecond foo line\n"));
    }
    let files: Vec<&str> = names.iter().map(String::as_str).collect();

    let outcome = project.replace_literal("foo", "quux", &files);
    assert!(outcome.skipped.is_empty());
    assert_eq!(outcome.replaced.len(), 150);
    for name in &names {
        assert_eq!(
            project.read_text(name),
            format!("{name} quux\nsecond quux line\n")
        );
    }
    assert_eq!(project.file_names(), names);
    assert_eq!(
        finish(outcome).counts(),
        ReplaceCounts {
            occurrences: 300,
            files: 150
        }
    );
}

#[test]
fn a_replacement_equal_to_the_match_changes_nothing() {
    let project = Project::new();
    project.write("file.txt", "foo\n");
    let open = project.write("open.txt", "foo\n");
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["file.txt", "open.txt"]);

    let mut request = project.request(&spec, "foo", targets);
    request.open_buffers = vec![open_buffer(&open, "foo\n", false)];
    let outcome = project.replace(&request);

    assert!(outcome.replaced.is_empty());
    assert!(outcome.buffer_edits.is_empty());
    assert!(outcome.skipped.is_empty());
    assert!(!finish(outcome).can_undo);
    assert!(project.journal.latest().unwrap().is_none());
}

#[test]
fn an_open_buffers_undo_copy_exists_before_the_editor_changes_it() {
    let project = Project::new();
    let path = project.write("open.txt", "foo\n");
    let spec = literal("foo");
    let mut request = project.request(&spec, "quux", project.search_all(&spec, &["open.txt"]));
    request.open_buffers = vec![open_buffer(&path, "foo\n", true)];
    let outcome = project.replace(&request);
    assert_eq!(outcome.buffer_edits.len(), 1);
    let transaction = project.journal.latest().unwrap().unwrap();
    assert_eq!(
        transaction.pre_image(&transaction.entries[0]).unwrap(),
        b"foo\n"
    );
    assert_eq!(transaction.entries[0].post_sha256, sha256_hex(b"quux\n"));
    // Losing the app callback must not lose the only undo copy.
    drop(outcome);
    assert!(project.journal.latest().unwrap().is_some());
}

#[test]
fn oversized_clean_and_dirty_open_buffers_are_skipped_before_matching() {
    for dirty in [false, true] {
        let project = Project::new();
        let path = project.write("open.txt", "foo\n");
        let spec = literal("foo");
        let targets = project.search_all(&spec, &["open.txt"]);
        let text = format!("foo\n{}", "x".repeat(DEFAULT_MAX_FILE_BYTES as usize));
        let mut request = project.request(&spec, "bar", targets);
        request.open_buffers = vec![open_buffer(&path, &text, dirty)];
        let outcome = project.replace(&request);
        assert!(outcome.buffer_edits.is_empty());
        assert_eq!(
            skipped(&outcome),
            vec![(
                "open.txt".to_owned(),
                SkipReason::TooLarge {
                    bytes: text.len() as u64,
                    limit: DEFAULT_MAX_FILE_BYTES,
                }
            )]
        );
        assert_eq!(project.read_text("open.txt"), "foo\n");
        assert!(!finish(outcome).can_undo);
    }
}

#[test]
fn oversized_replacement_output_leaves_disk_and_journal_untouched() {
    let project = Project::new();
    project.write("a.txt", "foo\n");
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["a.txt"]);
    let mut request = project.request(&spec, &"x".repeat(256), targets);
    request.max_file_bytes = 128;
    let outcome = project.replace(&request);
    assert!(matches!(
        outcome.skipped[0].reason,
        SkipReason::TooLarge { .. }
    ));
    assert_eq!(project.read_text("a.txt"), "foo\n");
    assert!(!finish(outcome).can_undo);
}
