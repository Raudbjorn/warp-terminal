use super::*;

fn description(pattern: &str) -> TransactionDescription {
    TransactionDescription {
        pattern: pattern.to_string(),
        replacement: "new".to_string(),
    }
}

fn journal_in(directory: &Path, limits: JournalLimits) -> Journal {
    Journal::new(directory.join("journal"), limits)
}

/// Records one file in a new transaction and returns the id of the transaction.
fn record_one(journal: &Journal, pattern: &str, pre_image: &[u8]) -> String {
    let mut writer = journal.begin(description(pattern)).unwrap();
    writer
        .record(
            Path::new("/project/file.txt"),
            EntryKind::Disk,
            pre_image,
            sha256_hex(b"after"),
            1,
        )
        .unwrap();
    writer
        .finish()
        .unwrap()
        .expect("the transaction has an entry")
}

#[test]
fn a_transaction_is_read_back_with_its_copies() {
    let directory = tempfile::tempdir().unwrap();
    let journal = journal_in(directory.path(), JournalLimits::default());

    let mut writer = journal.begin(description("old")).unwrap();
    let first = writer
        .record(
            Path::new("/project/a.txt"),
            EntryKind::Disk,
            b"old a",
            sha256_hex(b"new a"),
            2,
        )
        .unwrap();
    writer.reserve(5).unwrap();
    let second = writer
        .record_reserved(
            Path::new("/project/b.txt"),
            EntryKind::Buffer,
            b"old b",
            sha256_hex(b"new b"),
            1,
        )
        .unwrap();
    let id = writer.finish().unwrap().unwrap();

    let transaction = journal.latest().unwrap().unwrap();
    assert_eq!(transaction.id, id);
    assert_eq!(transaction.description, description("old"));
    assert_eq!(transaction.entries.len(), 2);

    let entry = &transaction.entries[0];
    assert_eq!(entry.index, first);
    assert_eq!(entry.path, Path::new("/project/a.txt"));
    assert_eq!(entry.kind, EntryKind::Disk);
    assert_eq!(entry.occurrences, 2);
    assert_eq!(entry.pre_sha256, sha256_hex(b"old a"));
    assert_eq!(entry.post_sha256, sha256_hex(b"new a"));
    assert_eq!(transaction.pre_image(entry).unwrap(), b"old a");

    let entry = &transaction.entries[1];
    assert_eq!(entry.index, second);
    assert_eq!(entry.kind, EntryKind::Buffer);
    assert_eq!(transaction.pre_image(entry).unwrap(), b"old b");
}

#[test]
fn a_transaction_that_changed_nothing_leaves_no_trace() {
    let directory = tempfile::tempdir().unwrap();
    let journal = journal_in(directory.path(), JournalLimits::default());

    let writer = journal.begin(description("old")).unwrap();
    assert_eq!(writer.finish().unwrap(), None);
    assert!(journal.latest().unwrap().is_none());
    assert_eq!(journal.transaction_ids(), Vec::<String>::new());

    let mut writer = journal.begin(description("old")).unwrap();
    let index = writer
        .record(
            Path::new("/project/a.txt"),
            EntryKind::Disk,
            b"old",
            sha256_hex(b"new"),
            1,
        )
        .unwrap();
    writer.retract(index).unwrap();
    assert_eq!(writer.finish().unwrap(), None);
    assert!(journal.latest().unwrap().is_none());
}

#[test]
fn a_retracted_entry_is_not_undone() {
    let directory = tempfile::tempdir().unwrap();
    let journal = journal_in(directory.path(), JournalLimits::default());

    let mut writer = journal.begin(description("old")).unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        writer
            .record(
                &Path::new("/project").join(name),
                EntryKind::Disk,
                name.as_bytes(),
                sha256_hex(b"new"),
                1,
            )
            .unwrap();
    }
    writer.retract(1).unwrap();
    writer.finish().unwrap();

    let transaction = journal.latest().unwrap().unwrap();
    let paths: Vec<_> = transaction
        .entries
        .iter()
        .map(|entry| entry.path.file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(paths, ["a.txt", "c.txt"]);
}

#[test]
fn the_latest_transaction_is_the_most_recent_one() {
    let directory = tempfile::tempdir().unwrap();
    let journal = journal_in(directory.path(), JournalLimits::default());

    record_one(&journal, "first", b"1");
    record_one(&journal, "second", b"2");
    let third = record_one(&journal, "third", b"3");

    let latest = journal.latest().unwrap().unwrap();
    assert_eq!(latest.id, third);
    assert_eq!(latest.description.pattern, "third");
}

#[test]
fn the_oldest_transactions_are_dropped_beyond_the_count_limit() {
    let directory = tempfile::tempdir().unwrap();
    let limits = JournalLimits {
        max_transactions: 3,
        ..Default::default()
    };
    let journal = journal_in(directory.path(), limits);

    let ids: Vec<String> = (0..6)
        .map(|index| record_one(&journal, &format!("replace {index}"), b"copy"))
        .collect();

    assert_eq!(journal.transaction_ids(), ids[3..]);
    assert_eq!(journal.latest().unwrap().unwrap().id, ids[5]);
}

