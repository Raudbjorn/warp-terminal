//! Conn's in-process session history.
//!
//! [`ConnModel`] listens to [`CLIAgentSessionsModel`] and accumulates the
//! per-session history that model discards, keyed by the pane the session runs
//! in. It is strictly a reader: it holds no writable handle to a session, a
//! terminal or a PTY, and it sits downstream of the terminal parser, so by the
//! time an event arrives the agent's hook has already returned. Conn cannot
//! delay, alter or block a session.

pub mod panel;

use std::collections::HashMap;
use std::path::PathBuf;

use ::conn::story::{ConnStory, parse_transcript};
use ::conn::{ConnEntry, ConnEntryKind, ConnSession, tidy_prompt};
use chrono::{DateTime, TimeDelta, Utc};
use warpui::r#async::FutureId;
use warpui::{Entity, EntityId, ModelContext, SingletonEntity};

use crate::terminal::cli_agent_sessions::event::{CLIAgentEvent, CLIAgentEventType};
use crate::terminal::cli_agent_sessions::{CLIAgentSessionsModel, CLIAgentSessionsModelEvent};

/// Per-pane session history.
#[derive(Default)]
pub struct ConnModel {
    /// Keyed by the terminal view the session runs in. This is a
    /// process-lifetime id, so it is deliberately not a persistence key; a
    /// durable store should key on the terminal's session UUID instead.
    ///
    /// Entries are never removed. A session's history deliberately outlives
    /// the session, because reading it afterwards is the point, but nothing
    /// yet discards it when the *pane* is closed, so a long-lived window that
    /// churns panes will accumulate history for panes that no longer exist.
    /// Bounded per session by `conn::MAX_ENTRIES`, unbounded in pane count.
    sessions: HashMap<EntityId, ConnSession>,
    /// The transcript read currently in flight for each pane. Reading it is
    /// how tests await the read before asserting on the story.
    reads_in_flight: HashMap<EntityId, FutureId>,
    /// When a read was last dispatched for each pane, so a busy turn re-reads
    /// at a steady rate instead of once per tool call.
    last_read: HashMap<EntityId, DateTime<Utc>>,
}

/// How often the transcript is re-read while a turn is running.
///
/// A turn can run for many minutes, and that is exactly when someone walks
/// away and comes back, so the account of it cannot wait for the turn to
/// finish. Re-reading is a whole-file parse — tens of milliseconds on a
/// background thread — so it is paced rather than done per tool call.
const READ_INTERVAL: TimeDelta = TimeDelta::seconds(3);

impl Entity for ConnModel {
    type Event = ();
}

impl SingletonEntity for ConnModel {}

impl ConnModel {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let cli_sessions_model = CLIAgentSessionsModel::handle(ctx);
        ctx.subscribe_to_model(&cli_sessions_model, |me, _, event, ctx| {
            me.handle_cli_session_event(event, ctx);
        });

