use std::sync::{Arc, Mutex};
use std::time::Duration;

use ::conn::{ConnEntryKind, ConnStatus};
use chrono::Utc;
use warp_core::execution_mode::{AppExecutionMode, ExecutionMode};
use warp_util::git::run_git_command;
use warpui::r#async::Timer;
use warpui::{App, EntityId, SingletonEntity};

use super::{ConnModel, ConnModelEvent, MAX_TRANSCRIPT_BYTES, conn_status};
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::event::{
    CLIAgentEvent, CLIAgentEventPayload, CLIAgentEventSource, CLIAgentEventType,
};
use crate::terminal::cli_agent_sessions::{
    BlockedSource, CLIAgentInputState, CLIAgentSession, CLIAgentSessionContext,
    CLIAgentSessionStatus, CLIAgentSessionsModel,
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

/// Captures the panes the model announces, so a test can see what the panel would be told.
fn capture_announced_panes(app: &mut App) -> Arc<Mutex<Vec<EntityId>>> {
    let announced = Arc::new(Mutex::new(Vec::new()));
    let captured = announced.clone();
    app.update(|ctx| {
        let model = ConnModel::handle(ctx);
        ctx.subscribe_to_model(&model, move |_, event, _| {
            if let ConnModelEvent::SessionUpdated { terminal_view_id } = event {
                captured
                    .lock()
                    .expect("the capture is not poisoned")
                    .push(*terminal_view_id);
            }
        });
    });
    announced
}

/// The panes the model has announced so far.
///
/// An announcement is an effect, so it reaches the subscribers when the app next updates.
fn announced_panes(app: &mut App, capture: &Arc<Mutex<Vec<EntityId>>>) -> Vec<EntityId> {
    app.update(|_| {});
    capture.lock().expect("the capture is not poisoned").clone()
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

/// A background-tasks report is the row's status (work still running after a turn), not
/// something the agent said or did, so it adds no history line.
#[test]
fn background_tasks_report_adds_no_history() {
    with_session(|app, view_id| {
        send(
            app,
            view_id,
            event(
                CLIAgentEventType::BackgroundTasks {
                    count: 2,
                    only_subagents: false,
                    shells: 1,
                    started_ms: Some(1),
                },
                Default::default(),
            ),
        );

        let model = ConnModel::handle(&*app);
        let entries = model.read(app, |me, _| {
            me.session(view_id)
                .map(|session| session.entries().len())
                .unwrap_or(0)
        });
        assert_eq!(entries, 0, "a background-tasks report is not history");
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

/// A prompt that was only a pasted-content tag is empty once the tag is stripped, and an empty
/// line in the spine is exactly what the blank-prompt filter is there to keep out.
#[test]
fn skips_prompts_that_are_empty_once_tidied() {
    with_session(|app, view_id| {
        send(app, view_id, prompt("<pasted_content id=\"0e44\">"));

        let entries = ConnModel::handle(&*app).read(app, |me, _| {
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

/// [`transcript_jsonl`] with its timestamps near now. A hook entry is stamped on receipt, so a
/// transcript that is meant to hold the same turn has to have been written around the same time.
fn transcript_jsonl_now() -> String {
    let started = Utc::now() - chrono::Duration::seconds(1);
    let at = |seconds: i64| (started + chrono::Duration::seconds(seconds)).to_rfc3339();
    [
        format!(
            r#"{{"type":"user","timestamp":"{}","promptSource":"typed","message":{{"content":"wire the ADRs in"}}}}"#,
            at(0)
        ),
        format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"content":[{{"type":"tool_use","name":"Bash","input":{{"description":"Explore the docs layout"}}}}]}}}}"#,
            at(0)
        ),
        format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"content":[{{"type":"text","text":"ADRs were assumed but never wired in."}}]}}}}"#,
            at(0)
        ),
        r#"{"type":"ai-title","aiTitle":"ADR wiring","sessionId":"session-1"}"#.to_owned(),
    ]
    .join("\n")
}

