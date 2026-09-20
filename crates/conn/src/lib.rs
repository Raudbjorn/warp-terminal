//! Session history for Conn, the re-entry layer for CLI agent sessions.
//!
//! Warp already tracks a CLI agent session's *current* state. That model is
//! lossy on purpose: it keeps only the latest prompt and response, and clears
//! permission details once they stop being relevant. Conn answers a different
//! question — "what did I ask for, and what has it decided since?" — which
//! needs the history that model discards.
//!
//! This crate is deliberately free of Warp dependencies so the history model
//! stays portable if the store ever moves out of the client process.

pub mod story;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::story::ConnStory;

/// Strips the wrapper Claude Code puts around pasted text.
///
/// A pasted prompt arrives as `<pasted_content id="0e44">…</pasted_content>`.
/// The tag is addressed to the agent; to a person coming back to the pane it
/// is noise sitting exactly where the instruction should be.
pub fn tidy_prompt(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(start) = rest.find("<pasted_content") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        // An unterminated tag means the hook truncated the prompt mid-tag, so
        // there is nothing after it left to keep.
        let Some(end) = rest[start..].find('>') else {
            break;
        };
        rest = &rest[start + end + 1..];
    }
    out.replace("</pasted_content>", "").trim().to_owned()
}

/// Blocks the harness submits as prompts, which nobody typed.
///
/// Background task notifications arrive through the same path as a real
/// instruction. In the transcript they are marked `promptSource: "system"`,
/// but the hook payload carries only the text, so they are recognised by their
/// opening tag.
const HARNESS_PROMPT_TAGS: &[&str] = &[
    "<task-notification>",
    "<system-reminder>",
    "<local-command-",
    "<command-name>",
];

/// Whether a prompt was written by the harness rather than by a person.
///
/// The spine is meant to read as "what I asked for". A notification the user
/// never wrote breaks that, and it is the one column the panel exists for.
pub fn is_harness_prompt(text: &str) -> bool {
    let text = text.trim_start();
    HARNESS_PROMPT_TAGS.iter().any(|tag| text.starts_with(tag))
}

/// Entries retained per session before the oldest evictable one is dropped.
///
/// Bounds memory and keeps the panel readable on a long session. Prompts are
/// exempt (see [`ConnSession::push`]).
pub const MAX_ENTRIES: usize = 500;

/// One thing that happened in a session, in the vocabulary of someone
/// returning to it rather than of the hook that reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnEntryKind {
    /// An instruction the user sent. The spine of the session.
    Prompt { text: String },
    /// The agent stopped to ask permission — a fork in the session, and the
    /// most interesting thing to find on return.
    PermissionRequested {
        summary: Option<String>,
        tool_name: Option<String>,
        /// The command or file path the tool was about to act on, as far as
        /// the plugin reports it.
        target: Option<String>,
    },
    /// A pending permission request was answered.
    PermissionResolved,
    /// The agent asked the user a question and is waiting.
    QuestionAsked { summary: Option<String> },
    /// Consecutive tool calls of the same kind, coalesced.
    ///
    /// The Warp plugin reports only a tool name for a completed call, with no
    /// command or path, so twenty separate `Bash` entries carry exactly as much
    /// information as one and bury the prompts between them. `runs` keeps the
    /// sense of how much work happened without the wall of identical rows.
    ToolCompleted {
        tool_name: Option<String>,
        target: Option<String>,
        runs: usize,
    },
    /// The agent finished its turn.
    Responded { text: Option<String> },
    /// The turn ended in failure.
    Failed {
        error_type: Option<String>,
        message: Option<String>,
    },
}

impl ConnEntryKind {
    /// Whether this entry may be dropped to stay under [`MAX_ENTRIES`].
    ///
    /// Prompts are not evictable: they are the reason Conn exists, they are
    /// bounded by how fast a person can type, and losing the earliest one
    /// loses the session's original intent.
    pub fn is_evictable(&self) -> bool {
        !matches!(self, ConnEntryKind::Prompt { .. })
    }
}

/// A timestamped entry in a session's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnEntry {
    pub at: DateTime<Utc>,
    pub kind: ConnEntryKind,
}

impl ConnEntry {
    pub fn new(kind: ConnEntryKind, at: DateTime<Utc>) -> Self {
        Self { at, kind }
    }
}

