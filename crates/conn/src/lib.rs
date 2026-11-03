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

pub mod overview;
pub mod seen;
pub mod story;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::story::{ConnStory, ConnTurn};

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

/// Entries retained per session before the oldest entry is dropped.
///
/// Bounds memory and keeps the panel readable on a long session. Prompts are
/// bounded too: an unattended or automated session must not grow without limit.
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
    pub fn is_evictable(&self) -> bool {
        true
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

/// Where a session stands, in the terms a returning reader acts on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnStatus {
    #[default]
    Working,
    /// Waiting on a permission, a question or plan approval.
    NeedsYou,
    /// Stopped, including a turn the user cancelled.
    Done,
    Failed,
    /// The agent process has exited.
    Ended,
}

impl ConnStatus {
    pub fn label(self) -> &'static str {
        match self {
            ConnStatus::Working => "Working",
            ConnStatus::NeedsYou => "Needs you",
            ConnStatus::Done => "Done",
            ConnStatus::Failed => "Failed",
            ConnStatus::Ended => "Ended",
        }
    }
}

/// Everything Conn knows about one CLI agent session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnSession {
    /// The agent's own session identifier, once it reports one.
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    /// The repository the session runs in, or its working directory outside git.
    pub project: Option<String>,
    pub status: ConnStatus,
    /// Where the agent writes its transcript, once it reports one. The hook
    /// events are truncated; this file is not.
    pub transcript_path: Option<String>,
    /// The time of the latest activity the reader has seen in this pane.
    pub watermark: Option<DateTime<Utc>>,
    /// The narrative account read from the transcript.
    story: Option<ConnStory>,
    /// When the read that produced the story was dispatched. Orders reads
    /// against each other only; it says nothing about what the story contains.
    story_read_at: Option<DateTime<Utc>>,
    /// How far the story reaches. Live entries recorded at or before this
    /// moment are already told by the story, so showing them again would
    /// repeat the turn the reader just read. Never covers a live prompt the
    /// story does not contain (see [`Self::in_flight`]).
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

    /// Takes a story read from the transcript, dispatched at `through`.
    /// Returns whether it was taken.
    ///
    /// `through` is when the event that triggered the read arrived — our own
    /// clock, so it orders reliably even though reads are dispatched from the
    /// main thread and resolve off it. A read that resolves after a later one is
    /// stale and is dropped rather than rewinding the panel.
    pub fn adopt_story(&mut self, story: ConnStory, through: DateTime<Utc>) -> bool {
        self.adopt_story_until(story, through, through)
    }

    /// [`Self::adopt_story`] for a read whose story is to cover less than the event that
    /// triggered it. `read_at` orders the read against others; `covered_until` is how far the
    /// story is taken to cover the live entries. They differ when the triggering event is a
    /// request the story must not hide: its own entry is told by the live entry alone, so the
    /// cutoff stops just short of it, and that must not make the read look older than one that
    /// came a moment before.
    pub fn adopt_story_until(
        &mut self,
        story: ConnStory,
        read_at: DateTime<Utc>,
        covered_until: DateTime<Utc>,
    ) -> bool {
        if self.is_stale_read(read_at) {
            return false;
        }
        self.story = Some(story);
        self.advance_coverage(read_at, covered_until);
        true
    }

    /// Takes a read that found the transcript unchanged since the story held was read. That story
    /// is still the transcript's, but the event that triggered this read may have arrived after
    /// the last one, and its live entries are covered by the story only once the boundary has
    /// moved to it. A `Stop` hook that follows a read which already saw the closing message is the
    /// case: nothing was written, so nothing is parsed, and yet the hook's entry repeats the answer.
    ///
    /// Returns whether that changed what is in flight, which is what a reader would see change.
    /// A stale read, or a session with no story, changes nothing.
    pub fn recheck_story(&mut self, through: DateTime<Utc>) -> bool {
        self.recheck_story_until(through, through)
    }

    /// [`Self::recheck_story`] with the read's order and its coverage cutoff apart, as in
    /// [`Self::adopt_story_until`].
    pub fn recheck_story_until(
        &mut self,
        read_at: DateTime<Utc>,
        covered_until: DateTime<Utc>,
    ) -> bool {
        if self.story.is_none() || self.is_stale_read(read_at) {
            return false;
        }
        let before = self.in_flight().len();
        self.advance_coverage(read_at, covered_until);
        self.in_flight().len() != before
    }

    /// Strictly older, not equal: a finished turn is read twice at the same boundary,
    /// immediately and again once the transcript has had a moment to flush its closing message.
    /// The second read must be able to replace the first.
    fn is_stale_read(&self, through: DateTime<Utc>) -> bool {
        self.story_read_at.is_some_and(|current| current > through)
    }

    /// The transcript is written asynchronously, so a read can land before the turn's closing
    /// message reaches the file. Coverage is computed against the live entries in `in_flight`,
    /// where hook prompts are matched to transcript prompts rather than compared by timestamp.
    fn advance_coverage(&mut self, read_at: DateTime<Utc>, covered_until: DateTime<Utc>) {
        self.story_through = match self.story.as_ref().and_then(|story| story.turns.last()) {
            None => None,
            Some(turn) if turn.outcome.is_none() => Some(turn.last_activity()),
            Some(_) => Some(covered_until),
        };
        self.story_read_at = Some(read_at);
    }

    /// Live entries the story does not yet cover: the turn in flight.
    ///
    /// Transcript timestamps and hook receipt timestamps are different clocks
    /// for this purpose. Match prompts in order, allowing the hook's truncated
    /// text to be a prefix of the transcript prompt, and consume repeated
    /// prompts one at a time.
    pub fn in_flight(&self) -> &[ConnEntry] {
        let Some(through) = self.story_through else {
            return &self.entries;
        };
        let covered_end = self.entries.partition_point(|entry| entry.at <= through);
        let Some(story) = self.story.as_ref() else {
            return &self.entries;
        };
        let mut covered = vec![false; covered_end];
        let mut search_from = 0;
        let mut first_matched = None;
        // A failure is not in the transcript, so it stays in flight for as long as it belongs to
        // the latest turn. Once a later prompt exists it is history, and holding it back would
        // drag every covered entry after it back into the slice.
        let last_prompt = self
            .entries
            .iter()
            .rposition(|entry| matches!(entry.kind, ConnEntryKind::Prompt { .. }));

        for turn in &story.turns {
            // A resumed conversation, or one whose oldest hook prompts were evicted, starts with
            // transcript turns that have no live entries. Those say nothing about the later
            // turns, so they are skipped rather than ending the match.
            let Some(prompt_index) = self.entries[search_from..covered_end]
                .iter()
                .position(|entry| match &entry.kind {
                    ConnEntryKind::Prompt { text } => {
                        prompt_matches(text, &turn.prompt)
                            && arrived_with(entry.at, turn.started_at)
                    }
                    _ => false,
                })
                .map(|offset| search_from + offset)
            else {
                continue;
            };
            first_matched.get_or_insert(prompt_index);

            let next_prompt = self.entries[prompt_index + 1..covered_end]
                .iter()
                .position(|entry| matches!(entry.kind, ConnEntryKind::Prompt { .. }))
                .map(|offset| prompt_index + 1 + offset)
                .unwrap_or(covered_end);
            let end = if turn.outcome.is_some() {
                through
            } else {
                turn.last_activity().min(through)
            };
            for index in prompt_index..next_prompt {
                let entry = &self.entries[index];
                if entry.at <= end {
                    let latest_failure = matches!(entry.kind, ConnEntryKind::Failed { .. })
                        && last_prompt.is_none_or(|last| index > last);
                    covered[index] = !latest_failure;
                }
            }
            search_from = next_prompt;
            if search_from == covered_end {
                break;
            }
        }

        // What precedes the first matched prompt belongs to turns older than one the story
        // holds, so the story already accounts for it, or it has been evicted from the story's
        // reach altogether.
        if let Some(first) = first_matched {
            covered[..first].fill(true);
        }

        let first_uncovered = covered.iter().position(|is_covered| !is_covered);
        &self.entries[first_uncovered.unwrap_or(covered_end)..]
    }

    /// The time of the latest thing that happened, told by the story or still in flight.
    pub fn latest_activity(&self) -> Option<DateTime<Utc>> {
        let told = self
            .story
            .as_ref()
            .and_then(|story| story.latest_turn())
            .map(ConnTurn::last_activity);
        told.max(self.entries.last().map(|entry| entry.at))
    }

    /// Answered turns the reader has not seen. See [`seen::unseen_turns`].
    pub fn unseen_turns(&self) -> usize {
        self.story.as_ref().map_or(0, |story| {
            seen::unseen_turns(story, self.watermark, self.turn_finished())
        })
    }

    /// The first turn the reader has not seen the answer of.
    pub fn first_unseen_turn(&self) -> Option<&ConnTurn> {
        seen::first_unseen_turn(self.story.as_ref()?, self.watermark, self.turn_finished())
    }

    /// Whether the story's latest turn has finished, so that its last words are
    /// its answer.
    ///
    /// A prompt the story does not contain yet means the agent has moved on to
    /// a newer turn, however it is doing, so the story's latest turn is behind it.
    fn turn_finished(&self) -> bool {
        let newer_prompt = self
            .in_flight()
            .iter()
            .any(|entry| matches!(entry.kind, ConnEntryKind::Prompt { .. }));
        newer_prompt
            || match self.status {
                ConnStatus::Working | ConnStatus::NeedsYou => false,
                ConnStatus::Done | ConnStatus::Failed | ConnStatus::Ended => true,
            }
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

/// How long after a turn began its prompt may still reach us, in seconds. The hook fires as the
/// prompt is submitted, so a prompt that arrived long after a turn began is a different
/// instruction that happens to read the same, such as `continue` said again after a resume. The
/// allowance is wide because it only has to tell hours from moments, and a prompt it wrongly
/// refuses is merely shown live alongside the story rather than hidden by an older turn's answer.
const PROMPT_ARRIVAL_SLACK_SECONDS: i64 = 120;

/// Whether a live prompt received at `prompt_at` can be the prompt of a turn that began at
/// `turn_started_at`. The hook can be received a moment before the transcript records the prompt,
/// so only a prompt received long after the turn began is refused.
fn arrived_with(prompt_at: DateTime<Utc>, turn_started_at: DateTime<Utc>) -> bool {
    (prompt_at - turn_started_at).num_seconds() <= PROMPT_ARRIVAL_SLACK_SECONDS
}

fn prompt_matches(hook: &str, transcript: &str) -> bool {
    !hook.is_empty()
        && !transcript.is_empty()
        && (hook == transcript || transcript.starts_with(hook) || hook.starts_with(transcript))
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
