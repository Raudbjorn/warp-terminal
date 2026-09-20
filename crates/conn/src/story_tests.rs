use super::{ConnStep, parse_transcript};

/// Shapes match a real Claude Code transcript: assistant records carry no
/// `promptId`, tool calls put their prose in `input.description`, and harness
/// injections arrive as user records with `promptSource: "system"`.
fn transcript(lines: &[&str]) -> String {
    lines.join("\n")
}

fn typed(ts: &str, text: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{ts}","promptSource":"typed","origin":{{"kind":"human"}},"message":{{"content":"{text}"}}}}"#
    )
}

fn tool(ts: &str, name: &str, description: &str) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","message":{{"content":[{{"type":"tool_use","name":"{name}","input":{{"command":"x","description":"{description}"}}}}]}}}}"#
    )
}

fn said(ts: &str, text: &str) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

#[test]
fn reads_a_turn_as_prompt_steps_and_outcome() {
    let jsonl = transcript(&[
        &typed("2026-09-20T10:00:00Z", "wire ADRs into the spec system"),
        &tool("2026-09-20T10:00:05Z", "Bash", "Explore the docs layout"),
        &tool("2026-09-20T10:00:09Z", "Bash", "Add ADR settings"),
        &said(
            "2026-09-20T10:01:00Z",
            "ADRs were assumed but never wired in.",
        ),
    ]);

    let story = parse_transcript(&jsonl);
    assert_eq!(story.turns.len(), 1);
    let turn = &story.turns[0];
    assert_eq!(turn.prompt, "wire ADRs into the spec system");
    assert_eq!(
        turn.steps,
        vec![
            ConnStep {
                description: "Explore the docs layout".to_owned(),
                runs: 1
            },
            ConnStep {
                description: "Add ADR settings".to_owned(),
                runs: 1
            },
        ]
    );
    assert_eq!(
        turn.outcome.as_deref(),
        Some("ADRs were assumed but never wired in.")
    );
}

/// The whole reason to read the transcript is that the plugin's 200-character
/// cap loses the middle of a real instruction.
#[test]
fn prompts_are_not_truncated() {
    let long = "a".repeat(900);
    let jsonl = typed("2026-09-20T10:00:00Z", &long);
    let story = parse_transcript(&jsonl);
    assert_eq!(story.turns[0].prompt.len(), 900);
}

/// Background task notifications arrive as user records. Treating them as
/// prompts would split one turn into several and put text the user never
/// wrote into the spine.
#[test]
fn harness_injections_do_not_start_turns() {
    let jsonl = transcript(&[
        &typed("2026-09-20T10:00:00Z", "run the tests"),
        r#"{"type":"user","timestamp":"2026-09-20T10:00:03Z","promptSource":"system","origin":{"kind":"task-notification"},"message":{"content":"<task-notification>done</task-notification>"}}"#,
        &tool("2026-09-20T10:00:05Z", "Bash", "Run the test suite"),
    ]);

    let story = parse_transcript(&jsonl);
    assert_eq!(story.turns.len(), 1, "one typed prompt is one turn");
    assert_eq!(story.turns[0].prompt, "run the tests");
    assert_eq!(story.turns[0].steps.len(), 1, "work stays with its turn");
}

/// Subagent work is a separate thread and would otherwise interleave into the
/// parent's step list.
#[test]
fn subagent_and_meta_records_are_excluded() {
    let jsonl = transcript(&[
        &typed("2026-09-20T10:00:00Z", "investigate"),
        r#"{"type":"assistant","timestamp":"2026-09-20T10:00:02Z","isSidechain":true,"message":{"content":[{"type":"tool_use","name":"Bash","input":{"description":"subagent work"}}]}}"#,
        r#"{"type":"user","timestamp":"2026-09-20T10:00:03Z","isMeta":true,"promptSource":"typed","message":{"content":"injected context"}}"#,
        &tool("2026-09-20T10:00:04Z", "Bash", "real work"),
    ]);

    let story = parse_transcript(&jsonl);
    assert_eq!(story.turns.len(), 1);
    assert_eq!(
        story.turns[0].steps,
        vec![ConnStep {
            description: "real work".to_owned(),
            runs: 1
        }]
    );
}

#[test]
fn repeated_identical_steps_fold_with_a_count() {
    let mut lines = vec![typed("2026-09-20T10:00:00Z", "build it")];
    for i in 0..7 {
        lines.push(tool(
            &format!("2026-09-20T10:00:{:02}Z", 10 + i),
            "Bash",
            "Check the build",
        ));
    }
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();

    let story = parse_transcript(&transcript(&refs));
    assert_eq!(
        story.turns[0].steps,
        vec![ConnStep {
            description: "Check the build".to_owned(),
            runs: 7
        }]
    );
}