/// Waits for the lookup of a working directory's project, if one is still running.
async fn await_project_resolution(app: &mut App, cwd: &str) {
    let awaiting = ConnModel::handle(&*app).update(app, |me, ctx| {
        me.project_resolution_in_flight(cwd)
            .map(|future_id| ctx.await_spawned_future(future_id))
    });
    if let Some(awaiting) = awaiting {
        awaiting.await;
    }
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
        std::fs::write(&path, transcript_jsonl_now()).expect("write transcript");

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

#[test]
fn a_closed_pane_is_forgotten() {
    with_session(|app, view_id| {
        send(app, view_id, prompt("combine the two repos"));

        ConnModel::handle(&*app).update(app, |me, ctx| {
            me.forget_pane(view_id, ctx);
        });

        let remembered = ConnModel::handle(&*app).read(app, |me, _| me.session(view_id).is_some());
        assert!(
            !remembered,
            "history for a pane that no longer exists is unreachable and unbounded"
        );
    });
}

/// The agent exiting is not the pane closing, and Warp removes its
/// `CLIAgentSession` at both moments. A session that has just finished is the
/// one most worth reading, and its pane is still there to read it in.
#[test]
fn an_agent_that_exits_leaves_its_history_behind() {
    with_session(|app, view_id| {
        send(app, view_id, prompt("combine the two repos"));

        CLIAgentSessionsModel::handle(&*app).update(app, |m, ctx| {
            m.remove_session(view_id, ctx);
        });

        let spine = ConnModel::handle(&*app).read(app, |me, _| {
            me.session(view_id)
                .expect("history survives the agent")
                .prompts()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
        assert_eq!(spine, vec!["combine the two repos"]);
    });
}

/// A session waiting on an answer sends no further events, so the request is the last chance to
/// read what led up to it. The request itself must stay visible beside that story.
#[test]
fn a_permission_request_reads_the_story_and_stays_in_flight() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, transcript_jsonl()).expect("write transcript");

        send(
            &mut app,
            view_id,
            event(
                CLIAgentEventType::PermissionRequest,
                CLIAgentEventPayload {
                    summary: Some("Wants to run ExitPlanMode".to_owned()),
                    transcript_path: path.to_str().map(str::to_owned),
                    ..Default::default()
                },
            ),
        );
        await_read(&mut app, view_id).await;

        ConnModel::handle(&app).read(&app, |me, _| {
            let session = me.session(view_id).expect("session");
            assert!(
                session
                    .story()
                    .and_then(|story| story.latest_turn())
                    .is_some_and(|turn| turn.outcome.is_some()),
                "the story is read up to the request"
            );
            assert!(
                matches!(
                    session
                        .in_flight()
                        .iter()
                        .map(|entry| &entry.kind)
                        .collect::<Vec<_>>()
                        .as_slice(),
                    [ConnEntryKind::PermissionRequested { .. }]
                ),
                "the request is still shown"
            );
        });
    });
}

#[test]
fn warp_statuses_collapse_into_what_the_reader_acts_on() {
    let cases = [
        (CLIAgentSessionStatus::InProgress, ConnStatus::Working),
        (
            CLIAgentSessionStatus::Blocked {
                message: None,
                source: BlockedSource::PermissionRequest,
            },
            ConnStatus::NeedsYou,
        ),
        (CLIAgentSessionStatus::Success, ConnStatus::Done),
        (CLIAgentSessionStatus::Cancelled, ConnStatus::Done),
        (
            CLIAgentSessionStatus::Failed {
                error_type: None,
                message: None,
            },
            ConnStatus::Failed,
        ),
    ];
    for (warp, expected) in cases {
        assert_eq!(conn_status(&warp), expected, "{warp:?}");
    }
}

fn status(app: &App, view_id: EntityId) -> Option<ConnStatus> {
    ConnModel::handle(app).read(app, |me, _| {
        me.session(view_id).map(|session| session.status)
    })
}

