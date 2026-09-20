use chrono::{TimeZone, Utc};

use super::story::{ConnStory, ConnTurn};
use super::{ConnEntry, ConnEntryKind, ConnSession, MAX_ENTRIES};

fn at(seconds: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).single().expect("valid time")
}

fn prompt(text: &str, seconds: i64) -> ConnEntry {
    ConnEntry::new(
        ConnEntryKind::Prompt {
            text: text.to_owned(),
        },
        at(seconds),
    )
}

fn tool(seconds: i64) -> ConnEntry {
    ConnEntry::new(
        ConnEntryKind::ToolCompleted {
            tool_name: Some("Bash".to_owned()),
            target: Some("cargo test".to_owned()),
            runs: 1,
        },
        at(seconds),
    )
}

/// A tool entry that won't coalesce with [`tool`].
fn other_tool(seconds: i64) -> ConnEntry {
    ConnEntry::new(
        ConnEntryKind::ToolCompleted {
            tool_name: Some("Read".to_owned()),
            target: None,
            runs: 1,
        },
        at(seconds),
    )
}

#[test]
fn history_preserves_order() {
    let mut session = ConnSession::default();
    session.push(prompt("first", 1));
    session.push(tool(2));
    session.push(prompt("second", 3));

    assert_eq!(
        session.prompts().collect::<Vec<_>>(),
        vec!["first", "second"],
        "the spine must read in the order the instructions were sent"
    );
    assert_eq!(session.entries().len(), 3);
    assert_eq!(session.dropped(), 0);
}

#[test]
fn eviction_drops_volume_entries_and_keeps_every_prompt() {
    let mut session = ConnSession::default();
    session.push(prompt("the original ask", 0));
    // Alternate so the entries don't coalesce, otherwise this never reaches
    // the cap at all.
    for i in 0..MAX_ENTRIES as i64 + 50 {
        if i % 2 == 0 {
            session.push(tool(i + 1));
        } else {
            session.push(other_tool(i + 1));
        }
    }

    assert_eq!(
        session.entries().len(),
        MAX_ENTRIES,
        "history must stay bounded"
    );
    assert_eq!(session.dropped(), 51);
    assert_eq!(
        session.prompts().collect::<Vec<_>>(),
        vec!["the original ask"],
        "the earliest prompt is the session's intent and must survive any \
         amount of tool traffic"
    );
}

#[test]
fn a_session_of_only_prompts_is_never_evicted() {
    let mut session = ConnSession::default();
    for i in 0..MAX_ENTRIES as i64 + 10 {
        session.push(prompt(&format!("ask {i}"), i));
    }

    assert_eq!(session.entries().len(), MAX_ENTRIES + 10);
    assert_eq!(session.dropped(), 0);
}

#[test]
fn last_response_reads_the_most_recent_completed_turn() {
    let mut session = ConnSession::default();
    session.push(ConnEntry::new(
        ConnEntryKind::Responded {
            text: Some("older".to_owned()),
        },
        at(1),
    ));
    session.push(tool(2));
    session.push(ConnEntry::new(
        ConnEntryKind::Responded {
            text: Some("newer".to_owned()),
        },
        at(3),
    ));

    assert_eq!(session.last_response(), Some("newer"));
}

#[test]
fn last_response_is_none_before_the_first_turn_completes() {
    let mut session = ConnSession::default();
    session.push(prompt("do the thing", 1));
    session.push(tool(2));

    assert_eq!(session.last_response(), None);
}

/// The plugin reports only a tool name for a completed call, so a burst of
/// twenty Bash calls is twenty identical rows that bury the prompts around
/// them. They fold into one entry carrying the count.
#[test]
fn repeated_tool_calls_coalesce_into_one_entry() {
    let mut session = ConnSession::default();
    session.push(prompt("run the tests", 0));
    for i in 0..11 {
        session.push(tool(i + 1));
    }

    assert_eq!(session.entries().len(), 2, "one prompt plus one folded run");
    match &session.entries()[1].kind {
        ConnEntryKind::ToolCompleted { runs, .. } => assert_eq!(*runs, 11),
        other => panic!("expected a tool run, got {other:?}"),
    }
    assert_eq!(
        session.entries()[1].at,
        at(11),
        "the folded entry carries the most recent call's time, not the first"
    );
}

/// Coalescing must not reach across an intervening entry, or a prompt sent
/// mid-run would vanish from the spine.
#[test]
fn tool_runs_do_not_coalesce_across_other_entries() {
    let mut session = ConnSession::default();
    session.push(tool(1));
    session.push(prompt("actually, stop", 2));
    session.push(tool(3));

    assert_eq!(session.entries().len(), 3);
    assert_eq!(
        session.prompts().collect::<Vec<_>>(),
        vec!["actually, stop"]
    );
}

/// Different tools are different information and stay separate.
#[test]
fn different_tools_do_not_coalesce() {
    let mut session = ConnSession::default();
    session.push(tool(1));
    session.push(other_tool(2));
    session.push(tool(3));

    assert_eq!(session.entries().len(), 3);
}

