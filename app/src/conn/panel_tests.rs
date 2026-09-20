//! Tests for the panel's row addressing, which is the part that decides what
//! a returning reader actually sees. Rendering needs a window; deciding which
//! row is at which index does not.

use ::conn::story::{ConnStep, ConnStory, ConnTurn};
use ::conn::{ConnEntry, ConnEntryKind, ConnSession};
use chrono::{DateTime, TimeZone, Utc};

use super::{ConnRow, PREVIEW_CHARS, row_at, row_count, truncate};

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).single().expect("valid time")
}

fn step(description: &str, runs: usize) -> ConnStep {
    ConnStep {
        description: description.to_owned(),
        runs,
    }
}

fn turn(started: i64, prompt: &str, steps: Vec<ConnStep>, outcome: Option<&str>) -> ConnTurn {
    ConnTurn {
        started_at: at(started),
        ended_at: Some(at(started + 60)),
        prompt: prompt.to_owned(),
        steps,
        outcome: outcome.map(str::to_owned),
    }
}

/// `describe` collapses a row to something comparable, so a test can assert on
/// the shape of a whole chapter rather than one row at a time.
fn describe(session: &ConnSession) -> Vec<String> {
    (0..row_count(session))
        .map(|index| match row_at(session, index) {
            Some(ConnRow::Turn { prompt, .. }) => format!("you: {prompt}"),
            Some(ConnRow::Step { description, runs }) => format!("did: {description} ×{runs}"),
            Some(ConnRow::Outcome(text)) => format!("said: {text}"),
            Some(ConnRow::Live(entry)) => format!("live: {:?}", entry.kind),
            None => "missing".to_owned(),
        })
        .collect()
}

#[test]
fn a_chapter_is_a_prompt_then_its_steps_then_its_outcome() {
    let mut session = ConnSession::default();
    session.adopt_story(
        ConnStory {
            title: None,
            turns: vec![turn(
                0,
                "wire the ADRs in",
                vec![
                    step("Explore the docs layout", 1),
                    step("Add ADR settings", 3),
                ],
                Some("ADRs were assumed but never wired in."),
            )],
        },
        at(100),
    );

    assert_eq!(
        describe(&session),
        vec![
            "you: wire the ADRs in",
            "did: Explore the docs layout ×1",
            "did: Add ADR settings ×3",
            "said: ADRs were assumed but never wired in.",
        ]
    );
}

#[test]
fn chapters_read_in_the_order_they_happened() {
    let mut session = ConnSession::default();
    session.adopt_story(
        ConnStory {
            title: None,
            turns: vec![
                turn(
                    0,
                    "first ask",
                    vec![step("first work", 1)],
                    Some("first done"),
                ),
                turn(200, "second ask", vec![], Some("second done")),
            ],
        },
        at(300),
    );

    assert_eq!(
        describe(&session),
        vec![
            "you: first ask",
            "did: first work ×1",
            "said: first done",
            "you: second ask",
            "said: second done",
        ]
    );
}

/// The turn in flight has no transcript account yet, so it shows as live hook
/// events after the chapters that do.
#[test]
fn the_turn_in_flight_follows_the_chapters_already_told() {
    let mut session = ConnSession::default();
    session.push(ConnEntry::new(
        ConnEntryKind::Prompt {
            text: "the told turn".to_owned(),
        },
        at(10),
    ));
    session.adopt_story(
        ConnStory {
            title: None,
            turns: vec![turn(10, "the told turn", vec![], Some("told"))],
        },
        at(100),
    );
    session.push(ConnEntry::new(
        ConnEntryKind::Prompt {
            text: "the new ask".to_owned(),
        },
        at(200),
    ));
    session.push(ConnEntry::new(
        ConnEntryKind::ToolCompleted {
            tool_name: Some("Bash".to_owned()),
            target: None,
            runs: 4,
        },
        at(210),
    ));

    let rows = describe(&session);
    assert_eq!(rows.len(), 4);
    assert_eq!(&rows[..2], ["you: the told turn", "said: told"]);
    assert!(
        rows[2].starts_with("live: Prompt"),
        "the new instruction is live, not told: {}",
        rows[2]
    );
    assert!(rows[3].starts_with("live: ToolCompleted"), "{}", rows[3]);
}

/// A turn that has not finished contributes no outcome row, so the panel does
/// not show an empty "said".
#[test]
fn an_unfinished_chapter_has_no_outcome_row() {
    let mut session = ConnSession::default();
    session.adopt_story(
        ConnStory {
            title: None,
            turns: vec![turn(
                0,
                "start something long",
                vec![step("Kick it off", 1)],
                None,
            )],
        },
        at(100),
    );

    assert_eq!(
        describe(&session),
        vec!["you: start something long", "did: Kick it off ×1"]
    );
}

#[test]
fn a_session_with_nothing_in_it_has_no_rows() {
    let session = ConnSession::default();
    assert_eq!(row_count(&session), 0);
    assert!(row_at(&session, 0).is_none());
}

/// The list asks for indices it has already allocated, and the story can
/// shrink between the two, so an index past the end has to be absent rather
/// than a panic.
#[test]
fn an_index_past_the_end_is_no_row() {
    let mut session = ConnSession::default();
    session.adopt_story(
        ConnStory {
            title: None,
            turns: vec![turn(0, "go", vec![], Some("done"))],
        },
        at(100),
    );

    assert_eq!(row_count(&session), 2);
    assert!(row_at(&session, 2).is_none());
    assert!(row_at(&session, 99).is_none());
}

#[test]
fn a_long_prompt_is_cut_with_a_mark() {
    let long = "a".repeat(PREVIEW_CHARS + 50);
    let cut = truncate(&long);
    assert_eq!(cut.chars().count(), PREVIEW_CHARS + 1);
    assert!(cut.ends_with('…'));
}

/// Truncating by bytes would split a multi-byte character and panic.
#[test]
fn truncation_counts_characters_not_bytes() {
    let long = "é".repeat(PREVIEW_CHARS + 10);
    let cut = truncate(&long);
    assert_eq!(cut.chars().count(), PREVIEW_CHARS + 1);
}

/// For a tool with no `command` or `file_path`, the plugin serialises the
/// whole tool input into the summary. Eighty characters of JSON in the panel
/// says less than the tool's name on its own.
#[test]
fn a_permission_summary_drops_a_raw_json_preview() {
    assert_eq!(
        super::drop_json_tail(
            r#"Wants to run AskUserQuestion: {"questions":[{"question":"FORMAT.md:12 already"#
        ),
        "Wants to run AskUserQuestion"
    );
}

/// A real command is the most useful thing the permission row can carry, so
/// only JSON is cut.
#[test]
fn a_permission_summary_keeps_a_real_command() {
    assert_eq!(
        super::drop_json_tail("Wants to run Bash: rm -rf node_modules"),
        "Wants to run Bash: rm -rf node_modules"
    );
}