#[test]
fn a_permission_request_needs_you() {
    with_session(|app, view_id| {
        send(app, view_id, prompt("delete the build dir"));
        send(
            app,
            view_id,
            event(
                CLIAgentEventType::PermissionRequest,
                CLIAgentEventPayload::default(),
            ),
        );

        assert_eq!(status(app, view_id), Some(ConnStatus::NeedsYou));
    });
}

#[test]
fn an_agent_that_exits_has_ended() {
    with_session(|app, view_id| {
        send(app, view_id, prompt("combine the two repos"));

        CLIAgentSessionsModel::handle(&*app).update(app, |m, ctx| {
            m.remove_session(view_id, ctx);
        });

        assert_eq!(status(app, view_id), Some(ConnStatus::Ended));
    });
}

/// A started or resumed agent sits at its prompt until it is asked something. Warp reports that as
/// in progress, which would show a session waiting on the reader as busy.
#[test]
fn a_new_agent_in_the_pane_waits_as_done() {
    with_session(|app, view_id| {
        send(app, view_id, prompt("combine the two repos"));
        CLIAgentSessionsModel::handle(&*app).update(app, |m, ctx| {
            m.remove_session(view_id, ctx);
            m.set_session(view_id, tracked_session(), ctx);
        });

        assert_eq!(status(app, view_id), Some(ConnStatus::Done));
    });
}

#[test]
fn a_prompt_to_a_new_agent_is_working() {
    with_session(|app, view_id| {
        send(app, view_id, prompt("combine the two repos"));

        assert_eq!(status(app, view_id), Some(ConnStatus::Working));
    });
}

#[test]
fn a_session_in_a_subdirectory_is_named_after_its_repository() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("checkout");
        let subdir = root.join("app");
        std::fs::create_dir_all(&subdir).expect("mkdir");
        run_git_command(&root, &["init"]).await.expect("git init");
        let cwd = subdir.to_str().expect("utf-8 path").to_owned();

        let mut submitted = prompt("tidy the app crate");
        submitted.cwd = Some(cwd.clone());
        submitted.project = Some("app".to_owned());
        send(&mut app, view_id, submitted);
        let resolving = ConnModel::handle(&app).update(&mut app, |me, ctx| {
            let future_id = me
                .project_resolution_in_flight(&cwd)
                .expect("a new working directory is resolved to its project");
            ctx.await_spawned_future(future_id)
        });
        resolving.await;

        let project = ConnModel::handle(&app).read(&app, |me, _| {
            me.session(view_id)
                .and_then(|session| session.project.clone())
        });
        // The session keeps the repository's grouping key, so two checkouts that share a
        // directory name stay apart. The overview turns it into a name.
        let project = project.expect("the project was resolved");
        assert!(std::path::Path::new(&project).is_absolute(), "{project}");
        assert_eq!(::conn::overview::project_label(&project), "checkout");
    });
}

#[test]
fn a_finished_turn_is_done() {
    with_session(|app, view_id| {
        send(app, view_id, prompt("rename the crate"));
        send(app, view_id, stop("Renamed.", None));

        assert_eq!(status(app, view_id), Some(ConnStatus::Done));
    });
}

/// A prompt that also reports where the transcript is, which dispatches a single read.
fn prompt_with_transcript(text: &str, path: &std::path::Path) -> CLIAgentEvent {
    event(
        CLIAgentEventType::PromptSubmit,
        CLIAgentEventPayload {
            query: Some(text.to_owned()),
            transcript_path: path.to_str().map(str::to_owned),
            ..Default::default()
        },
    )
}

fn from_session(mut event: CLIAgentEvent, session_id: &str) -> CLIAgentEvent {
    event.session_id = Some(session_id.to_owned());
    event
}