fn responded(text: &str, seconds: i64) -> ConnEntry {
    ConnEntry::new(
        ConnEntryKind::Responded {
            text: Some(text.to_owned()),
        },
        at(seconds),
    )
}

/// `ended_at` is `None` on a turn nothing has happened on yet, so
/// `last_activity` falls back to when it started.
fn turn(started: i64, ended: Option<i64>, outcome: Option<&str>) -> ConnTurn {
    ConnTurn {
        started_at: at(started),
        ended_at: ended.map(at),
        prompt: "do the thing".to_owned(),
        steps: vec![],
        outcome: outcome.map(str::to_owned),
    }
}

fn story(turns: Vec<ConnTurn>) -> ConnStory {
    ConnStory { title: None, turns }
}

/// Until a story has been read, the hook entries are all there is.
#[test]
fn everything_is_in_flight_before_a_story_arrives() {
    let mut session = ConnSession::default();
    session.push(prompt("do the thing", 1));
    session.push(tool(2));

    assert_eq!(session.story(), None);
    assert_eq!(session.in_flight().len(), 2);
}

/// The story tells the completed turns better than the hook entries do, so
/// showing both would repeat the turn the reader has just read.
#[test]
fn a_story_supersedes_the_live_entries_it_covers() {
    let mut session = ConnSession::default();
    session.push(prompt("do the thing", 1));
    session.push(tool(2));
    session.push(responded("done", 3));
    session.push(prompt("now the next thing", 5));
    session.push(tool(6));

    assert!(session.adopt_story(story(vec![turn(1, Some(3), Some("done"))]), at(3)));

    let in_flight: Vec<_> = session
        .in_flight()
        .iter()
        .map(|entry| entry.kind.clone())
        .collect();
    assert_eq!(
        in_flight,
        vec![
            ConnEntryKind::Prompt {
                text: "now the next thing".to_owned()
            },
            ConnEntryKind::ToolCompleted {
                tool_name: Some("Bash".to_owned()),
                target: Some("cargo test".to_owned()),
                runs: 1,
            },
        ],
        "only the turn the story does not cover is still in flight"
    );
}

/// Reads are dispatched in order but resolve off the main thread, so a slow
/// read can land after a later one. Taking it would rewind the panel.
#[test]
fn a_stale_read_does_not_replace_a_newer_story() {
    let mut session = ConnSession::default();
    let newer = story(vec![
        turn(1, Some(3), Some("first")),
        turn(4, Some(6), Some("second")),
    ]);
    let older = story(vec![turn(1, Some(3), Some("first"))]);

    assert!(session.adopt_story(newer, at(6)));
    assert!(!session.adopt_story(older, at(3)));
    assert_eq!(
        session.story().expect("story kept").turns.len(),
        2,
        "the later read wins regardless of which resolves first"
    );
}

/// The transcript is written asynchronously, so a read triggered by the stop
/// event can land before the closing message reaches the file. The response is
/// the single most valuable thing on the panel, so it must not be hidden
/// behind a story that does not have it yet.
#[test]
fn a_story_without_the_final_response_leaves_the_live_entry_visible() {
    let mut session = ConnSession::default();
    session.push(prompt("do the thing", 1));
    session.push(responded("done", 9));

    assert!(session.adopt_story(story(vec![turn(1, Some(3), None)]), at(9)));

    let in_flight: Vec<_> = session
        .in_flight()
        .iter()
        .map(|entry| entry.kind.clone())
        .collect();
    assert_eq!(
        in_flight,
        vec![ConnEntryKind::Responded {
            text: Some("done".to_owned())
        }]
    );
}

/// Pasted text arrives wrapped in a tag addressed to the agent. Left in, it
/// sits exactly where the instruction should be.
#[test]
fn a_pasted_prompt_loses_its_wrapper() {
    assert_eq!(
        super::tidy_prompt(
            "<pasted_content id=\"0e44\">\ni really love the ideology\n</pasted_content>"
        ),
        "i really love the ideology"
    );
}

/// The hook truncates a prompt at 200 characters, which can land inside the
/// opening tag.
#[test]
fn a_prompt_truncated_inside_the_wrapper_loses_the_fragment() {
    assert_eq!(super::tidy_prompt("<pasted_content id=\"0e4"), "");
}

#[test]
fn a_typed_prompt_is_left_alone() {
    assert_eq!(super::tidy_prompt("  commit changes  "), "commit changes");
}

/// A background task notification arrives through the same hook as a typed
/// instruction. Left in, it opens a chapter nobody asked for, in the one
/// column the panel exists for.
#[test]
fn a_harness_notification_is_not_a_prompt() {
    assert!(super::is_harness_prompt(
        "<task-notification>\n<task-id>a179a5</task-id>\n</task-notification>"
    ));
    assert!(super::is_harness_prompt("  <system-reminder>watch out"));
    assert!(!super::is_harness_prompt(
        "combine what we have in this repo with <task-notification> semantics"
    ));
}
