use super::super::dialog::dialog_on_screen;
use super::super::dialog::tests::{PERMISSION, permission_at};
use super::*;

fn at(selected: usize) -> ScreenDialog {
    dialog_on_screen(&permission_at(selected)).unwrap()
}

#[test]
fn allow_on_the_selected_option_is_one_enter_then_done() {
    let mut answer = Answer::new(at(0), 0);
    assert_eq!(answer.next(Some(&at(0)), true), Step::Press(Key::Enter));
    // The dialog left the screen: answered.
    assert_eq!(answer.next(None, false), Step::Done);
}

#[test]
fn deny_moves_down_to_the_last_option_then_enters() {
    let mut answer = Answer::new(at(0), 2);
    assert_eq!(answer.next(Some(&at(0)), true), Step::Press(Key::Down));
    assert_eq!(answer.next(Some(&at(1)), true), Step::Press(Key::Down));
    assert_eq!(answer.next(Some(&at(2)), true), Step::Press(Key::Enter));
    assert_eq!(answer.next(Some(&at(2)), false), Step::Wait);
    assert_eq!(answer.next(None, false), Step::Done);
}

#[test]
fn a_cursor_the_user_moved_is_walked_back() {
    // The user left the ❯ on the last option; Allow goes up to the first.
    let mut answer = Answer::new(at(0), 0);
    assert_eq!(answer.next(Some(&at(2)), true), Step::Press(Key::Up));
    assert_eq!(answer.next(Some(&at(1)), true), Step::Press(Key::Up));
    assert_eq!(answer.next(Some(&at(0)), true), Step::Press(Key::Enter));
}

#[test]
fn an_answer_presses_nothing_once_the_dialog_changed() {
    let other = dialog_on_screen(&PERMISSION.replace("1 hello", "1 bye")).unwrap();
    let mut answer = Answer::new(at(0), 0);
    assert_eq!(
        answer.next(Some(&other), true),
        Step::Refuse(DIALOG_CHANGED)
    );
    let mut answer = Answer::new(at(0), 0);
    assert_eq!(answer.next(None, true), Step::Refuse(DIALOG_CHANGED));
}

#[test]
fn an_answer_stops_when_the_dialog_changes_mid_selection() {
    let other = dialog_on_screen(&PERMISSION.replace("1 hello", "1 bye")).unwrap();
    let mut answer = Answer::new(at(0), 2);
    assert_eq!(answer.next(Some(&at(0)), true), Step::Press(Key::Down));
    assert_eq!(
        answer.next(Some(&other), true),
        Step::Refuse(DIALOG_CHANGED)
    );
}

#[test]
fn a_claude_that_stopped_waiting_gets_no_key() {
    let mut answer = Answer::new(at(0), 0);
    assert_eq!(
        answer.next(Some(&at(0)), false),
        Step::Refuse(DIALOG_CHANGED)
    );
}

#[test]
fn a_selection_that_does_not_move_is_reported() {
    let mut answer = Answer::new(at(0), 2);
    let steps: Vec<Step> = (0..6).map(|_| answer.next(Some(&at(0)), true)).collect();
    assert_eq!(
        steps
            .iter()
            .filter(|s| **s == Step::Press(Key::Down))
            .count(),
        4
    );
    assert_eq!(steps.last(), Some(&Step::Refuse(NOT_MOVED)));
    assert!(!steps.contains(&Step::Press(Key::Enter)));
}

#[test]
fn enter_that_leaves_the_dialog_open_is_reported() {
    let mut answer = Answer::new(at(0), 0);
    assert_eq!(answer.next(Some(&at(0)), true), Step::Press(Key::Enter));
    let last = (0..CHECKS_AFTER_ENTER)
        .map(|_| answer.next(Some(&at(0)), true))
        .last();
    assert_eq!(last, Some(Step::Refuse(STILL_OPEN)));
}
