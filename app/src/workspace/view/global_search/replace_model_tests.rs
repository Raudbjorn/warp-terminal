use super::*;

#[test]
fn closing_the_model_keeps_the_run_exclusive_until_the_worker_finishes() {
    static LOCK: AtomicBool = AtomicBool::new(false);
    let model_permit = RunPermit::acquire(&LOCK).unwrap();
    let worker_permit = model_permit.clone();
    drop(model_permit);
    assert!(RunPermit::acquire(&LOCK).is_none());
    drop(worker_permit);
    assert!(RunPermit::acquire(&LOCK).is_some());
}
#[test]
fn timeout_and_cancel_release_the_run_but_not_a_late_writes_quarantine() {
    for cancelled in [false, true] {
        static LOCK: AtomicBool = AtomicBool::new(false);
        let path = PathBuf::from(format!("pending-save-{cancelled}"));
        let transaction = PathBuf::from(format!("journal-save-{cancelled}"));
        let guard = WriteGuard::acquire(path.clone(), transaction.clone()).unwrap();
        let (tx, rx) = futures::channel::oneshot::channel();
        let completion: SaveFuture = Box::pin(async move {
            rx.await
                .unwrap_or_else(|_| Err(Arc::new(FileSaveError::CompletionLost)))
        });
        let observed = completion.shared();
        let monitor = monitor_save(observed.clone(), guard.clone());
        let permit = RunPermit::acquire(&LOCK).unwrap();
        let cancel = AtomicBool::new(cancelled);
        let deadline = if cancelled {
            Instant::now() + SAVE_WAIT_TIMEOUT
        } else {
            Instant::now() + Duration::from_millis(5)
        };
        assert!(
            futures::executor::block_on(await_save(observed.clone(), deadline, &cancel)).is_err()
        );
        assert!(
            await_save(observed, deadline, &cancel)
                .now_or_never()
                .is_some(),
            "later files reuse the expired/cancelled batch deadline"
        );
        // The phase retains the failed/pending journal entries before releasing its run permit.
        guard.journal_finished();
        drop(permit);
        assert!(RunPermit::acquire(&LOCK).is_some());
        assert!(file_write_guard::is_quarantined(&path));
        assert!(file_write_guard::transaction_is_pinned(&transaction));
        let unrelated = WriteGuard::acquire(
            PathBuf::from(format!("unrelated-{cancelled}")),
            PathBuf::from("unrelated-journal"),
        )
        .unwrap();
        unrelated.cancel_before_dispatch();
        tx.send(Ok(())).unwrap();
        futures::executor::block_on(monitor);
        assert!(!file_write_guard::is_quarantined(&path));
        assert!(!file_write_guard::transaction_is_pinned(&transaction));
    }
}

#[test]
fn a_lost_completion_keeps_the_path_and_journal_quarantined() {
    let path = PathBuf::from("lost-save");
    let transaction = PathBuf::from("lost-save-journal");
    let guard = WriteGuard::acquire(path.clone(), transaction.clone()).unwrap();
    let completion: SaveFuture = Box::pin(async { Err(Arc::new(FileSaveError::CompletionLost)) });
    guard.journal_finished();
    futures::executor::block_on(monitor_save(completion.shared(), guard.clone()));
    assert!(file_write_guard::is_quarantined(&path));
    assert!(file_write_guard::transaction_is_pinned(&transaction));
    // Test cleanup only: production never marks unknown I/O finished.
    guard.write_finished();
}
#[test]
fn cancellation_wakes_a_wait_that_is_already_pending() {
    let completion: SaveFuture = Box::pin(futures::future::pending());
    let cancel = AtomicBool::new(false);
    let (result, ()) = futures::executor::block_on(async {
        futures::join!(
            await_save(
                completion.shared(),
                Instant::now() + SAVE_WAIT_TIMEOUT,
                &cancel
            ),
            async {
                async_io::Timer::after(Duration::from_millis(5)).await;
                cancel.store(true, Ordering::Relaxed);
            }
        )
    });
    assert!(result.unwrap_err().contains("stopped waiting"));
}
