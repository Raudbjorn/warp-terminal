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

use ::conn::{ConnEntry, ConnEntryKind, ConnSession};
use chrono::Utc;
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
}

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

        let Some(kind) = entry_kind(event) else {
            return;
        };
        session.push(ConnEntry::new(kind, Utc::now()));
        ctx.notify();
    }
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
                text: query.to_owned(),
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