        Self::default()
    }

    // Consumed by the panel; nothing reads it until that lands.
    #[allow(dead_code)]
    pub fn session(&self, terminal_view_id: EntityId) -> Option<&ConnSession> {
        self.sessions.get(&terminal_view_id)
    }

    /// The transcript read in flight for a pane, if any. Tests await it before
    /// asserting on the story.
    #[allow(dead_code)]
    pub fn read_in_flight(&self, terminal_view_id: EntityId) -> Option<FutureId> {
        self.reads_in_flight.get(&terminal_view_id).copied()
    }

    fn handle_cli_session_event(
        &mut self,
        event: &CLIAgentSessionsModelEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        match event {
            CLIAgentSessionsModelEvent::RawEvent {
                terminal_view_id,
                event,
            } => self.record(*terminal_view_id, event, ctx),
            // A pane's history outlives the session that produced it: the
            // point of Conn is to still be readable after a session ends.
            // Only the pane going away discards it, which the workspace
            // reports separately.
            CLIAgentSessionsModelEvent::Started { .. }
            | CLIAgentSessionsModelEvent::StatusChanged { .. }
            | CLIAgentSessionsModelEvent::InputSessionChanged { .. }
            | CLIAgentSessionsModelEvent::Ended { .. }
            | CLIAgentSessionsModelEvent::SessionUpdated { .. } => {}
        }
    }

    fn record(
        &mut self,
        terminal_view_id: EntityId,
        event: &CLIAgentEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        let session = self.sessions.entry(terminal_view_id).or_default();

        // Identity and location can arrive on any event, and the plugin only
        // sets them when it knows them, so never overwrite a known value with
        // `None`.
        if event.session_id.is_some() {
            session.session_id = event.session_id.clone();
        }
        if event.cwd.is_some() {
            session.cwd = event.cwd.clone();
        }
        if event.project.is_some() {
            session.project = event.project.clone();
        }

        // The plugin only sends the path on `stop`, so it is kept once known.
        if let Some(path) = event.payload.transcript_path.as_deref() {
            session.transcript_path = Some(path.to_owned());
        }

        let at = Utc::now();
        if let Some(kind) = entry_kind(event) {
            session.push(ConnEntry::new(kind, at));
            ctx.notify();
        }

        let locator = TranscriptLocator {
            path: session.transcript_path.clone(),
            cwd: session.cwd.clone(),
            session_id: session.session_id.clone(),
        };
        if self.should_read(terminal_view_id, event, at) {
            self.read_transcript(terminal_view_id, locator, at, ctx);
        }
    }

    /// Whether this event should trigger a transcript read.
    ///
    /// A finished turn always reads: it is the turn boundary and the last
    /// chance to record the closing message. Anything else reads at most once
    /// per [`READ_INTERVAL`], and never while a read is already running,
    /// because a long turn emits events continuously.
    fn should_read(
        &self,
        terminal_view_id: EntityId,
        event: &CLIAgentEvent,
        now: DateTime<Utc>,
    ) -> bool {
        if matches!(
            event.event,
            CLIAgentEventType::Stop | CLIAgentEventType::StopFailure
        ) {
            return true;
        }
        if self.reads_in_flight.contains_key(&terminal_view_id) {
            return false;
        }
        self.last_read
            .get(&terminal_view_id)
            .is_none_or(|last| now - *last >= READ_INTERVAL)
    }

    /// Reads and parses the transcript off the main thread.
    ///
    /// The parse is tens of milliseconds on a long session and grows with it,
    /// so it must never run on the thread that draws frames. Locating the file
    /// touches the filesystem too, so that happens in the same place.
    fn read_transcript(
        &mut self,
        terminal_view_id: EntityId,
        locator: TranscriptLocator,
        through: DateTime<Utc>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.last_read.insert(terminal_view_id, through);
        let handle = ctx.spawn(read_story(locator), move |me, read, ctx| {
            me.reads_in_flight.remove(&terminal_view_id);
            // A transcript that cannot be found or read is not a failure: the
            // live hook entries still describe the session, just less well.
            let Some((path, story)) = read else {
                return;
            };
            let Some(session) = me.sessions.get_mut(&terminal_view_id) else {
                return;
            };
            // Remember where it was found so the next read skips the search.
            session.transcript_path = Some(path.to_string_lossy().into_owned());
            if session.adopt_story(story, through) {
                ctx.notify();
            }
        });
        self.reads_in_flight
            .insert(terminal_view_id, handle.future_id());
    }
}

/// What is known about where a session's transcript lives.
struct TranscriptLocator {
    /// A path the plugin reported, which is authoritative when present.
    path: Option<String>,
    cwd: Option<String>,
    session_id: Option<String>,
}

