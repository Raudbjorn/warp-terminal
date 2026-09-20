use ::conn::ConnEntryKind;
use warpui::{App, EntityId, SingletonEntity};

use super::ConnModel;
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::event::{
    CLIAgentEvent, CLIAgentEventPayload, CLIAgentEventSource, CLIAgentEventType,
};
use crate::terminal::cli_agent_sessions::{
    CLIAgentInputState, CLIAgentSession, CLIAgentSessionContext, CLIAgentSessionStatus,
    CLIAgentSessionsModel,
};

fn event(event: CLIAgentEventType, payload: CLIAgentEventPayload) -> CLIAgentEvent {
    CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event,
        session_id: Some("session-1".to_owned()),
        cwd: Some("/tmp/proj".to_owned()),
        project: Some("proj".to_owned()),
        payload,
    }
}

fn prompt(text: &str) -> CLIAgentEvent {
    event(
        CLIAgentEventType::PromptSubmit,
        CLIAgentEventPayload {
            query: Some(text.to_owned()),
            ..Default::default()
        },
    )
}

fn tracked_session() -> CLIAgentSession {
    CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        remote_host: None,
        draft_text: None,
        custom_command_prefix: None,
        received_rich_notification: true,
    }
}

/// Drives events through the real `CLIAgentSessionsModel` rather than calling
/// `ConnModel` directly, so the subscription wiring is covered too.
fn with_session<F>(assertions: F)
where
    F: FnOnce(&mut App, EntityId) + 'static,
{
    App::test((), |mut app| async move {
        let sessions = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        app.add_singleton_model(ConnModel::new);
        let view_id = EntityId::new();
        sessions.update(&mut app, |m, ctx| {
            m.set_session(view_id, tracked_session(), ctx);
        });
        assertions(&mut app, view_id);
    });
}

fn send(app: &mut App, view_id: EntityId, event: CLIAgentEvent) {
    CLIAgentSessionsModel::handle(&*app).update(app, |m, ctx| {
        m.update_from_event(view_id, &event, ctx);
    });
}

#[test]
fn records_the_prompt_spine_in_order() {
    with_session(|app, view_id| {
        send(app, view_id, prompt("first ask"));
        send(app, view_id, prompt("second ask"));

        let model = ConnModel::handle(&*app);
        let spine = model.read(app, |me, _| {
            me.session(view_id)
                .expect("session recorded")
                .prompts()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
        assert_eq!(spine, vec!["first ask", "second ask"]);
    });
}

/// The decision log is the thing Warp's own model erases, so it is the most
/// important thing for Conn to retain.
#[test]
fn retains_permission_requests_after_they_are_resolved() {
    with_session(|app, view_id| {
        send(app, view_id, prompt("refactor the parser"));
        send(
            app,
            view_id,
            event(
                CLIAgentEventType::PermissionRequest,
                CLIAgentEventPayload {
                    summary: Some("Remove node_modules".to_owned()),
                    tool_name: Some("Bash".to_owned()),
                    tool_input_preview: Some("rm -rf node_modules".to_owned()),
                    ..Default::default()
                },
            ),
        );
        send(
            app,
            view_id,
            event(CLIAgentEventType::PermissionReplied, Default::default()),
        );

        let model = ConnModel::handle(&*app);
        let kinds = model.read(app, |me, _| {
            me.session(view_id)
                .expect("session recorded")
                .entries()
                .iter()
                .map(|entry| entry.kind.clone())
                .collect::<Vec<_>>()
        });

        assert_eq!(
            kinds,
            vec![
                ConnEntryKind::Prompt {
                    text: "refactor the parser".to_owned()
                },
                ConnEntryKind::PermissionRequested {
                    summary: Some("Remove node_modules".to_owned()),
                    tool_name: Some("Bash".to_owned()),
                    target: Some("rm -rf node_modules".to_owned()),
                },
                ConnEntryKind::PermissionResolved,
            ],
            "the permission request must survive its own resolution"
        );
    });
}

#[test]
fn captures_session_identity_and_last_response() {
    with_session(|app, view_id| {
        send(app, view_id, prompt("write a haiku"));
        send(
            app,
            view_id,
            event(
                CLIAgentEventType::Stop,
                CLIAgentEventPayload {
                    response: Some("Memory is safe".to_owned()),
                    ..Default::default()
                },
            ),
        );

        let model = ConnModel::handle(&*app);
        let (project, cwd, session_id, last) = model.read(app, |me, _| {
            let session = me.session(view_id).expect("session recorded");
            (
                session.project.clone(),
                session.cwd.clone(),
                session.session_id.clone(),
                session.last_response().map(str::to_owned),
            )
        });

        assert_eq!(project.as_deref(), Some("proj"));
        assert_eq!(cwd.as_deref(), Some("/tmp/proj"));
        assert_eq!(session_id.as_deref(), Some("session-1"));
        assert_eq!(last.as_deref(), Some("Memory is safe"));
    });
}

/// Idle and unknown events carry no history; padding the log with them would
/// make the panel harder to read for no gain.
#[test]
fn ignores_events_that_carry_no_history() {
    with_session(|app, view_id| {
        send(
            app,
            view_id,
            event(CLIAgentEventType::IdlePrompt, Default::default()),
        );
        send(
            app,
            view_id,
            event(
                CLIAgentEventType::Unknown("from_a_newer_plugin".to_owned()),
                Default::default(),
            ),
        );

        let model = ConnModel::handle(&*app);
        let entries = model.read(app, |me, _| {
            me.session(view_id)
                .map(|session| session.entries().len())
                .unwrap_or(0)
        });
        assert_eq!(entries, 0, "no history entries for idle or unknown events");
    });
}

/// An empty prompt would render as a blank line in the spine, which reads as a
/// bug rather than as an empty instruction.
#[test]
fn skips_blank_prompts() {
    with_session(|app, view_id| {
        send(app, view_id, prompt("   "));

        let model = ConnModel::handle(&*app);
        let entries = model.read(app, |me, _| {
            me.session(view_id)
                .map(|session| session.entries().len())
                .unwrap_or(0)
        });
        assert_eq!(entries, 0);
    });
}