#[test]
fn the_oldest_transactions_are_dropped_beyond_the_total_size_limit() {
    let directory = tempfile::tempdir().unwrap();
    let limits = JournalLimits {
        max_transactions: 10,
        max_transaction_bytes: 10_000,
        max_total_bytes: 25_000,
    };
    let journal = journal_in(directory.path(), limits);

    let ids: Vec<String> = (0..5)
        .map(|index| record_one(&journal, &format!("replace {index}"), &[b'x'; 10_000]))
        .collect();

    // Each transaction holds a bit over 10,000 bytes, so only two fit when the next one starts.
    assert_eq!(journal.transaction_ids(), ids[2..]);
}

#[test]
fn a_transaction_refuses_copies_beyond_its_size_limit() {
    let directory = tempfile::tempdir().unwrap();
    let limits = JournalLimits {
        max_transaction_bytes: 10,
        ..Default::default()
    };
    let journal = journal_in(directory.path(), limits);
    let mut writer = journal.begin(description("old")).unwrap();

    let path = Path::new("/project/a.txt");
    let post = sha256_hex(b"new");
    assert!(
        writer
            .record(path, EntryKind::Disk, b"123456", post.clone(), 1)
            .is_ok()
    );
    assert!(matches!(
        writer.record(path, EntryKind::Disk, b"12345", post.clone(), 1),
        Err(RecordError::Full)
    ));
    assert!(matches!(writer.reserve(5), Err(RecordError::Full)));
    assert!(writer.reserve(4).is_ok());
    assert!(
        writer
            .record_reserved(path, EntryKind::Buffer, b"1234", post, 1)
            .is_ok()
    );
    assert!(matches!(writer.reserve(1), Err(RecordError::Full)));

    writer.finish().unwrap();
    assert_eq!(journal.latest().unwrap().unwrap().entries.len(), 2);
}

#[test]
fn forgetting_every_entry_drops_the_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let journal = journal_in(directory.path(), JournalLimits::default());
    let older = record_one(&journal, "older", b"1");

    let mut writer = journal.begin(description("newer")).unwrap();
    for name in ["a.txt", "b.txt"] {
        writer
            .record(
                &Path::new("/project").join(name),
                EntryKind::Disk,
                name.as_bytes(),
                sha256_hex(b"new"),
                1,
            )
            .unwrap();
    }
    writer.finish().unwrap();

    let mut transaction = journal.latest().unwrap().unwrap();
    transaction.forget(&[0]).unwrap();
    let reloaded = journal.latest().unwrap().unwrap();
    assert_eq!(reloaded.id, transaction.id);
    assert_eq!(reloaded.entries.len(), 1);
    assert_eq!(reloaded.entries[0].index, 1);
    assert_eq!(reloaded.pre_image(&reloaded.entries[0]).unwrap(), b"b.txt");

    let mut transaction = reloaded;
    transaction.forget(&[1]).unwrap();
    // With the newer transaction gone, the older one is next to be undone.
    assert_eq!(journal.latest().unwrap().unwrap().id, older);
}

#[test]
fn a_damaged_copy_is_not_handed_out() {
    let directory = tempfile::tempdir().unwrap();
    let journal = journal_in(directory.path(), JournalLimits::default());
    let id = record_one(&journal, "old", b"original");

    let blob = directory.path().join("journal").join(&id).join("0.pre");
    fs::write(&blob, b"tampered").unwrap();

    let transaction = journal.latest().unwrap().unwrap();
    assert!(transaction.pre_image(&transaction.entries[0]).is_err());
}

#[test]
fn a_manifest_cut_short_by_a_crash_keeps_its_complete_entries() {
    let directory = tempfile::tempdir().unwrap();
    let journal = journal_in(directory.path(), JournalLimits::default());
    let id = record_one(&journal, "old", b"original");

    let manifest = directory
        .path()
        .join("journal")
        .join(&id)
        .join(MANIFEST_FILE);
    let mut content = fs::read(&manifest).unwrap();
    content.extend_from_slice(b"{\"record\":\"entry\",\"index\":1,\"pa");
    fs::write(&manifest, content).unwrap();

    let transaction = journal.latest().unwrap().unwrap();
    assert_eq!(transaction.entries.len(), 1);
    assert_eq!(transaction.entries[0].index, 0);
}

#[test]
fn an_unreadable_transaction_is_passed_over() {
    let directory = tempfile::tempdir().unwrap();
    let journal = journal_in(directory.path(), JournalLimits::default());
    let good = record_one(&journal, "good", b"1");
    let bad = record_one(&journal, "bad", b"2");

    let manifest = directory
        .path()
        .join("journal")
        .join(&bad)
        .join(MANIFEST_FILE);
    fs::write(&manifest, b"not a manifest\n").unwrap();

    assert_eq!(journal.latest().unwrap().unwrap().id, good);
}

#[cfg(unix)]
#[test]
fn the_journal_is_closed_to_other_users() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let journal = journal_in(directory.path(), JournalLimits::default());
    let id = record_one(&journal, "old", b"secret");

    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let root = directory.path().join("journal");
    let transaction = root.join(&id);
    assert_eq!(mode(&root), 0o700);
    assert_eq!(mode(&transaction), 0o700);
    for entry in fs::read_dir(&transaction).unwrap() {
        assert_eq!(mode(&entry.unwrap().path()), 0o600);
    }
}

#[test]
fn hashes_are_hexadecimal_sha256() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}
