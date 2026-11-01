use std::time::Duration;

use ::conn::{ConnEntryKind, ConnStatus};
use chrono::Utc;
use warp_util::git::run_git_command;
use warpui::r#async::Timer;
use warpui::{App, EntityId, SingletonEntity};

use super::{ConnModel, MAX_TRANSCRIPT_BYTES, conn_status};
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
        assert_eq!(project.as_deref(), Some("checkout"));
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

        let (adopted, in_flight) = ConnModel::handle(&app).read(&app, |me, _| {
            (
                me.session(view_id).is_some_and(|s| s.story().is_some()),
                me.read_in_flight(view_id),
            )
        });
        if adopted {
            assert_eq!(
                in_flight,
                Some(delayed),
                "the immediate read landed and the delayed one is still tracked"
            );
        }
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