/// A pane keeps its history after its agent exits, so a different agent started there must not
/// be shown the previous conversation as its own.
#[test]
fn a_replacement_agent_does_not_inherit_the_previous_transcript() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, transcript_jsonl()).expect("write transcript");

        send(
            &mut app,
            view_id,
            prompt_with_transcript("wire the ADRs in", &path),
        );
        await_read(&mut app, view_id).await;
        let watermark = Utc::now();
        ConnModel::handle(&app).update(&mut app, |me, _| {
            me.sessions
                .get_mut(&view_id)
                .expect("session recorded")
                .watermark = Some(watermark);
        });

        send(
            &mut app,
            view_id,
            from_session(prompt("a different conversation"), "session-2"),
        );

        ConnModel::handle(&app).read(&app, |me, _| {
            let session = me.session(view_id).expect("session recorded");
            assert_eq!(session.session_id.as_deref(), Some("session-2"));
            assert!(
                session.story().is_none(),
                "the old story is not the new one's"
            );
            assert_eq!(
                session.transcript_path, None,
                "the old transcript is not the new one's"
            );
            assert_eq!(
                session.prompts().collect::<Vec<_>>(),
                vec!["a different conversation"]
            );
            assert_eq!(
                session.watermark,
                Some(watermark),
                "what the reader has seen belongs to the pane"
            );
        });
    });
}

/// The same session id is the same conversation resumed, which carries on.
#[test]
fn the_same_session_keeps_its_history() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, transcript_jsonl()).expect("write transcript");

        send(
            &mut app,
            view_id,
            prompt_with_transcript("wire the ADRs in", &path),
        );
        await_read(&mut app, view_id).await;
        send(&mut app, view_id, prompt("now write the tests"));

        ConnModel::handle(&app).read(&app, |me, _| {
            let session = me.session(view_id).expect("session recorded");
            assert!(session.story().is_some());
            assert!(session.transcript_path.is_some());
            assert_eq!(session.prompts().count(), 2);
        });
    });
}

/// A read dispatched for the previous agent can land after the new one has started. What it read
/// is the previous agent's story.
#[test]
fn a_read_for_a_replaced_agent_is_not_adopted() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, transcript_jsonl()).expect("write transcript");

        send(
            &mut app,
            view_id,
            prompt_with_transcript("wire the ADRs in", &path),
        );
        let stale = ConnModel::handle(&app)
            .read(&app, |me, _| me.read_in_flight(view_id))
            .expect("the prompt reads");
        send(
            &mut app,
            view_id,
            from_session(prompt("a different conversation"), "session-2"),
        );

        let awaiting =
            ConnModel::handle(&app).update(&mut app, |_, ctx| ctx.await_spawned_future(stale));
        awaiting.await;

        ConnModel::handle(&app).read(&app, |me, _| {
            let session = me.session(view_id).expect("session recorded");
            assert!(session.story().is_none(), "the stale read was dropped");
            assert_eq!(session.transcript_path, None);
        });
    });
}

/// A finished turn reads twice, at once and again after a grace period. The first landing must not
/// make the pane look as though the second were not coming.
#[test]
fn the_first_read_to_land_does_not_clear_the_one_still_coming() {
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
        let delayed = ConnModel::handle(&app)
            .read(&app, |me, _| me.read_in_flight(view_id))
            .expect("the stop event reads");

        // Long enough for the immediate read to land, far short of the grace period.
        Timer::after(Duration::from_millis(300)).await;

        // Whether or not the immediate read has landed by now, the one tracked is the read still
        // to come, so this holds on a slow machine too and is not gated on how fast this one is.
        let in_flight = ConnModel::handle(&app).read(&app, |me, _| me.read_in_flight(view_id));
        assert_eq!(
            in_flight,
            Some(delayed),
            "the delayed read is still tracked while the grace period runs"
        );
        let awaiting =
            ConnModel::handle(&app).update(&mut app, |_, ctx| ctx.await_spawned_future(delayed));
        awaiting.await;
        let in_flight = ConnModel::handle(&app).read(&app, |me, _| me.read_in_flight(view_id));
        assert_eq!(in_flight, None, "the last read to land clears the pane");
    });
}

