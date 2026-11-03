use super::*;

fn tab(group: Option<&str>, pos: Option<u32>) -> LayoutTab {
    LayoutTab {
        pinned: false,
        group: group.map(str::to_owned),
        pos,
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn a_stale_or_known_rev_is_not_adopted() {
    // The phone's change (rev 6) arrives with a fresh read: adopt.
    assert!(should_adopt(Some(6), Some(5), true));
    // The same rev from a read that may predate our own write: never.
    assert!(!should_adopt(Some(6), Some(5), false));
    // Our own rev, or an older one, changes nothing.
    assert!(!should_adopt(Some(5), Some(5), true));
    assert!(!should_adopt(Some(4), Some(5), true));
    // Nothing stored yet: nothing to adopt; after a restart any stored rev is news.
    assert!(!should_adopt(None, None, true));
    assert!(should_adopt(Some(1), None, true));
}

#[test]
fn nothing_is_written_without_an_attached_tab() {
    // A test app polling the default server, where it has no tabs: even with the rev
    // unset (the "first start" case) it writes nothing there.
    assert!(!may_write(0));
    assert!(may_write(1));
}

#[test]
fn a_write_only_lands_on_the_rev_it_read() {
    assert_eq!(next_rev(None, None), Some(1));
    assert_eq!(next_rev(Some(5), Some(5)), Some(6));
    // The phone wrote rev 6 after our read of 5: our write is dropped.
    assert_eq!(next_rev(Some(6), Some(5)), None);
    assert_eq!(next_rev(Some(3), None), None);
}

#[test]
fn unknown_group_names_keep_their_place() {
    // "laptop" and "empty" are unknown here; the app swapped "a" and "b".
    let tmux = names(&["a", "laptop", "b", "empty"]);
    assert_eq!(
        merge_group_order(&tmux, &names(&["b", "a"])),
        names(&["b", "laptop", "a", "empty"])
    );
    // Unchanged app order: the list comes back as it was.
    assert_eq!(merge_group_order(&tmux, &names(&["a", "b"])), tmux);
    // The app has no groups at all: every name stays.
    assert_eq!(merge_group_order(&tmux, &[]), tmux);
}

#[test]
fn a_new_app_group_goes_after_its_predecessor() {
    let tmux = names(&["a", "laptop", "b"]);
    assert_eq!(
        merge_group_order(&tmux, &names(&["a", "new", "b"])),
        names(&["a", "new", "laptop", "b"])
    );
    assert_eq!(
        merge_group_order(&tmux, &names(&["first", "a", "b"])),
        names(&["first", "a", "laptop", "b"])
    );
    assert_eq!(
        merge_group_order(&[], &names(&["x", "y"])),
        names(&["x", "y"])
    );
}

#[test]
fn sections_follow_the_list_and_other_stays_last() {
    let tabs = [
        tab(None, Some(0)),
        tab(Some("a"), Some(0)),
        tab(Some("b"), Some(0)),
        tab(Some("c"), Some(0)),
    ];
    // "b" first, "zzz" unknown, "a" listed, "c" not listed: after the listed ones.
    assert_eq!(
        adopted_order(&tabs, &names(&["b", "zzz", "a"])),
        [2, 1, 3, 0]
    );
}

#[test]
fn tabs_without_a_pos_keep_their_order_after_the_positioned_ones() {
    let tabs = [
        tab(Some("a"), None), // no tmux session
        tab(Some("a"), Some(1)),
        tab(Some("a"), None), // no tmux session
        tab(Some("a"), Some(0)),
    ];
    assert_eq!(adopted_order(&tabs, &names(&["a"])), [3, 1, 0, 2]);
    // Nothing positioned: nothing moves.
    let plain = [tab(None, None), tab(None, None), tab(None, None)];
    assert_eq!(adopted_order(&plain, &[]), [0, 1, 2]);
}

#[test]
fn pinned_tabs_never_move() {
    let mut pinned = tab(Some("a"), Some(9));
    pinned.pinned = true;
    let tabs = [
        pinned.clone(),
        tab(None, Some(1)),
        tab(None, Some(0)),
        pinned,
    ];
    assert_eq!(adopted_order(&tabs, &[]), [0, 2, 1, 3]);
    assert_eq!(section_positions(&tabs), [None, Some(0), Some(1), None]);
}

#[test]
fn positions_count_within_each_section() {
    let tabs = [
        tab(Some("a"), None),
        tab(Some("a"), None),
        tab(None, None),
        tab(Some("b"), None),
        tab(None, None),
    ];
    assert_eq!(
        section_positions(&tabs),
        [Some(0), Some(1), Some(0), Some(0), Some(1)]
    );
    assert_eq!(app_group_order(&tabs), names(&["a", "b"]));
}

#[test]
fn group_list_and_rev_parse_loosely() {
    assert_eq!(parse_group_list("a\n\n b \nc\n"), names(&["a", "b", "c"]));
    assert_eq!(parse_rev(" 7\n"), Some(7));
    assert_eq!(parse_rev(""), None);
    assert_eq!(parse_rev("x"), None);
}
