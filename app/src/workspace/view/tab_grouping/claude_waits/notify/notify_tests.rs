use super::super::dialog::dialog_on_screen;
use super::super::dialog::tests::{permission_at, OTHER, PERMISSION};
use super::*;

fn permission(screen: &str) -> Card {
    Card::Approval(dialog_on_screen(screen).unwrap())
}

fn ids() -> impl FnMut() -> String {
    let mut n = 0;
    move || {
        n += 1;
        format!("n{n}")
    }
}

fn read(card: Card) -> Read<()> {
    Read { card, target: () }
}

fn none() -> HashSet<u32> {
    HashSet::new()
}

#[test]
fn a_notification_says_what_the_dialog_asks() {
    let waiting = Card::Waiting("Claude needs your input".to_owned());
    assert_eq!(content(&waiting), "Claude needs your input");
    assert_eq!(
        content(&permission(OTHER)),
        "Needs you: Allow rm -rf build?"
    );
}

#[test]
fn each_terminal_gets_its_own_notification() {
    let mut entries = HashMap::new();
    let changes = update(
        &mut entries,
        vec![
            (1, read(permission(PERMISSION))),
            (2, read(permission(OTHER))),
        ],
        &none(),
        ids(),
    );
    assert_eq!(changes, [1, 2]);
    assert_eq!(entries[&1].id, "n1");
    assert_eq!(entries[&2].id, "n2");
}

#[test]
fn the_terminal_in_view_gets_no_notification() {
    let mut entries = HashMap::new();
    let seen = HashSet::from([1]);
    let first = update(
        &mut entries,
        vec![(1, read(permission(PERMISSION)))],
        &seen,
        ids(),
    );
    assert!(first.is_empty());
    // Looked away: the dialog was seen, so it still does not notify.
    let away = update(
        &mut entries,
        vec![(1, read(permission(PERMISSION)))],
        &none(),
        ids(),
    );
    assert!(away.is_empty());
    // Notified, then its terminal comes into view: nothing more to send.
    let mut entries = HashMap::new();
    update(
        &mut entries,
        vec![(2, read(permission(OTHER)))],
        &none(),
        ids(),
    );
    let seen = HashSet::from([2]);
    let back = update(
        &mut entries,
        vec![(2, read(permission(OTHER)))],
        &seen,
        ids(),
    );
    assert!(back.is_empty());
}

#[test]
fn only_the_front_window_s_active_tab_is_in_view() {
    assert_eq!(in_view(true, true, Some(7)), Some(7));
    assert_eq!(in_view(false, true, Some(7)), None);
    assert_eq!(in_view(true, false, Some(7)), None);
    assert_eq!(in_view::<u32>(true, true, None), None);
}

#[test]
fn an_answered_dialog_drops_its_entry() {
    let mut entries = HashMap::new();
    update(
        &mut entries,
        vec![(1, read(permission(PERMISSION)))],
        &none(),
        ids(),
    );
    // Answered in the terminal: Claude no longer waits.
    let changes = update(&mut entries, vec![], &none(), ids());
    assert!(changes.is_empty());
    assert!(entries.is_empty());
}

#[test]
fn one_dialog_notifies_once() {
    let mut entries = HashMap::new();
    let mut new_id = ids();
    let first = update(
        &mut entries,
        vec![(1, read(permission(PERMISSION)))],
        &none(),
        &mut new_id,
    );
    assert_eq!(first, [1]);
    // The user moved the ❯: the same dialog, no second notification.
    let moved = read(permission(&permission_at(1)));
    assert_eq!(
        update(&mut entries, vec![(1, moved)], &none(), &mut new_id),
        Vec::<u32>::new()
    );
    // Another dialog in its place: the old entry goes, the new one posts.
    let next = update(
        &mut entries,
        vec![(1, read(permission(OTHER)))],
        &none(),
        &mut new_id,
    );
    assert_eq!(next, [1]);
    assert_eq!(entries[&1].id, "n2");
}

#[test]
fn a_wait_without_a_dialog_notifies_one_read_later() {
    let waiting = || read(Card::Waiting("Claude needs your input".to_owned()));
    let mut entries = HashMap::new();
    let mut new_id = ids();
    assert_eq!(
        update(&mut entries, vec![(1, waiting())], &none(), &mut new_id),
        Vec::<u32>::new()
    );
    // Its dialog appears on the next read: only the dialog posts.
    let dialog = read(permission(PERMISSION));
    let changes = update(&mut entries, vec![(1, dialog)], &none(), &mut new_id);
    assert_eq!(changes, [1]);
    // Still no dialog on the second read: the wait itself posts, once.
    let mut entries = HashMap::new();
    update(&mut entries, vec![(1, waiting())], &none(), &mut new_id);
    let second = update(&mut entries, vec![(1, waiting())], &none(), &mut new_id);
    assert_eq!(second, [1]);
    assert_eq!(
        update(&mut entries, vec![(1, waiting())], &none(), &mut new_id),
        Vec::<u32>::new()
    );
}