/// Reading a path that is not a regular file must neither hang nor grow without bound, and must
/// leave the live history in place.
async fn assert_unreadable_transcript_is_ignored(
    app: &mut App,
    view_id: EntityId,
    path: &std::path::Path,
) {
    send(app, view_id, prompt_with_transcript("do the thing", path));
    await_read(app, view_id).await;

    ConnModel::handle(&*app).read(app, |me, _| {
        let session = me.session(view_id).expect("session recorded");
        assert!(session.story().is_none());
        assert_eq!(session.entries().len(), 1, "the live prompt is still there");
    });
}

#[test]
fn a_directory_is_not_read_as_a_transcript() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        assert_unreadable_transcript_is_ignored(&mut app, view_id, dir.path()).await;
    });
}

/// `/dev/zero` exists and never ends, which is what a terminal naming it would be counting on.
#[cfg(unix)]
#[test]
fn an_endless_device_is_not_read_as_a_transcript() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        assert_unreadable_transcript_is_ignored(
            &mut app,
            view_id,
            std::path::Path::new("/dev/zero"),
        )
        .await;
    });
}

#[test]
fn a_transcript_over_the_size_limit_is_not_read() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        // Sparse, so this costs no disk and no time.
        let file = std::fs::File::create(&path).expect("create transcript");
        file.set_len(MAX_TRANSCRIPT_BYTES + 1).expect("extend");
        drop(file);

        assert_unreadable_transcript_is_ignored(&mut app, view_id, &path).await;
    });
}

/// Two transcripts of the same session, one with a turn the other has not.
fn transcript_with_a_second_turn() -> String {
    let mut jsonl = transcript_jsonl();
    jsonl.push('\n');
    jsonl.push_str(
        r#"{"type":"user","timestamp":"2026-09-20T10:05:00Z","promptSource":"typed","message":{"content":"now the tests"}}"#,
    );
    jsonl.push('\n');
    jsonl.push_str(
        r#"{"type":"assistant","timestamp":"2026-09-20T10:05:05Z","message":{"content":[{"type":"text","text":"And the tests."}]}}"#,
    );
    jsonl
}

/// The two reads of a finished turn share a boundary, so the dispatch order is what tells
/// them apart. A read that lands after a later one was adopted is stale, and must not put
/// the older story back.
#[test]
fn an_earlier_read_does_not_replace_a_later_story() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let older = dir.path().join("older.jsonl");
        let newer = dir.path().join("newer.jsonl");
        std::fs::write(&older, transcript_jsonl()).expect("write transcript");
        std::fs::write(
            &newer,
            transcript_jsonl().replace("ADR wiring", "ADR wiring, revisited"),
        )
        .expect("write transcript");

        // The first read is dispatched first and lands last, having seen less of the
        // transcript than the second.
        let through = Utc::now();
        let stale = ConnModel::handle(&app).update(&mut app, |me, ctx| {
            me.dispatch_read(
                view_id,
                older.to_str().expect("utf-8 path"),
                through,
                Some(Duration::from_millis(200)),
                ctx,
            )
        });
        let fresh = ConnModel::handle(&app).update(&mut app, |me, ctx| {
            me.dispatch_read(
                view_id,
                newer.to_str().expect("utf-8 path"),
                through,
                None,
                ctx,
            )
        });

        // The later read lands first.
        let awaiting =
            ConnModel::handle(&app).update(&mut app, |_, ctx| ctx.await_spawned_future(fresh));
        awaiting.await;
        // The earlier read lands after it, and must not put its story back.
        let awaiting =
            ConnModel::handle(&app).update(&mut app, |_, ctx| ctx.await_spawned_future(stale));
        awaiting.await;

        let (title, path) = ConnModel::handle(&app).read(&app, |me, _| {
            let session = me.session(view_id).expect("session recorded");
            (
                session.story().and_then(|story| story.title.clone()),
                session.transcript_path.clone(),
            )
        });
        assert_eq!(
            title.as_deref(),
            Some("ADR wiring, revisited"),
            "the story of the read that landed last is the one that is kept"
        );
        assert_eq!(path.as_deref(), newer.to_str());
    });
}

