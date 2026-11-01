use chrono::{DateTime, TimeZone, Utc};

use super::{advance_watermark, unseen_turns};
use crate::story::{ConnStory, ConnTurn};

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).single().expect("valid time")
}

#[test]
fn two_seconds_of_focus_sees_the_latest_activity() {
    assert_eq!(
        advance_watermark(Some(at(10)), Some(at(50)), at(100), at(102)),
        Some(at(50))
    );
}

#[test]
fn a_glance_shorter_than_two_seconds_sees_nothing() {
    assert_eq!(
        advance_watermark(Some(at(10)), Some(at(50)), at(100), at(101)),
        Some(at(10))
    );
}

/// A new session in the same pane starts its activity over, which must not unsee the old one.
#[test]
fn the_watermark_never_moves_backwards() {
    assert_eq!(
        advance_watermark(Some(at(80)), Some(at(50)), at(100), at(105)),
        Some(at(80))
    );
}

fn turn(started: i64, answered: Option<i64>) -> ConnTurn {
    ConnTurn {
        started_at: at(started),
        ended_at: Some(at(answered.unwrap_or(started + 5))),
        prompt: format!("turn at {started}"),
        steps: Vec::new(),
        outcome: answered.map(|_| "done".to_owned()),
    }
}

fn story(turns: Vec<ConnTurn>) -> ConnStory {
    ConnStory {
        title: None,
        plan_file: None,
        turns,
    }
}

#[test]
fn answers_after_the_watermark_are_unseen() {
    let story = story(vec![
        turn(0, Some(10)),
        turn(20, Some(30)),
        turn(40, Some(50)),
    ]);
    assert_eq!(unseen_turns(&story, Some(at(30)), true), 1);
}

/// A turn still running has no answer to read yet, even when the agent has narrated its work.
#[test]
fn a_turn_in_flight_is_not_unseen() {
    let story = story(vec![turn(0, Some(10)), turn(20, Some(25))]);
    assert_eq!(unseen_turns(&story, Some(at(10)), false), 0);
}

/// A turn that was running when the reader looked away is unseen once it answers.
#[test]
fn a_turn_answered_after_the_reader_left_is_unseen() {
    let story = story(vec![turn(0, Some(60))]);
    assert_eq!(unseen_turns(&story, Some(at(30)), true), 1);
}

#[test]
fn a_pane_never_seen_has_every_answer_unseen() {
    let story = story(vec![turn(0, Some(10)), turn(20, Some(30))]);
    assert_eq!(unseen_turns(&story, None, true), 2);
}
