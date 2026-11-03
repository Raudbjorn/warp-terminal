use super::super::dialog::dialog_on_screen;
use super::super::dialog::tests::{OTHER, PERMISSION, permission_at};
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
fn an_action_answers_only_the_dialog_its_notification_showed() {
    let card = permission(PERMISSION);
    assert_eq!(act(Some(ALLOW), Some(&card), true), Act::Pick(0));
    assert_eq!(act(Some(DENY), Some(&card), true), Act::Pick(2));
    // The dialog is gone or was replaced: nothing is pressed.
    assert_eq!(
        act(Some(ALLOW), None, true),
        Act::Refuse(dialog::DIALOG_CHANGED)
    );
    assert_eq!(
        act(Some(DENY), None, true),
        Act::Refuse(dialog::DIALOG_CHANGED)
    );
    assert_eq!(act(Some("open"), None, true), Act::Open);
    assert_eq!(act(None, Some(&card), true), Act::Open);
}

#[test]
fn allow_and_deny_only_on_a_permission() {
    let other = Card::Dialog(dialog_on_screen(OTHER).unwrap());
    assert_eq!(
        act(Some(ALLOW), Some(&other), true),
        Act::Refuse(dialog::DIALOG_CHANGED)
    );
    assert_eq!(content(&other, true).0, OPEN_ONLY);
    let waiting = Card::Waiting("Claude needs your input".to_owned());
    assert_eq!(
        content(&waiting, true),
        (OPEN_ONLY, "Claude needs your input".to_owned())
    );
    let (category, body) = content(&permission(OTHER), true);
    assert_eq!(
        (category, body.as_str()),
        (APPROVAL, "Needs you: Allow rm -rf build?")
    );
}

#[test]
fn a_dialog_behind_a_full_screen_program_only_opens_the_chat() {
    let card = permission(PERMISSION);
    assert_eq!(content(&card, false).0, OPEN_ONLY);
    assert_eq!(
        act(Some(ALLOW), Some(&card), false),
        Act::Refuse(dialog::DIALOG_CHANGED)
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
        &none(),
        ids(),
    );
    assert_eq!(changes, [Change::Post(1), Change::Post(2)]);
    // A click on terminal 1's notification never finds terminal 2's dialog.
    assert!(clicked(&mut entries, &2, "n1").is_none());
    assert!(clicked(&mut entries, &1, "n1").is_some());
}

#[test]
fn the_terminal_in_view_gets_no_notification() {
    let mut entries = HashMap::new();
    let seen = HashSet::from([1]);
    let first = update(
        &mut entries,
        vec![(1, read(permission(PERMISSION)))],
        &seen,
        &none(),
        ids(),
    );
    assert_eq!(first, []);
    // Looked away: the dialog was seen, so it still does not notify.
    let away = update(
        &mut entries,
        vec![(1, read(permission(PERMISSION)))],
        &none(),
        &none(),
        ids(),
    );
    assert_eq!(away, []);
    // Notified, then its terminal comes into view: taken back.
    let mut entries = HashMap::new();
    update(
        &mut entries,
        vec![(2, read(permission(OTHER)))],
        &none(),
        &none(),
        ids(),
    );
    let seen = HashSet::from([2]);
    let back = update(
        &mut entries,
        vec![(2, read(permission(OTHER)))],
        &seen,
        &none(),
        ids(),
    );
    assert_eq!(back, [Change::TakeBack("n1".to_owned())]);
}

#[test]
fn only_the_front_window_s_active_tab_is_in_view() {
    assert_eq!(in_view(true, true, Some(7)), Some(7));
    assert_eq!(in_view(false, true, Some(7)), None);
    assert_eq!(in_view(true, false, Some(7)), None);
    assert_eq!(in_view::<u32>(true, true, None), None);
}

