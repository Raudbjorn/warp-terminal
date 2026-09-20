use chrono::{TimeZone, Utc};

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
