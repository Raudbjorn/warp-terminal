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

/// Registers both models and one tracked session, so events can be driven
/// through the real `CLIAgentSessionsModel` and the subscription wiring is
/// covered rather than assumed.
fn setup(app: &mut App) -> EntityId {
    let sessions = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
    app.add_singleton_model(ConnModel::new);
    let view_id = EntityId::new();
    sessions.update(app, |m, ctx| {
        m.set_session(view_id, tracked_session(), ctx);
    });
    view_id
}

fn with_session<F>(assertions: F)
where
    F: FnOnce(&mut App, EntityId) + 'static,
{
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
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

fn stop(response: &str, transcript_path: Option<&str>) -> CLIAgentEvent {
    event(
        CLIAgentEventType::Stop,
        CLIAgentEventPayload {
            response: Some(response.to_owned()),
            transcript_path: transcript_path.map(str::to_owned),
            ..Default::default()
        },
    )
}

/// A two-turn transcript in the shape Claude Code writes.
fn transcript_jsonl() -> String {
    [
        r#"{"type":"user","timestamp":"2026-09-20T10:00:00Z","promptSource":"typed","message":{"content":"wire the ADRs in"}}"#,
        r#"{"type":"assistant","timestamp":"2026-09-20T10:00:05Z","message":{"content":[{"type":"tool_use","name":"Bash","input":{"description":"Explore the docs layout"}}]}}"#,
        r#"{"type":"assistant","timestamp":"2026-09-20T10:00:40Z","message":{"content":[{"type":"text","text":"ADRs were assumed but never wired in."}]}}"#,
        r#"{"type":"ai-title","aiTitle":"ADR wiring","sessionId":"session-1"}"#,
    ]
    .join("\n")
}

/// Waits for the transcript read the stop event dispatched.
async fn await_read(app: &mut App, view_id: EntityId) {
    let awaiting = ConnModel::handle(&*app).update(app, |me, ctx| {
        let future_id = me
            .read_in_flight(view_id)
            .expect("the stop event must have dispatched a transcript read");
        ctx.await_spawned_future(future_id)
    });
    awaiting.await;
}

/// The hook payload truncates the prompt at 200 characters and names tools
/// without saying what they did. The transcript has both in full, which is the
/// whole reason for reading it.
#[test]
fn a_finished_turn_is_read_from_the_transcript() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, transcript_jsonl()).expect("write transcript");

        send(&mut app, view_id, prompt("wire the ADRs in"));
        send(
            &mut app,
            view_id,
            stop("ADRs were assumed...", path.to_str()),
        );
        await_read(&mut app, view_id).await;

        let story = ConnModel::handle(&app)
            .read(&app, |me, _| {
                me.session(view_id)
                    .expect("session recorded")
                    .story()
                    .cloned()
            })
            .expect("story adopted");

        assert_eq!(story.title.as_deref(), Some("ADR wiring"));
        assert_eq!(story.turns.len(), 1);
        assert_eq!(story.turns[0].prompt, "wire the ADRs in");
        assert_eq!(
            story.turns[0].steps[0].description, "Explore the docs layout",
            "the step reads as the agent described it, not as a tool name"
        );
        assert_eq!(
            story.turns[0].outcome.as_deref(),
            Some("ADRs were assumed but never wired in.")
        );
    });
}

/// Once the story covers a turn, repeating that turn's hook entries below it
/// would show the reader the same thing twice.
#[test]
fn the_story_takes_over_from_the_live_entries() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, transcript_jsonl()).expect("write transcript");

        send(&mut app, view_id, prompt("wire the ADRs in"));
        send(
            &mut app,
            view_id,
            stop("ADRs were assumed...", path.to_str()),
        );
        await_read(&mut app, view_id).await;
        // A new turn starts, which only the hook events know about yet.
        send(&mut app, view_id, prompt("now write the tests"));

        let in_flight = ConnModel::handle(&app).read(&app, |me, _| {
            me.session(view_id)
                .expect("session recorded")
                .in_flight()
                .iter()
                .map(|entry| entry.kind.clone())
                .collect::<Vec<_>>()
        });

        assert_eq!(
            in_flight,
            vec![ConnEntryKind::Prompt {
                text: "now write the tests".to_owned()
            }],
            "the story covers the finished turn; only the new one is in flight"
        );
    });
}

