//! Turns a Claude Code transcript into the story of what happened.
//!
//! The hook events Warp forwards are good for live status and for binding a
//! session to a pane, but the plugin truncates prompts and responses to 200
//! characters and sends no tool inputs at all. The transcript it points at has
//! the full material, and — usefully — a lot of it is already prose the model
//! wrote while working: every `Bash` and `Agent` call carries a one-line
//! `description`, and the session has an `aiTitle`.
//!
//! So a readable account needs no summarisation of our own. It needs the
//! transcript grouped into turns.
//!
//! Parsing is deliberately lenient. The file is appended to while a session
//! runs, so the last line can be half-written, and new record types appear as
//! Claude Code changes. Anything that fails to parse is skipped rather than
//! failing the read.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One exchange: what was asked, what was done, what came back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnTurn {
    pub started_at: DateTime<Utc>,
    /// The instruction as typed, untruncated.
    pub prompt: String,
    /// What the agent did, in order, in its own words.
    pub steps: Vec<ConnStep>,
    /// The agent's closing message for this turn, untruncated. `None` while
    /// the turn is still running.
    pub outcome: Option<String>,
}

/// A step the agent took. `runs` counts immediately repeated identical steps,
/// since a description repeated eight times says no more than once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnStep {
    pub description: String,
    pub runs: usize,
}

/// A session's story.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnStory {
    /// Claude Code's own generated session title, when it has one.
    pub title: Option<String>,
    pub turns: Vec<ConnTurn>,
}

impl ConnStory {
    /// The most recent turn, which is the one a returning reader wants first.
    pub fn latest_turn(&self) -> Option<&ConnTurn> {
        self.turns.last()
    }
}

/// Reads a transcript in JSONL form.
///
/// Lenient by design: unparseable lines are skipped. See the module docs.
pub fn parse_transcript(jsonl: &str) -> ConnStory {
    let mut records: Vec<Record> = jsonl
        .lines()
        .filter_map(|line| serde_json::from_str::<Record>(line).ok())
        .collect();

    // `aiTitle` records carry no timestamp, so take the title before sorting
    // discards their order; the last one written is the current title.
    let title = records
        .iter()
        .rev()
        .find_map(|record| record.ai_title.clone())
        .filter(|title| !title.trim().is_empty());

    // Subagent records (`isSidechain`) are a different thread of work, and
    // `isMeta` records are injected by the harness. Neither is part of the
    // story the user took part in.
    records.retain(|record| {
        !record.is_sidechain
            && !record.is_meta
            && record.timestamp.is_some()
            && matches!(record.kind.as_deref(), Some("user") | Some("assistant"))
    });
    records.sort_by_key(|record| record.timestamp);

    let mut turns: Vec<ConnTurn> = Vec::new();
    for record in &records {
        if let Some(prompt) = record.typed_prompt() {
            turns.push(ConnTurn {
                started_at: record
                    .timestamp
                    .expect("filtered to records with a timestamp"),
                prompt,
                steps: Vec::new(),
                outcome: None,
            });
            continue;
        }
        // Anything before the first typed prompt belongs to no turn — a
        // resumed session replays earlier context, and the harness injects
        // records of its own.
        let Some(turn) = turns.last_mut() else {
            continue;
        };
        for step in record.tool_steps() {
            push_step(&mut turn.steps, step);
        }
        if let Some(text) = record.assistant_text() {
            turn.outcome = Some(text);
        }
    }

    ConnStory { title, turns }
}

/// Appends a step, folding it into the previous one when identical.
fn push_step(steps: &mut Vec<ConnStep>, description: String) {
    match steps.last_mut() {
        Some(last) if last.description == description => last.runs += 1,
        _ => steps.push(ConnStep {
            description,
            runs: 1,
        }),
    }
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<DateTime<Utc>>,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: bool,
    #[serde(rename = "isMeta", default)]
    is_meta: bool,
    /// `"typed"` for something the user wrote. `"system"` covers harness
    /// injections such as background task notifications, which arrive as user
    /// messages but were never typed by anyone.
    #[serde(rename = "promptSource")]
    prompt_source: Option<String>,
    #[serde(rename = "aiTitle")]
    ai_title: Option<String>,
    message: Option<Message>,
}

impl Record {
    fn typed_prompt(&self) -> Option<String> {
        if self.kind.as_deref() != Some("user") || self.prompt_source.as_deref() != Some("typed") {
            return None;
        }
        let text = match self.message.as_ref()?.content.as_ref()? {
            Content::Text(text) => text.clone(),
            Content::Blocks(blocks) => blocks
                .iter()
                .filter(|block| block.kind == "text")
                .filter_map(|block| block.text.as_deref())
                .collect::<Vec<_>>()
                .join(" "),
        };
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }

    /// Each tool call as the model described it, falling back to the tool's
    /// name when it reported no description.
    fn tool_steps(&self) -> Vec<String> {
        let Some(Content::Blocks(blocks)) = self.message.as_ref().and_then(|m| m.content.as_ref())
        else {
            return Vec::new();
        };
        blocks
            .iter()
            .filter(|block| block.kind == "tool_use")
            .filter_map(|block| {
                block
                    .input
                    .as_ref()
                    .and_then(|input| input.get("description"))
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|description| !description.is_empty())
                    .map(str::to_owned)
                    .or_else(|| block.name.clone())
            })
            .collect()
    }

    /// The assistant's prose for this record, ignoring `thinking` blocks.
    fn assistant_text(&self) -> Option<String> {
        if self.kind.as_deref() != Some("assistant") {
            return None;
        }
        let Content::Blocks(blocks) = self.message.as_ref()?.content.as_ref()? else {
            return None;
        };
        let text = blocks
            .iter()
            .filter(|block| block.kind == "text")
            .filter_map(|block| block.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n\n");
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }
}

#[derive(Deserialize)]
struct Message {
    content: Option<Content>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    name: Option<String>,
    input: Option<serde_json::Value>,
}

#[cfg(test)]
#[path = "story_tests.rs"]
mod tests;