/// Everything Conn knows about one CLI agent session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnSession {
    /// The agent's own session identifier, once it reports one.
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    /// Project name, as the plugin derives it from `cwd`.
    pub project: Option<String>,
    /// Where the agent writes its transcript, once it reports one. The hook
    /// events are truncated; this file is not.
    pub transcript_path: Option<String>,
    /// The narrative account read from the transcript, covering everything up
    /// to [`Self::story_through`].
    story: Option<ConnStory>,
    /// How far the story reaches. Live entries recorded at or before this
    /// moment are already told by the story, so showing them again would
    /// repeat the turn the reader just read.
    story_through: Option<DateTime<Utc>>,
    /// Chronological history. Order is the product: "I asked X, then it
    /// decided Y, then I asked Z."
    entries: Vec<ConnEntry>,
    /// Entries dropped to stay under [`MAX_ENTRIES`], so a reader can say how
    /// much history is missing instead of silently showing a partial log.
    dropped: usize,
}

impl ConnSession {
    pub fn entries(&self) -> &[ConnEntry] {
        &self.entries
    }

    pub fn story(&self) -> Option<&ConnStory> {
        self.story.as_ref()
    }

    /// Takes a story read from the transcript, covering live entries up to
    /// `through`. Returns whether it was taken.
    ///
    /// `through` is when the turn-completion event that triggered the read
    /// arrived — our own clock, so it orders reliably even though reads are
    /// dispatched from the main thread and resolve off it. A read that
    /// resolves after a later one is stale and is dropped rather than
    /// rewinding the panel.
    pub fn adopt_story(&mut self, story: ConnStory, through: DateTime<Utc>) -> bool {
        if self.story_through.is_some_and(|current| current >= through) {
            return false;
        }
        // The transcript is written asynchronously, so a read can land before
        // the turn's closing message reaches the file. Trust the story only as
        // far as it actually goes, so the live entry carrying the response is
        // still shown rather than hidden behind a story that lacks it.
        let through = match story.turns.last() {
            Some(turn) if turn.outcome.is_none() => turn.last_activity(),
            _ => through,
        };
        self.story_through = Some(through);
        self.story = Some(story);
        true
    }

    /// Live entries the story does not yet cover: the turn in flight.
    ///
    /// Entries are appended as events arrive, so `at` is non-decreasing and
    /// the boundary is a partition rather than a scan.
    pub fn in_flight(&self) -> &[ConnEntry] {
        let Some(through) = self.story_through else {
            return &self.entries;
        };
        let start = self.entries.partition_point(|entry| entry.at <= through);
        &self.entries[start..]
    }

    /// Number of entries evicted from the front of the history.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// Appends an entry, evicting the oldest evictable one if that would
    /// exceed [`MAX_ENTRIES`].
    ///
    /// A session consisting only of prompts is allowed to exceed the cap: no
    /// prompt is ever dropped, and human typing bounds how many there can be.
    pub fn push(&mut self, entry: ConnEntry) {
        if self.coalesce_tool_run(&entry) {
            return;
        }
        self.entries.push(entry);
        if self.entries.len() <= MAX_ENTRIES {
            return;
        }
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.kind.is_evictable())
        {
            self.entries.remove(index);
            self.dropped += 1;
        }
    }

    /// Folds a repeated tool call into the previous entry, returning whether
    /// it was absorbed.
    fn coalesce_tool_run(&mut self, incoming: &ConnEntry) -> bool {
        let ConnEntryKind::ToolCompleted {
            tool_name,
            target,
            runs,
        } = &incoming.kind
        else {
            return false;
        };
        let Some(last) = self.entries.last_mut() else {
            return false;
        };
        let ConnEntryKind::ToolCompleted {
            tool_name: last_tool,
            target: last_target,
            runs: last_runs,
        } = &mut last.kind
        else {
            return false;
        };
        if last_tool != tool_name || last_target != target {
            return false;
        }
        *last_runs += runs;
        // The run is still in progress, so the entry's timestamp advances to
        // the most recent call rather than staying at the first.
        last.at = incoming.at;
        true
    }

    /// Every instruction sent, oldest first. The panel's spine.
    pub fn prompts(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().filter_map(|entry| match &entry.kind {
            ConnEntryKind::Prompt { text } => Some(text.as_str()),
            _ => None,
        })
    }

    /// The most recent thing the agent said, if it has finished a turn.
    pub fn last_response(&self) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| match &entry.kind {
                ConnEntryKind::Responded { text } => text.as_deref(),
                _ => None,
            })
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