/// The read dispatched later is the one that saw more of the transcript, so it replaces the
/// earlier one even when it lands after it.
#[test]
fn a_later_read_replaces_an_earlier_story() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let older = dir.path().join("older.jsonl");
        let newer = dir.path().join("newer.jsonl");
        std::fs::write(&older, transcript_jsonl()).expect("write transcript");
        std::fs::write(
            &newer,
            transcript_jsonl().replace("ADR wiring", "ADR wiring, revisited"),
        )
        .expect("write transcript");

        let through = Utc::now();
        let early = ConnModel::handle(&app).update(&mut app, |me, ctx| {
            me.dispatch_read(
                view_id,
                older.to_str().expect("utf-8 path"),
                through,
                None,
                ctx,
            )
        });
        let late = ConnModel::handle(&app).update(&mut app, |me, ctx| {
            me.dispatch_read(
                view_id,
                newer.to_str().expect("utf-8 path"),
                through,
                Some(Duration::from_millis(200)),
                ctx,
            )
        });

        let awaiting =
            ConnModel::handle(&app).update(&mut app, |_, ctx| ctx.await_spawned_future(early));
        awaiting.await;
        let awaiting =
            ConnModel::handle(&app).update(&mut app, |_, ctx| ctx.await_spawned_future(late));
        awaiting.await;

        let title = ConnModel::handle(&app).read(&app, |me, _| {
            me.session(view_id)
                .and_then(|session| session.story())
                .and_then(|story| story.title.clone())
        });
        assert_eq!(
            title.as_deref(),
            Some("ADR wiring, revisited"),
            "the read that saw more of the transcript is the one that is kept"
        );
    });
}

/// A turn can run for minutes without another hook event, and the transcript keeps growing
/// meanwhile, so it is re-read on a timer while it runs.
#[test]
fn a_running_turn_is_re_read() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, transcript_jsonl()).expect("write transcript");

        // The prompt starts the turn and names the transcript.
        send(
            &mut app,
            view_id,
            prompt_with_transcript("wire the ADRs in", &path),
        );
        await_read(&mut app, view_id).await;

        // The agent writes more of the transcript without sending an event.
        std::fs::write(&path, transcript_with_a_second_turn()).expect("write transcript");

        // The paced read picks the turn up.
        let mut turns = None;
        for _ in 0..3 {
            let paced = ConnModel::handle(&app)
                .read(&app, |me, _| me.paced_read_in_flight(view_id))
                .expect("a running turn schedules a paced read");
            let awaiting =
                ConnModel::handle(&app).update(&mut app, |_, ctx| ctx.await_spawned_future(paced));
            awaiting.await;
            // The read the timer dispatched may have landed already.
            let read = ConnModel::handle(&app).read(&app, |me, _| me.read_in_flight(view_id));
            if let Some(read) = read {
                let awaiting = ConnModel::handle(&app)
                    .update(&mut app, |_, ctx| ctx.await_spawned_future(read));
                awaiting.await;
            }
            turns = ConnModel::handle(&app).read(&app, |me, _| {
                me.session(view_id)
                    .and_then(|session| session.story())
                    .map(|story| story.turns.len())
            });
            if turns == Some(2) {
                break;
            }
        }
        assert_eq!(
            turns,
            Some(2),
            "the turn that ran while the agent worked is told"
        );
    });
}