#[test]
fn an_answered_dialog_takes_its_notification_back() {
    let mut entries = HashMap::new();
    update(
        &mut entries,
        vec![(1, read(permission(PERMISSION)))],
        &none(),
        &none(),
        ids(),
    );
    // Answered in the terminal: Claude no longer waits.
    let changes = update(&mut entries, vec![], &none(), &none(), ids());
    assert_eq!(changes, [Change::TakeBack("n1".to_owned())]);
    assert!(entries.is_empty());
    // A late click on it finds nothing, so Allow refuses.
    assert!(clicked(&mut entries, &1, "n1").is_none());
}

#[test]
fn one_dialog_notifies_once() {
    let mut entries = HashMap::new();
    let mut new_id = ids();
    let first = update(
        &mut entries,
        vec![(1, read(permission(PERMISSION)))],
        &none(),
        &none(),
        &mut new_id,
    );
    assert_eq!(first, [Change::Post(1)]);
    // The user moved the ❯: the same dialog, no second notification.
    let moved = read(permission(&permission_at(1)));
    assert_eq!(
        update(
            &mut entries,
            vec![(1, moved)],
            &none(),
            &none(),
            &mut new_id
        ),
        []
    );
    // Another dialog in its place: the old one goes, the new one posts.
    let next = update(
        &mut entries,
        vec![(1, read(permission(OTHER)))],
        &none(),
        &none(),
        &mut new_id,
    );
    assert_eq!(next, [Change::TakeBack("n1".to_owned()), Change::Post(1)]);
    assert_eq!(entries[&1].id, "n2");
}

#[test]
fn a_wait_without_a_dialog_notifies_one_read_later() {
    let waiting = || read(Card::Waiting("Claude needs your input".to_owned()));
    let mut entries = HashMap::new();
    let mut new_id = ids();
    assert_eq!(
        update(
            &mut entries,
            vec![(1, waiting())],
            &none(),
            &none(),
            &mut new_id
        ),
        []
    );
    // Its dialog appears on the next read: only the dialog posts.
    let dialog = read(permission(PERMISSION));
    let changes = update(
        &mut entries,
        vec![(1, dialog)],
        &none(),
        &none(),
        &mut new_id,
    );
    assert_eq!(changes, [Change::Post(1)]);
    // Still no dialog on the second read: the wait itself posts, once.
    let mut entries = HashMap::new();
    update(
        &mut entries,
        vec![(1, waiting())],
        &none(),
        &none(),
        &mut new_id,
    );
    let second = update(
        &mut entries,
        vec![(1, waiting())],
        &none(),
        &none(),
        &mut new_id,
    );
    assert_eq!(second, [Change::Post(1)]);
    assert_eq!(
        update(
            &mut entries,
            vec![(1, waiting())],
            &none(),
            &none(),
            &mut new_id
        ),
        []
    );
}

#[test]
fn a_wait_left_by_an_answer_from_a_notification_never_notifies() {
    let stale = || read(Card::Waiting("Wants to run Bash: mkdir x".to_owned()));
    let answered = HashSet::from([1]);
    let mut entries = HashMap::new();
    let mut new_id = ids();
    assert_eq!(
        update(
            &mut entries,
            vec![(1, stale())],
            &none(),
            &answered,
            &mut new_id
        ),
        []
    );
    // Only the read right after the answer knows it; the wait stays quiet after it.
    assert_eq!(
        update(
            &mut entries,
            vec![(1, stale())],
            &none(),
            &none(),
            &mut new_id
        ),
        []
    );
    assert_eq!(
        update(
            &mut entries,
            vec![(1, stale())],
            &none(),
            &none(),
            &mut new_id
        ),
        []
    );
    assert!(!entries[&1].posted);
}

#[test]
fn a_new_dialog_right_after_an_answer_still_notifies() {
    let answered = HashSet::from([1]);
    let mut entries = HashMap::new();
    let changes = update(
        &mut entries,
        vec![(1, read(permission(OTHER)))],
        &none(),
        &answered,
        ids(),
    );
    assert_eq!(changes, [Change::Post(1)]);
}
