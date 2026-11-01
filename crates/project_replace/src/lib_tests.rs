use crate::apply::{ReplaceOutcome, ReplaceRequest};
use crate::engine::MatchFinder;
use crate::preview::Previewer;
use crate::scope::Scope;
use crate::selection::Dismissals;
use crate::undo::UndoOutcome;

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}

/// The app plans on the main thread and replaces on a background one, and the search worker
/// shares its scope between the threads that walk the folders.
#[test]
fn what_crosses_threads_can_cross_threads() {
    assert_send::<ReplaceRequest>();
    assert_send::<ReplaceOutcome>();
    assert_send::<UndoOutcome>();
    assert_send::<MatchFinder>();
    assert_send::<Previewer>();
    assert_send::<Dismissals>();
    assert_sync::<Scope>();
}