/// A burst of events must not leave a whole-transcript read pending for each of them, and the one
/// that is pending must be for the latest boundary: an older one would be dropped as stale once
/// the newer boundary's immediate read landed, and nothing would be left to pick up the answer
/// the transcript flushed after it.
#[test]
fn a_burst_of_events_keeps_one_delayed_read_for_the_latest_boundary() {
    with_session(|app, view_id| {
        send(app, view_id, stop("done", Some("/tmp/session.jsonl")));
        let first = ConnModel::handle(&*app).read(app, |me, _| {
            me.delayed_reads.get(&view_id).map(|pending| pending.seq)
        });
        assert!(first.is_some(), "a finished turn schedules a delayed read");

        send(app, view_id, stop("done again", Some("/tmp/session.jsonl")));
        let (pending, newest, count) = ConnModel::handle(&*app).read(app, |me, _| {
            (
                me.delayed_reads.get(&view_id).map(|pending| pending.seq),
                me.next_read_seq - 1,
                me.delayed_reads.len(),
            )
        });

        assert_eq!(count, 1, "one delayed read is pending per pane");
        assert_eq!(
            pending,
            Some(newest),
            "the pending read is the newest dispatched, not the one for the older boundary"
        );
        assert_ne!(pending, first, "the older boundary's read was replaced");
    });
}

/// An agent that needs input is waiting on the reader, and the panel has to say so beside
/// its own "asked you" entry.
#[test]
fn a_session_that_needs_input_needs_you() {
    App::test((), |mut app| async move {
        // An interactive run, where the plugin reports no status change for the request.
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::App, false, ctx));
        let view_id = setup(&mut app);

        send(&mut app, view_id, prompt("do the thing"));
        send(
            &mut app,
            view_id,
            event(
                CLIAgentEventType::NeedsInput,
                CLIAgentEventPayload::default(),
            ),
        );

        assert_eq!(
            status(&app, view_id),
            Some(ConnStatus::NeedsYou),
            "the panel says the reader is needed"
        );

        // The agent works again, and the session says so.
        send(&mut app, view_id, tool_complete("Bash"));
        assert_eq!(status(&app, view_id), Some(ConnStatus::Working));
    });
}

/// The overview groups sessions by project, so a session that changes project has to be
/// announced even when the event that changed it carries no history of its own.
#[test]
fn a_changed_project_is_announced() {
    with_session(|app, view_id| {
        let announced = capture_announced_panes(app);

        // A session start names the project and carries no history of its own.
        send(
            app,
            view_id,
            event(
                CLIAgentEventType::SessionStart,
                CLIAgentEventPayload::default(),
            ),
        );

        assert_eq!(
            announced_panes(app, &announced),
            vec![view_id],
            "the panel is told which project the session belongs to"
        );
    });
}

/// A transcript that has not grown since the last read has nothing new to say, so it is not
/// read and parsed again.
#[test]
fn an_unchanged_transcript_is_not_read_again() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, transcript_jsonl()).expect("write transcript");

        let announced = capture_announced_panes(&mut app);
        send(
            &mut app,
            view_id,
            prompt_with_transcript("wire the ADRs in", &path),
        );
        await_read(&mut app, view_id).await;
        // The session's working directory is resolved to its project in the background, and that
        // announces too, whenever it lands. It has to be in before counting, or it is counted as
        // though the second read had caused it.
        await_project_resolution(&mut app, "/tmp/proj").await;
        let first = announced_panes(&mut app, &announced).len();

        // The same file again: it has not grown, so there is nothing new in it.
        let read = ConnModel::handle(&app).update(&mut app, |me, ctx| {
            me.dispatch_read(
                view_id,
                path.to_str().expect("utf-8 path"),
                Utc::now(),
                None,
                ctx,
            )
        });
        let awaiting =
            ConnModel::handle(&app).update(&mut app, |_, ctx| ctx.await_spawned_future(read));
        awaiting.await;

        assert_eq!(
            announced_panes(&mut app, &announced).len(),
            first,
            "a transcript that has not grown is not read again"
        );
    });
}

