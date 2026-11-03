use super::*;
#[test]
fn lost_observers_keep_bounded_path_and_journal_quarantine_until_both_completions() {
    let transaction = PathBuf::from("journal/one");
    let first = WriteGuard::acquire(PathBuf::from("pending"), transaction.clone()).unwrap();
    assert!(first.authorizes(Path::new("pending")));
    assert!(!first.authorizes(Path::new("unrelated")));
    assert!(WriteGuard::acquire(PathBuf::from("pending"), transaction.clone()).is_err());
    let late_completion = first.clone();
    first.journal_finished();
    drop(first); // Cancellation/lost waiter must not release a still-running write.
    assert!(is_quarantined(Path::new("pending")));
    assert!(transaction_is_pinned(&transaction));
    let unrelated =
        WriteGuard::acquire(PathBuf::from("unrelated"), PathBuf::from("journal/two")).unwrap();
    unrelated.write_finished();
    assert!(is_quarantined(Path::new("unrelated"))); // Journal processing still owns the pin.
    unrelated.journal_finished();
    assert!(!is_quarantined(Path::new("unrelated")));
    let rest: Vec<_> = (1..MAX_PENDING_WRITES)
        .map(|n| {
            WriteGuard::acquire(PathBuf::from(format!("pending-{n}")), transaction.clone()).unwrap()
        })
        .collect();
    assert!(WriteGuard::acquire(PathBuf::from("over-cap"), transaction.clone()).is_err());
    late_completion.write_finished();
    assert!(!is_quarantined(Path::new("pending")));
    for guard in rest {
        guard.cancel_before_dispatch();
    }
    assert!(!transaction_is_pinned(&transaction));
}