/// A transcript that has been rotated or has not been written yet must leave
/// the hook-derived history in place rather than blanking the panel.
#[test]
fn a_missing_transcript_leaves_the_live_history_alone() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);

        send(&mut app, view_id, prompt("do the thing"));
        send(
            &mut app,
            view_id,
            stop("done", Some("/nonexistent/session.jsonl")),
        );
        await_read(&mut app, view_id).await;

        let (story, entries) = ConnModel::handle(&app).read(&app, |me, _| {
            let session = me.session(view_id).expect("session recorded");
            (session.story().is_some(), session.entries().len())
        });

        assert!(!story, "no story from a file that cannot be read");
        assert_eq!(entries, 2, "the prompt and the response are still there");
    });
}

/// The plugin sends the path on the stop event only, so it has to be
/// remembered to survive the events that do not carry it.
#[test]
fn the_transcript_path_is_remembered_across_events() {
    with_session(|app, view_id| {
        send(app, view_id, stop("done", Some("/tmp/session.jsonl")));
        send(app, view_id, prompt("next"));

        let path = ConnModel::handle(&*app).read(app, |me, _| {
            me.session(view_id)
                .expect("session recorded")
                .transcript_path
                .clone()
        });
        assert_eq!(path.as_deref(), Some("/tmp/session.jsonl"));
    });
}

fn tool_complete(name: &str) -> CLIAgentEvent {
    event(
        CLIAgentEventType::ToolComplete,
        CLIAgentEventPayload {
            tool_name: Some(name.to_owned()),
            ..Default::default()
        },
    )
}

/// A long turn is exactly when someone walks away and comes back, so the
/// account of it cannot wait for the turn to end. The transcript is read while
/// the turn is still running.
#[test]
fn a_turn_in_flight_is_read_before_it_finishes() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, transcript_jsonl()).expect("write transcript");

        // Only `stop` carries the path, and no stop has happened yet.
        send(
            &mut app,
            view_id,
            event(
                CLIAgentEventType::PromptSubmit,
                CLIAgentEventPayload {
                    query: Some("wire the ADRs in".to_owned()),
                    transcript_path: path.to_str().map(str::to_owned),
                    ..Default::default()
                },
            ),
        );
        await_read(&mut app, view_id).await;

        let turns = ConnModel::handle(&app).read(&app, |me, _| {
            me.session(view_id)
                .and_then(|session| session.story())
                .map(|story| story.turns.len())
        });
        assert_eq!(turns, Some(1), "the story is told while the turn runs");
    });
}

/// A busy turn emits events continuously. Reading the whole transcript for
/// each one would spend the background executor on the same file over and
/// over.
#[test]
fn a_burst_of_tool_calls_does_not_read_once_per_call() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);

        send(&mut app, view_id, prompt("do a lot of work"));
        let first = ConnModel::handle(&app)
            .read(&app, |me, _| me.read_in_flight(view_id))
            .expect("the first event reads");

        for _ in 0..20 {
            send(&mut app, view_id, tool_complete("Bash"));
        }

        let still = ConnModel::handle(&app).read(&app, |me, _| me.read_in_flight(view_id));
        assert_eq!(
            still,
            Some(first),
            "twenty tool calls inside the interval are still the one read"
        );
    });
}

/// The stop event is the turn boundary and the last chance to record the
/// closing message, so it reads whatever the pacing says.
#[test]
fn a_finished_turn_always_reads() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, transcript_jsonl()).expect("write transcript");

        send(&mut app, view_id, prompt("wire the ADRs in"));
        let first = ConnModel::handle(&app)
            .read(&app, |me, _| me.read_in_flight(view_id))
            .expect("the first event reads");
        send(
            &mut app,
            view_id,
            stop("ADRs were assumed...", path.to_str()),
        );

        let after_stop = ConnModel::handle(&app).read(&app, |me, _| me.read_in_flight(view_id));
        assert_ne!(
            after_stop,
            Some(first),
            "the stop event dispatches its own read"
        );
    });
}

/// Pasted text arrives wrapped in a tag meant for the agent, sitting exactly
/// where the instruction should be.
#[test]
fn a_pasted_prompt_is_recorded_without_its_wrapper() {
    with_session(|app, view_id| {
        send(
            app,
            view_id,
            prompt("<pasted_content id=\"0e44\">\ncombine the two repos\n</pasted_content>"),
        );

        let spine = ConnModel::handle(&*app).read(app, |me, _| {
            me.session(view_id)
                .expect("session recorded")
                .prompts()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
        assert_eq!(spine, vec!["combine the two repos"]);
    });
}