/// A read that has already seen the closing message leaves the `Stop` hook's own entry after the
/// story's boundary, and the reads the hook triggers find the file as long as it was. They parse
/// nothing, but must still move the boundary, or the answer is shown twice.
#[test]
fn a_stop_after_the_read_that_saw_the_answer_does_not_show_it_twice() {
    App::test((), |mut app| async move {
        let view_id = setup(&mut app);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, transcript_jsonl_now()).expect("write transcript");

        send(
            &mut app,
            view_id,
            prompt_with_transcript("wire the ADRs in", &path),
        );
        // The re-read while the turn is working gets there first, and finds the answer written.
        let read = ConnModel::handle(&app).update(&mut app, |me, ctx| {
            me.dispatch_read(
                view_id,
                path.to_str().expect("utf-8 path"),
                Utc::now(),
                None,
                ctx,
            )
        });
        let awaiting =
            ConnModel::handle(&app).update(&mut app, |_, ctx| ctx.await_spawned_future(read));
        awaiting.await;

        send(
            &mut app,
            view_id,
            stop("ADRs were assumed...", path.to_str()),
        );
        await_read(&mut app, view_id).await;

        let in_flight = ConnModel::handle(&app).read(&app, |me, _| {
            me.session(view_id)
                .expect("session recorded")
                .in_flight()
                .len()
        });
        assert_eq!(in_flight, 0, "the story already holds the answer");
    });
}

// The accepted paths are Unix spellings; an absolute path on Windows needs a drive, and the
// Windows test below covers one.
#[cfg(unix)]
#[test]
fn only_an_absolute_local_jsonl_path_is_taken_for_a_transcript() {
    use std::path::Path;

    for path in [
        "/home/me/.claude/projects/-home-me-app/0f3c-1.jsonl",
        // Claude Code can be pointed elsewhere, and the plugin reports where it wrote.
        "/srv/claude-work/projects/x/session_1.jsonl",
    ] {
        assert!(
            super::is_plausible_transcript_path(Path::new(path)),
            "{path} should be read"
        );
    }
    for path in [
        "/home/me/.ssh/id_ed25519",
        "/etc/shadow",
        "/home/me/.claude/projects/x/session.json",
        "/home/me/.claude/projects/x/session.jsonl.bak",
        "projects/x/session.jsonl",
        "../session.jsonl",
        "",
    ] {
        assert!(
            !super::is_plausible_transcript_path(Path::new(path)),
            "{path:?} should not be read"
        );
    }
}

/// On Windows a path can name a share on another host, and asking the filesystem about it is
/// enough to start authenticating to that host.
#[cfg(windows)]
#[test]
fn a_network_path_is_not_taken_for_a_transcript() {
    use std::path::Path;

    for path in [
        r"\\attacker-host\share\session.jsonl",
        r"\\?\UNC\attacker-host\share\session.jsonl",
        r"\\.\pipe\session.jsonl",
        "//attacker-host/share/session.jsonl",
    ] {
        assert!(
            !super::is_plausible_transcript_path(Path::new(path)),
            "{path} should not be probed"
        );
    }
    assert!(super::is_plausible_transcript_path(Path::new(
        r"C:\Users\me\.claude\projects\x\session.jsonl"
    )));
}

#[test]
fn a_session_id_is_only_a_file_name() {
    for id in ["0f3c9a1e-77b2", "session_1", "ABC123"] {
        assert!(super::is_plain_id(id), "{id} is an id");
    }
    for id in ["", "../x", "a/b", r"a\b", "C:x", "a b", "a.jsonl", "a\0b"] {
        assert!(!super::is_plain_id(id), "{id:?} is not an id");
    }
}

/// The directory is one component however odd the working directory, and is spelt the way Claude
/// Code spells it, which turns an underscore into a dash as well.
#[test]
fn a_project_directory_is_named_the_way_claude_code_names_it() {
    assert_eq!(
        super::mangle_project_dir("/home/me/dev/cognitive_construct"),
        "-home-me-dev-cognitive-construct"
    );
    assert_eq!(
        super::mangle_project_dir("/home/me/my.app"),
        "-home-me-my-app"
    );
    // Separators of any kind are flattened, so the result cannot climb out of the projects root
    // or replace it.
    assert_eq!(super::mangle_project_dir(r"C:\Users\me"), "C--Users-me");
    assert!(!super::mangle_project_dir("/../..//x").contains(['/', '\\']));
}
