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
fn eviction_drops_oldest_entries_and_keeps_history_bounded() {
    let mut session = ConnSession::default();
    for i in 0..MAX_ENTRIES as i64 + 50 {
        if i % 2 == 0 {
            session.push(tool(i + 1));
        } else {
            session.push(other_tool(i + 1));
        }
    }

    assert_eq!(session.entries().len(), MAX_ENTRIES);
    assert_eq!(session.dropped(), 50);
}

#[test]
fn eviction_keeps_history_bounded_including_prompts() {
    let mut session = ConnSession::default();
    for i in 0..MAX_ENTRIES as i64 + 10 {
        session.push(prompt(&format!("ask {i}"), i));
    }

    assert_eq!(session.entries().len(), MAX_ENTRIES);
    assert_eq!(session.dropped(), 10);
    assert_eq!(
        session.prompts().next(),
        Some("ask 10"),
        "old prompts must be evicted once the bounded history is full"
    );
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

fn turn_with_prompt(
    started: i64,
    ended: Option<i64>,
    outcome: Option<&str>,
    prompt: &str,
) -> ConnTurn {
    ConnTurn {
        started_at: at(started),
        ended_at: ended.map(at),
        prompt: prompt.to_owned(),
        steps: vec![],
        outcome: outcome.map(str::to_owned),
    }
}

fn story(turns: Vec<ConnTurn>) -> ConnStory {
    ConnStory {
        title: None,
        plan_file: None,
        turns,
    }
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

#[test]
fn a_transcript_story_does_not_hide_a_failure_entry() {
    let mut session = ConnSession::default();
    session.push(prompt("do the thing", 1));
    session.push(ConnEntry::new(
        ConnEntryKind::Failed {
            error_type: Some("rate_limit".to_owned()),
            message: Some("try again".to_owned()),
        },
        at(9),
    ));

    assert!(session.adopt_story(story(vec![turn(1, Some(3), Some("partial"))]), at(9)));
    assert_eq!(session.in_flight().len(), 1);
    assert!(matches!(
        session.in_flight()[0].kind,
        ConnEntryKind::Failed { .. }
    ));
}

#[test]
fn a_truncated_hook_prompt_matches_the_full_transcript_prompt() {
    let mut session = ConnSession::default();
    let full = "x".repeat(240);
    session.push(prompt(&full[..200], 100));

    assert!(session.adopt_story(
        story(vec![turn_with_prompt(1, Some(2), Some("done"), &full)]),
        at(100)
    ));
    assert!(session.in_flight().is_empty());
}

#[test]
fn repeated_prompts_are_matched_one_at_a_time() {
    let mut session = ConnSession::default();
    session.push(prompt("repeat this", 10));
    session.push(prompt("repeat this", 20));

    assert!(session.adopt_story(
        story(vec![
            turn_with_prompt(1, Some(2), Some("first"), "repeat this"),
            turn_with_prompt(3, Some(4), Some("second"), "repeat this"),
        ]),
        at(20)
    ));
    assert!(session.in_flight().is_empty());
}

#[test]
fn a_transcript_turn_without_live_entries_does_not_stop_matching() {
    let mut session = ConnSession::default();
    // The hook history starts at the second turn: the first was evicted, or the conversation
    // was resumed in this pane.
    session.push(prompt("second", 10));
    session.push(responded("two", 12));

    assert!(session.adopt_story(
        story(vec![
            turn_with_prompt(1, Some(2), Some("one"), "first"),
            turn_with_prompt(10, Some(12), Some("two"), "second"),
        ]),
        at(12)
    ));
    assert!(session.in_flight().is_empty());
}

#[test]
fn entries_before_the_first_matched_prompt_are_covered() {
    let mut session = ConnSession::default();
    session.push(tool(3));
    session.push(prompt("second", 10));

    assert!(session.adopt_story(
        story(vec![
            turn_with_prompt(1, Some(4), Some("one"), "first"),
            turn_with_prompt(10, Some(11), Some("two"), "second"),
        ]),
        at(11)
    ));
    assert!(session.in_flight().is_empty());
}

#[test]
fn a_failure_followed_by_another_turn_does_not_drag_covered_entries_back() {
    let mut session = ConnSession::default();
    session.push(prompt("first", 1));
    session.push(ConnEntry::new(
        ConnEntryKind::Failed {
            error_type: Some("rate_limit".to_owned()),
            message: None,
        },
        at(3),
    ));
    session.push(prompt("second", 10));
    session.push(responded("two", 12));

    assert!(session.adopt_story(
        story(vec![
            turn_with_prompt(1, Some(3), Some("one"), "first"),
            turn_with_prompt(10, Some(12), Some("two"), "second"),
        ]),
        at(12)
    ));
    assert!(
        session.in_flight().is_empty(),
        "the old failure is history once a later turn exists"
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

/// A finished turn is read twice at the same boundary: once immediately, and
/// again after the transcript has had a moment to flush its closing message.
/// The second read carries the agent's actual last word, so it has to be able
/// to replace the first.
#[test]
fn a_re_read_at_the_same_boundary_replaces_the_story() {
    let mut session = ConnSession::default();
    let raced = story(vec![turn(1, Some(3), Some("I'll check the repo."))]);
    let flushed = story(vec![turn(1, Some(5), Some("No plan on main."))]);

    assert!(session.adopt_story(raced, at(9)));
    assert!(session.adopt_story(flushed, at(9)));
    assert_eq!(
        session.story().expect("story kept").turns[0]
            .outcome
            .as_deref(),
        Some("No plan on main.")
    );
}

/// An unfinished turn's coverage is its last transcript activity, which can be
/// earlier than the read that produced it. That must not become the ordering
/// boundary, or an older read dispatched in between would pass the stale guard
/// and rewind the story.
#[test]
fn an_unfinished_turn_does_not_let_an_older_read_replace_a_newer_one() {
    let mut session = ConnSession::default();
    let newer = story(vec![turn(10, Some(20), None), turn(21, None, None)]);
    let older = story(vec![turn(10, Some(20), None)]);

    assert!(session.adopt_story(newer, at(30)));
    assert!(!session.adopt_story(older, at(25)));
    assert_eq!(session.story().expect("story kept").turns.len(), 2);
}

/// The intentional equal-boundary reread still works after an unfinished turn.
#[test]
fn an_equal_boundary_reread_still_replaces_an_unfinished_story() {
    let mut session = ConnSession::default();
    assert!(session.adopt_story(story(vec![turn(10, Some(20), None)]), at(30)));
    assert!(session.adopt_story(story(vec![turn(10, Some(28), Some("done"))]), at(30)));
}

/// A read that finds nothing has told nothing, so it hides nothing.
#[test]
fn an_empty_story_does_not_hide_live_entries() {
    let mut session = ConnSession::default();
    session.push(prompt("do the thing", 1));
    session.push(tool(2));

    assert!(session.adopt_story(story(vec![]), at(5)));
    assert_eq!(session.in_flight().len(), 2);
}

/// A read triggered by a new prompt can land before the transcript has flushed
/// it, leaving the previous answered turn as the story's latest. That story does
/// not contain the prompt, so the prompt must stay visible; the previous turn's
/// own hook entries stay covered.
#[test]
fn a_story_behind_a_newer_live_prompt_does_not_hide_it() {
    let mut session = ConnSession::default();
    session.push(prompt("first", 1));
    session.push(responded("done", 9));
    session.push(prompt("second", 10));
    session.push(tool(11));

    assert!(session.adopt_story(
        story(vec![turn_with_prompt(1, Some(5), Some("done"), "first")]),
        at(11)
    ));

    let kinds: Vec<_> = session
        .in_flight()
        .iter()
        .map(|entry| entry.kind.clone())
        .collect();
    assert_eq!(
        kinds,
        vec![
            ConnEntryKind::Prompt {
                text: "second".to_owned()
            },
            tool(11).kind,
        ]
    );
}

/// Once the transcript catches up, the prompt is covered again.
#[test]
fn a_story_that_reaches_the_newer_prompt_covers_it() {
    let mut session = ConnSession::default();
    session.push(prompt("first", 1));
    session.push(responded("done", 9));
    session.push(prompt("second", 10));

    assert!(session.adopt_story(
        story(vec![
            turn_with_prompt(1, Some(5), Some("done"), "first"),
            turn_with_prompt(10, Some(10), None, "second"),
        ]),
        at(11)
    ));
    assert!(session.in_flight().is_empty());
}

/// While a new prompt has not reached the transcript, the story's latest turn
/// is still the previous, finished one: its answer is unseen, not narration.
#[test]
fn the_previous_turn_counts_as_unseen_while_a_new_prompt_is_live_only() {
    let mut session = ConnSession::default();
    session.push(prompt("first", 1));
    session.push(responded("done", 9));
    session.push(prompt("second", 10));
    session.status = super::ConnStatus::Working;
    assert!(session.adopt_story(
        story(vec![turn_with_prompt(1, Some(5), Some("done"), "first")]),
        at(11)
    ));

    assert_eq!(session.unseen_turns(), 1);
    assert_eq!(
        session.first_unseen_turn().map(|turn| turn.started_at),
        Some(at(1))
    );
}

/// Without a newer prompt a working session's last turn is still running.
#[test]
fn a_running_last_turn_is_still_not_counted_as_unseen() {
    let mut session = ConnSession::default();
    session.push(prompt("first", 1));
    session.status = super::ConnStatus::Working;
    assert!(session.adopt_story(
        story(vec![turn_with_prompt(
            1,
            Some(5),
            Some("narration"),
            "first"
        )]),
        at(6)
    ));

    assert_eq!(session.unseen_turns(), 0);
}

/// A transcript read some time after its dispatch can hold records later than
/// the dispatch boundary; coverage still must not run past the boundary or panic.
#[test]
fn a_story_newer_than_its_dispatch_boundary_is_handled() {
    let mut session = ConnSession::default();
    session.push(prompt("first", 1));
    session.push(responded("done", 4));

    assert!(session.adopt_story(
        story(vec![turn_with_prompt(1, Some(9), Some("done"), "first")]),
        at(4)
    ));
    assert!(session.in_flight().is_empty());
}

/// A read that sees the closing message before the `Stop` hook arrives leaves the hook's own
/// `Responded` entry after the story's boundary. The reads the hook triggers find the transcript
/// as long as it was, and parse nothing, but they must still move the boundary, or the answer
/// shows twice: once from the story and once from the entry.
#[test]
fn an_unchanged_transcript_still_covers_a_later_hook_entry() {
    let mut session = ConnSession::default();
    session.push(prompt("fix it", 1));
    session.push(tool(2));
    assert!(session.adopt_story(
        story(vec![turn_with_prompt(1, Some(5), Some("Done."), "fix it")]),
        at(5)
    ));
    session.push(responded("Done.", 6));
    assert_eq!(
        session.in_flight().len(),
        1,
        "the hook's entry is after the boundary the story was taken at"
    );

    assert!(session.recheck_story(at(6)));

    assert!(
        session.in_flight().is_empty(),
        "the story already holds this answer"
    );
}

#[test]
fn a_recheck_cannot_rewind_or_invent_a_story() {
    let mut session = ConnSession::default();
    assert!(
        !session.recheck_story(at(1)),
        "with no story there is nothing to recheck"
    );

    session.push(prompt("fix it", 1));
    assert!(session.adopt_story(
        story(vec![turn_with_prompt(1, Some(5), Some("Done."), "fix it")]),
        at(9)
    ));
    assert!(
        !session.recheck_story(at(7)),
        "a read from before the story's own must not move the boundary back"
    );
}

/// The turn is still running when the hook arrives, so the boundary is the transcript's own last
/// activity and a recheck has nothing to move: the closing message has not been written yet. It
/// changes nothing a reader sees, so it says so.
#[test]
fn a_recheck_does_not_cover_a_turn_the_transcript_has_not_finished() {
    let mut session = ConnSession::default();
    session.push(prompt("fix it", 1));
    assert!(session.adopt_story(
        story(vec![turn_with_prompt(1, Some(3), None, "fix it")]),
        at(4)
    ));
    session.push(responded("Done.", 6));

    assert!(!session.recheck_story(at(6)));

    assert_eq!(session.in_flight().len(), 1);
}

/// A resumed conversation starts with turns this pane never saw. Saying `continue` again must not
/// be taken for the `continue` of one of them, or the new instruction would vanish from the live
/// entries and the old turn's answer would be shown as its result.
#[test]
fn a_repeated_prompt_is_not_matched_to_an_older_turn_that_reads_the_same() {
    let mut session = ConnSession::default();
    session.push(prompt("continue", 100_000));
    assert!(session.adopt_story(
        story(vec![
            turn_with_prompt(1_000, Some(1_060), Some("Old answer."), "continue"),
            turn_with_prompt(2_000, Some(2_060), Some("Older answer."), "tidy up"),
        ]),
        at(100_005)
    ));

    assert_eq!(
        session.in_flight().len(),
        1,
        "the new prompt has not reached the transcript, and is still live"
    );
}

/// The same words, once the transcript has the new turn too: now the prompt does belong to a turn,
/// and it must be the new one rather than the first that reads the same.
#[test]
fn a_repeated_prompt_is_matched_to_the_turn_that_began_when_it_arrived() {
    let mut session = ConnSession::default();
    session.push(prompt("continue", 100_000));
    session.push(responded("All done.", 100_040));
    assert!(session.adopt_story(
        story(vec![
            turn_with_prompt(1_000, Some(1_060), Some("Old answer."), "continue"),
            turn_with_prompt(100_001, Some(100_040), Some("All done."), "continue"),
        ]),
        at(100_041)
    ));

    assert!(
        session.in_flight().is_empty(),
        "the story holds the turn this prompt began"
    );
}