/// Records are not guaranteed to be in order, and a turn assembled out of
/// order reads as nonsense.
#[test]
fn records_are_ordered_by_timestamp() {
    let jsonl = transcript(&[
        &said("2026-09-20T10:05:00Z", "second outcome"),
        &typed("2026-09-20T10:00:00Z", "first ask"),
        &typed("2026-09-20T10:04:00Z", "second ask"),
        &said("2026-09-20T10:01:00Z", "first outcome"),
    ]);

    let story = parse_transcript(&jsonl);
    assert_eq!(story.turns.len(), 2);
    assert_eq!(story.turns[0].prompt, "first ask");
    assert_eq!(story.turns[0].outcome.as_deref(), Some("first outcome"));
    assert_eq!(story.turns[1].prompt, "second ask");
    assert_eq!(story.turns[1].outcome.as_deref(), Some("second outcome"));
}

/// The file is appended to while the session runs, so a read can land on a
/// half-written final line. Losing the whole story over that would make the
/// panel blank at exactly the wrong moment.
#[test]
fn a_truncated_final_line_is_skipped() {
    let jsonl = format!(
        "{}\n{}\n{{\"type\":\"assist",
        typed("2026-09-20T10:00:00Z", "keep going"),
        said("2026-09-20T10:00:10Z", "done")
    );
    let story = parse_transcript(&jsonl);
    assert_eq!(story.turns.len(), 1);
    assert_eq!(story.turns[0].outcome.as_deref(), Some("done"));
}

#[test]
fn session_title_comes_from_the_ai_title_record() {
    let jsonl = transcript(&[
        r#"{"type":"ai-title","aiTitle":"an early guess","sessionId":"s"}"#,
        &typed("2026-09-20T10:00:00Z", "go"),
        r#"{"type":"ai-title","aiTitle":"Conn re-entry layer","sessionId":"s"}"#,
    ]);

    let story = parse_transcript(&jsonl);
    assert_eq!(
        story.title.as_deref(),
        Some("Conn re-entry layer"),
        "the most recent title wins"
    );
}

#[test]
fn a_tool_call_without_a_description_falls_back_to_its_name() {
    let jsonl = transcript(&[
        &typed("2026-09-20T10:00:00Z", "go"),
        r#"{"type":"assistant","timestamp":"2026-09-20T10:00:01Z","message":{"content":[{"type":"tool_use","name":"ListAgents","input":{}}]}}"#,
    ]);

    let story = parse_transcript(&jsonl);
    assert_eq!(
        story.turns[0].steps,
        vec![ConnStep {
            description: "ListAgents".to_owned(),
            runs: 1
        }]
    );
}

/// `thinking` blocks are not the agent's account of what it did.
#[test]
fn thinking_blocks_are_not_the_outcome() {
    let jsonl = transcript(&[
        &typed("2026-09-20T10:00:00Z", "go"),
        r#"{"type":"assistant","timestamp":"2026-09-20T10:00:01Z","message":{"content":[{"type":"thinking","thinking":"pondering"}]}}"#,
        &said("2026-09-20T10:00:02Z", "the real answer"),
    ]);

    let story = parse_transcript(&jsonl);
    assert_eq!(story.turns[0].outcome.as_deref(), Some("the real answer"));
}

/// An in-flight turn has no closing message yet, and the panel needs to be
/// able to tell that apart from a turn that ended silently.
#[test]
fn an_unfinished_turn_has_no_outcome() {
    let jsonl = transcript(&[
        &typed("2026-09-20T10:00:00Z", "start something long"),
        &tool("2026-09-20T10:00:01Z", "Bash", "Kick off the build"),
    ]);

    let story = parse_transcript(&jsonl);
    assert_eq!(story.turns[0].outcome, None);
    assert_eq!(story.turns[0].steps.len(), 1);
}

/// The panel merges the story with the live hook entries, so it needs to know
/// how far the story reaches. Anything the transcript has not recorded yet is
/// still in flight.
#[test]
fn a_turn_ends_at_its_last_record() {
    let jsonl = transcript(&[
        &typed("2026-09-20T10:00:00Z", "first ask"),
        &said("2026-09-20T10:00:30Z", "first outcome"),
        &typed("2026-09-20T10:01:00Z", "second ask"),
        &tool("2026-09-20T10:01:10Z", "Bash", "Still working"),
    ]);

    let story = parse_transcript(&jsonl);
    assert_eq!(
        story.turns[0].ended_at,
        Some("2026-09-20T10:00:30Z".parse().expect("valid time")),
        "a finished turn ends when it said its last word"
    );
    assert_eq!(
        story.turns[1].last_activity(),
        "2026-09-20T10:01:10Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .expect("valid time"),
        "an unfinished turn reaches as far as its last step"
    );
}

/// A prompt nothing has happened on yet has no end, so its last activity is
/// when it was sent.
#[test]
fn a_turn_with_no_activity_ends_nowhere() {
    let jsonl = typed("2026-09-20T10:00:00Z", "just asked");
    let story = parse_transcript(&jsonl);
    assert_eq!(story.turns[0].ended_at, None);
    assert_eq!(story.turns[0].last_activity(), story.turns[0].started_at);
}