/// Runs on a background thread. Returns the file it read and the story in it.
async fn read_story(locator: TranscriptLocator) -> Option<(PathBuf, ConnStory)> {
    let path = locate_transcript(&locator).await?;
    let jsonl = tokio::fs::read_to_string(&path).await.ok()?;
    Some((path, parse_transcript(&jsonl)))
}

/// Finds a session's transcript.
///
/// The plugin reports the path on `stop` only, so the first turn of a session
/// — which can be the longest, and is the one most worth reading while it is
/// still running — would otherwise have no account at all until it ended.
/// Claude Code writes transcripts to a known place, so the path is derived
/// until the plugin confirms one.
async fn locate_transcript(locator: &TranscriptLocator) -> Option<PathBuf> {
    if let Some(path) = locator.path.as_deref() {
        let path = PathBuf::from(path);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Some(path);
        }
    }

    let session_id = locator.session_id.as_deref()?;
    let file_name = format!("{session_id}.jsonl");
    let projects = claude_projects_dir()?;

    if let Some(cwd) = locator.cwd.as_deref() {
        let derived = projects.join(mangle_project_dir(cwd)).join(&file_name);
        if tokio::fs::try_exists(&derived).await.unwrap_or(false) {
            return Some(derived);
        }
    }

    // The directory name is a guess; the session id is not. When the guess is
    // wrong, look for the file itself rather than give up on the session.
    let mut entries = tokio::fs::read_dir(&projects).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let candidate = entry.path().join(&file_name);
        if tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
            return Some(candidate);
        }
    }
    None
}

fn claude_projects_dir() -> Option<PathBuf> {
    let root = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::home_dir()?.join(".claude"),
    };
    Some(root.join("projects"))
}

/// Claude Code names a project directory after the working directory it ran
/// in, with the path separators flattened.
fn mangle_project_dir(cwd: &str) -> String {
    cwd.replace(['/', '.'], "-")
}

/// Translates a plugin event into a history entry, or `None` for events that
/// carry no history.
fn entry_kind(event: &CLIAgentEvent) -> Option<ConnEntryKind> {
    let payload = &event.payload;
    match &event.event {
        // A session beginning tells a returning reader nothing they can act
        // on, and it pushed the first real prompt down the panel.
        CLIAgentEventType::SessionStart => None,
        // A prompt with no text tells the reader nothing, and an empty line in
        // the spine is worse than no line.
        CLIAgentEventType::PromptSubmit => payload
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(|query| ConnEntryKind::Prompt {
                text: tidy_prompt(query),
            }),
        CLIAgentEventType::PermissionRequest => Some(ConnEntryKind::PermissionRequested {
            summary: payload.summary.clone(),
            tool_name: payload.tool_name.clone(),
            target: payload.tool_input_preview.clone(),
        }),
        CLIAgentEventType::PermissionReplied => Some(ConnEntryKind::PermissionResolved),
        CLIAgentEventType::QuestionAsked => Some(ConnEntryKind::QuestionAsked {
            summary: payload.summary.clone(),
        }),
        CLIAgentEventType::ToolComplete => Some(ConnEntryKind::ToolCompleted {
            tool_name: payload.tool_name.clone(),
            // Always `None` with the current Warp plugin, which sends no
            // `tool_input` for a completed call. Kept because a plugin we
            // control could fill it in.
            target: payload.tool_input_preview.clone(),
            runs: 1,
        }),
        CLIAgentEventType::Stop => Some(ConnEntryKind::Responded {
            text: payload.response.clone(),
        }),
        CLIAgentEventType::StopFailure => Some(ConnEntryKind::Failed {
            error_type: payload.error_type.clone(),
            message: payload.response.clone(),
        }),
        // The agent sitting at its prompt is already implied by the turn that
        // preceded it, so it would only pad the history.
        CLIAgentEventType::IdlePrompt => None,
        // A newer plugin than this build understands. Dropping it keeps
        // unknown events out of the history rather than guessing at them.
        CLIAgentEventType::Unknown(_) => None,
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
