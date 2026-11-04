use super::*;

#[test]
fn any_value_pins_and_only_unset_does_not() {
    assert_eq!(parse(""), None);
    assert_eq!(parse("1761134400"), Some(1761134400));
    assert_eq!(parse(" 1761134400\n"), Some(1761134400));
    assert_eq!(parse("yes"), Some(0));
    assert_eq!(parse("-3"), Some(0));
}

#[test]
fn the_last_pinned_comes_first_and_ties_go_by_name() {
    let pins = [(0, 100, "b"), (1, 300, "z"), (2, 100, "a"), (3, 0, "c")];
    assert_eq!(order(pins), vec![1, 2, 0, 3]);
}

/// A tab that went back to a plain shell loses its session's pin, so the pin menu falls back to
/// Warp's own; tabs that were not checked this time, and attached ones, keep theirs. Fails if a
/// detached tab keeps the pin of the session it left.
#[test]
fn a_detached_tab_forgets_its_session_pin() {
    use std::collections::{HashMap, HashSet};
    let mut pins: HashMap<u32, &str> =
        HashMap::from([(1, "attached"), (2, "detached"), (3, "unchecked")]);
    let polled = HashSet::from([1, 2]);
    let attached = HashSet::from([1]);
    assert!(forget_detached(&mut pins, &polled, &attached));
    let mut left: Vec<_> = pins.into_iter().collect();
    left.sort();
    assert_eq!(left, [(1, "attached"), (3, "unchecked")]);
    // Nothing more to drop on the next poll.
    let mut pins = HashMap::from([(1, "attached")]);
    assert!(!forget_detached(&mut pins, &polled, &attached));
}
