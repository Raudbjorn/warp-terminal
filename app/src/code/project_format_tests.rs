use super::*;

#[test]
fn discovery_timeout_releases_the_save_and_drops_pending_work() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct PendingDiscovery(Arc<AtomicBool>);
    impl Future for PendingDiscovery {
        type Output = FormatOutcome;
        fn poll(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            std::task::Poll::Pending
        }
    }
    impl Drop for PendingDiscovery {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let outcome = futures_lite::future::block_on(await_format_outcome(
        PendingDiscovery(dropped.clone()),
        Duration::from_millis(5),
    ));
    assert!(matches!(outcome, FormatOutcome::Failed(_)));
    assert!(dropped.load(Ordering::SeqCst));
}
