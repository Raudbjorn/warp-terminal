use super::*;
use crate::terminal::cli_agent_sessions::event::{
    CLI_AGENT_NOTIFICATION_SENTINEL, CLIAgentEventSource, CLIAgentEventType,
};

#[test]
fn codex_parses_any_text_as_stop() {
    let event = Osc9FallbackSessionHandler::parse_osc9_text(CLIAgent::Codex, "Agent turn complete")
        .unwrap();
    assert_eq!(event.event, CLIAgentEventType::Stop);
    assert_eq!(event.agent, CLIAgent::Codex);
    assert_eq!(event.payload.query.as_deref(), Some("Agent turn complete"));
}

#[test]
fn codex_body_becomes_query() {
    let event = Osc9FallbackSessionHandler::parse_osc9_text(
        CLIAgent::Codex,
        "I've updated the README with the new instructions.",
    )
    .unwrap();
    assert_eq!(event.event, CLIAgentEventType::Stop);
    assert_eq!(
        event.payload.query.as_deref(),
        Some("I've updated the README with the new instructions.")
    );
}

#[test]
fn codex_approval_text_still_becomes_stop() {
    let event = Osc9FallbackSessionHandler::parse_osc9_text(
        CLIAgent::Codex,
        "Approval requested: rm -rf /tmp/foo",
    )
    .unwrap();
    assert_eq!(event.event, CLIAgentEventType::Stop);
    assert_eq!(
        event.payload.query.as_deref(),
        Some("Approval requested: rm -rf /tmp/foo")
    );
}

#[test]
fn codex_ignores_empty_body() {
    assert!(Osc9FallbackSessionHandler::parse_osc9_text(CLIAgent::Codex, "").is_none());
    assert!(Osc9FallbackSessionHandler::parse_osc9_text(CLIAgent::Codex, "   ").is_none());
}

#[test]
fn codex_try_parse_ignores_titled_notifications() {
    let mut handler = Osc9FallbackSessionHandler {
        agent: CLIAgent::Codex,
    };
    assert!(
        handler
            .try_parse(Some("some-title"), "Agent turn complete", false)
            .is_none()
    );
}

#[test]
fn codex_try_parse_handles_osc9() {
    let mut handler = Osc9FallbackSessionHandler {
        agent: CLIAgent::Codex,
    };
    let event = handler
        .try_parse(None, "Agent turn complete", false)
        .unwrap();
    assert_eq!(event.event, CLIAgentEventType::Stop);
}

#[test]
fn codex_try_parse_ignores_osc9_when_plugin_already_active() {
    let _guard = FeatureFlag::CodexPlugin.override_enabled(true);
    let mut handler = Osc9FallbackSessionHandler {
        agent: CLIAgent::Codex,
    };
    let body = r#"{"v":1,"agent":"codex","event":"permission_request","summary":"Approve?","tool_name":"Bash"}"#;

    let event = handler
        .try_parse(Some(CLI_AGENT_NOTIFICATION_SENTINEL), body, false)
        .unwrap();

    assert_eq!(event.event, CLIAgentEventType::PermissionRequest);
    // Once the session is rich, OSC 9 fallback is dropped.
    assert!(
        handler
            .try_parse(None, "Agent turn complete", true)
            .is_none()
    );
}

#[test]
fn codex_try_parse_ignores_structured_event_without_codex_plugin() {
    let _guard = FeatureFlag::CodexPlugin.override_enabled(false);
    let mut handler = Osc9FallbackSessionHandler {
        agent: CLIAgent::Codex,
    };
    let body = r#"{"v":1,"agent":"codex","event":"permission_request","summary":"Approve?","tool_name":"Bash"}"#;

    assert!(
        handler
            .try_parse(Some(CLI_AGENT_NOTIFICATION_SENTINEL), body, false)
            .is_none()
    );
    assert!(
        handler
            .try_parse(None, "Agent turn complete", false)
            .is_some()
    );
}

#[test]
fn codex_try_parse_ignores_other_structured_agents() {
    let mut handler = Osc9FallbackSessionHandler {
        agent: CLIAgent::Codex,
    };
    let body = r#"{"v":1,"agent":"claude","event":"stop"}"#;

    assert!(
        handler
            .try_parse(Some(CLI_AGENT_NOTIFICATION_SENTINEL), body, false)
            .is_none()
    );
    assert!(
        handler
            .try_parse(None, "Agent turn complete", false)
            .is_some()
    );
}

#[test]
fn auggie_is_supported() {
    assert!(is_agent_supported(&CLIAgent::Auggie));
}

#[test]
fn grok_listener_prefers_rich_events_over_osc9_fallback() {
    let mut handler = create_handler(&CLIAgent::Grok).expect("Grok should support notifications");
    let osc9 = handler
        .try_parse(None, "Turn complete · Grok", false)
        .unwrap();
    assert_eq!(osc9.event, CLIAgentEventType::Stop);
    assert_eq!(osc9.agent, CLIAgent::Grok);
    assert_eq!(osc9.payload.query.as_deref(), Some("Turn complete · Grok"));
    assert_eq!(osc9.source, CLIAgentEventSource::CodexOsc9Fallback);

    let body = r#"{"v":1,"agent":"grok","event":"session_start","session_id":"s1","plugin_version":"1.0.0"}"#;
    let rich = handler
        .try_parse(Some(CLI_AGENT_NOTIFICATION_SENTINEL), body, false)
        .unwrap();
    assert_eq!(rich.event, CLIAgentEventType::SessionStart);
    assert_eq!(rich.agent, CLIAgent::Grok);
    assert_eq!(rich.payload.plugin_version.as_deref(), Some("1.0.0"));

    assert!(
        handler
            .try_parse(None, "Turn complete · Grok", true)
            .is_none()
    );
}

#[test]
fn grok_try_parse_ignores_other_agents() {
    let mut handler = create_handler(&CLIAgent::Grok).expect("Grok should support notifications");
    let body = r#"{"v":1,"agent":"claude","event":"stop"}"#;
    assert!(
        handler
            .try_parse(Some(CLI_AGENT_NOTIFICATION_SENTINEL), body, false)
            .is_none()
    );
}

#[test]
fn auggie_default_handler_skips_session_start() {
    let mut handler = DefaultSessionListener;
    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Auggie,
        event: CLIAgentEventType::SessionStart,
        session_id: None,
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };
    assert!(handler.handle_event(event).is_none());
}

#[test]
fn auggie_default_handler_forwards_stop() {
    let mut handler = DefaultSessionListener;
    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Auggie,
        event: CLIAgentEventType::Stop,
        session_id: None,
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };
    assert!(handler.handle_event(event).is_some());
}

#[test]
fn pi_is_supported() {
    assert!(is_agent_supported(&CLIAgent::Pi));
}

#[test]
fn oh_my_pi_is_supported() {
    assert!(is_agent_supported(&CLIAgent::OhMyPi));
}

#[test]
fn pi_default_handler_skips_session_start() {
    let mut handler = DefaultSessionListener;
    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Pi,
        event: CLIAgentEventType::SessionStart,
        session_id: None,
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };
    assert!(handler.handle_event(event).is_none());
}

#[test]
fn pi_default_handler_forwards_stop() {
    let mut handler = DefaultSessionListener;
    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Pi,
        event: CLIAgentEventType::Stop,
        session_id: None,
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };
    assert!(handler.handle_event(event).is_some());
}

#[test]
fn droid_is_supported() {
    assert!(is_agent_supported(&CLIAgent::Droid));
}

#[test]
fn droid_default_handler_skips_session_start() {
    let mut handler = DefaultSessionListener;
    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Droid,
        event: CLIAgentEventType::SessionStart,
        session_id: None,
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };
    assert!(handler.handle_event(event).is_none());
}

#[test]
fn droid_default_handler_forwards_stop() {
    let mut handler = DefaultSessionListener;
    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Droid,
        event: CLIAgentEventType::Stop,
        session_id: None,
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };
    assert!(handler.handle_event(event).is_some());
}

#[test]
fn droid_default_handler_forwards_permission_request() {
    let mut handler = DefaultSessionListener;
    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Droid,
        event: CLIAgentEventType::PermissionRequest,
        session_id: None,
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };
    assert!(handler.handle_event(event).is_some());
}

#[test]
fn muse_is_supported_via_default_listener() {
    assert!(is_agent_supported(&CLIAgent::Muse));
    let mut handler = create_handler(&CLIAgent::Muse).expect("Muse should support OSC 777");

    let stop_body = r#"{"v":1,"agent":"muse","event":"stop"}"#;
    let parsed_stop = handler
        .try_parse(Some(CLI_AGENT_NOTIFICATION_SENTINEL), stop_body, false)
        .expect("should parse muse stop");
    assert_eq!(parsed_stop.agent, CLIAgent::Muse);
    assert_eq!(parsed_stop.event, CLIAgentEventType::Stop);
    assert!(handler.handle_event(parsed_stop).is_some());

    let claude_body = r#"{"v":1,"agent":"claude","event":"stop"}"#;
    let parsed_claude = handler
        .try_parse(Some(CLI_AGENT_NOTIFICATION_SENTINEL), claude_body, false)
        .expect("structured events still parse");
    assert_eq!(parsed_claude.agent, CLIAgent::Claude);

    assert!(
        handler.try_parse(None, "Turn complete", false).is_none(),
        "Muse must not treat OSC 9 text as a stop event"
    );
}

#[test]
fn warp_tui_notifications_are_supported() {
    assert!(is_agent_supported(&CLIAgent::WarpTui));
    let mut handler = create_handler(&CLIAgent::WarpTui).expect("should create handler");
    let stop_body = r#"{"v":1,"agent":"warp-tui","event":"stop","session_id":"sess-42"}"#;
    let parsed_stop = handler
        .try_parse(Some(CLI_AGENT_NOTIFICATION_SENTINEL), stop_body, false)
        .expect("should parse stop");
    assert_eq!(parsed_stop.agent, CLIAgent::WarpTui);
    assert_eq!(parsed_stop.event, CLIAgentEventType::Stop);
    assert!(handler.handle_event(parsed_stop).is_some());
}

#[test]
fn oh_my_pi_end_to_end_parsing_and_handling() {
    let mut handler = create_handler(&CLIAgent::OhMyPi).expect("should create handler");

    // Test session_start payload: proves SessionStart is skipped
    let start_body = r#"{"v":1,"agent":"omp","event":"session_start"}"#;
    let parsed_start = handler
        .try_parse(Some(CLI_AGENT_NOTIFICATION_SENTINEL), start_body, false)
        .expect("should successfully parse session_start payload");
    assert_eq!(parsed_start.agent, CLIAgent::OhMyPi);
    assert_eq!(parsed_start.event, CLIAgentEventType::SessionStart);
    assert!(handler.handle_event(parsed_start).is_none());

    // Test stop payload: proves Stop forwards with CLIAgent::OhMyPi
    let stop_body = r#"{"v":1,"agent":"omp","event":"stop"}"#;
    let parsed_stop = handler
        .try_parse(Some(CLI_AGENT_NOTIFICATION_SENTINEL), stop_body, false)
        .expect("should successfully parse stop payload");
    assert_eq!(parsed_stop.agent, CLIAgent::OhMyPi);
    assert_eq!(parsed_stop.event, CLIAgentEventType::Stop);

    let handled_stop = handler
        .handle_event(parsed_stop)
        .expect("should forward stop event");
    assert_eq!(handled_stop.agent, CLIAgent::OhMyPi);
    assert_eq!(handled_stop.event, CLIAgentEventType::Stop);
}

#[test]
fn antigravity_notifications_are_supported() {
    assert!(is_agent_supported(&CLIAgent::Antigravity));
    let mut handler = create_handler(&CLIAgent::Antigravity).expect("should create handler");

    let start_body = r#"{"v":1,"agent":"agy","event":"session_start"}"#;
    let parsed_start = handler
        .try_parse(Some(CLI_AGENT_NOTIFICATION_SENTINEL), start_body, false)
        .expect("should successfully parse session_start payload");
    assert_eq!(parsed_start.agent, CLIAgent::Antigravity);
    assert_eq!(parsed_start.event, CLIAgentEventType::SessionStart);
    assert!(handler.handle_event(parsed_start).is_none());

    let task_started_body = r#"{"v":1,"agent":"antigravity","event":"task_started"}"#;
    let parsed_task_started = handler
        .try_parse(
            Some(CLI_AGENT_NOTIFICATION_SENTINEL),
            task_started_body,
            false,
        )
        .expect("should successfully parse task_started payload");
    assert_eq!(parsed_task_started.agent, CLIAgent::Antigravity);
    assert_eq!(parsed_task_started.event, CLIAgentEventType::PromptSubmit);
    assert!(handler.handle_event(parsed_task_started).is_some());

    let stop_body = r#"{"v":1,"agent":"agy","event":"stop","session_id":"agy-123"}"#;
    let parsed_stop = handler
        .try_parse(Some(CLI_AGENT_NOTIFICATION_SENTINEL), stop_body, false)
        .expect("should successfully parse stop payload");
    assert_eq!(parsed_stop.agent, CLIAgent::Antigravity);
    assert_eq!(parsed_stop.event, CLIAgentEventType::Stop);
    assert!(handler.handle_event(parsed_stop).is_some());

    let task_complete_body =
        r#"{"v":1,"agent":"antigravity","event":"task_complete","session_id":"agy-456"}"#;
    let parsed_task_complete = handler
        .try_parse(
            Some(CLI_AGENT_NOTIFICATION_SENTINEL),
            task_complete_body,
            false,
        )
        .expect("should successfully parse task_complete payload");
    assert_eq!(parsed_task_complete.agent, CLIAgent::Antigravity);
    assert_eq!(parsed_task_complete.event, CLIAgentEventType::Stop);
    assert!(handler.handle_event(parsed_task_complete).is_some());

    let prompt_req_body = r#"{"v":1,"agent":"agy","event":"prompt_required"}"#;
    let parsed_prompt_req = handler
        .try_parse(
            Some(CLI_AGENT_NOTIFICATION_SENTINEL),
            prompt_req_body,
            false,
        )
        .expect("should successfully parse prompt_required payload");
    assert_eq!(parsed_prompt_req.agent, CLIAgent::Antigravity);
    assert_eq!(parsed_prompt_req.event, CLIAgentEventType::NeedsInput);
    assert!(handler.handle_event(parsed_prompt_req).is_some());
}

fn claude_event(body: &str) -> CLIAgentEvent {
    parse_event(Some(CLI_AGENT_NOTIFICATION_SENTINEL), body).unwrap()
}

#[test]
fn warp_plugin_events_count_until_harps_plugin_speaks() {
    let warp = claude_event(r#"{"v":1,"agent":"claude","event":"prompt_submit"}"#);
    let harp = claude_event(r#"{"v":1,"agent":"claude","producer":"harp","event":"stop"}"#);
    let now = Instant::now();
    let mut choice = PluginChoice::default();
    // Only the Warp plugin is loaded (Claude started before Harp): its events are the status.
    assert!(keep_from_one_plugin(&mut choice, &warp, now));
    // Harp's plugin speaks (a different hook): from now on Harp's win, the Warp copy is dropped.
    assert!(keep_from_one_plugin(&mut choice, &harp, now));
    assert!(!keep_from_one_plugin(&mut choice, &warp, now));
    assert!(keep_from_one_plugin(&mut choice, &harp, now));
}

/// Both plugins report one hook; if the Warp copy was counted just before Harp's first event,
/// Harp's copy must not count again (two permission requests, one completion, a row stuck on
/// "needs you"). Fails if the handover event is counted.
#[test]
fn harps_first_event_is_dropped_when_the_warp_plugin_just_reported_the_same_hook() {
    let request = |producer: &str| {
        claude_event(&format!(
            r#"{{"v":1,"agent":"claude"{producer},"event":"permission_request","session_id":"s","tool_name":"Bash","tool_input_preview":"ls"}}"#
        ))
    };
    let warp = request("");
    let harp = request(r#","producer":"harp""#);
    let now = Instant::now();

    let mut choice = PluginChoice::default();
    assert!(keep_from_one_plugin(&mut choice, &warp, now));
    assert!(!keep_from_one_plugin(&mut choice, &harp, now));
    // Harp has spoken even though that copy was dropped: the Warp plugin is ignored now, and the
    // next Harp event counts.
    assert!(!keep_from_one_plugin(&mut choice, &warp, now));
    assert!(keep_from_one_plugin(&mut choice, &harp, now));

    // A different tool, or a Warp event from long before, is not the same hook.
    let other = claude_event(
        r#"{"v":1,"agent":"claude","producer":"harp","event":"permission_request","session_id":"s","tool_name":"Edit","tool_input_preview":"ls"}"#,
    );
    let mut choice = PluginChoice::default();
    assert!(keep_from_one_plugin(&mut choice, &warp, now));
    assert!(keep_from_one_plugin(&mut choice, &other, now));
    let mut choice = PluginChoice::default();
    assert!(keep_from_one_plugin(&mut choice, &warp, now));
    let later = now + HANDOVER_DUPLICATE_WINDOW + Duration::from_secs(1);
    assert!(keep_from_one_plugin(&mut choice, &harp, later));
}

/// The event that creates the listener is applied without the filter. A reattach to a running
/// agent that is waiting on a permission starts with that request; Harp's copy of it must not
/// count a second time. Fails if the listener starts with an empty choice.
#[test]
fn the_event_that_creates_the_listener_seeds_the_plugin_choice() {
    let request = |producer: &str| {
        claude_event(&format!(
            r#"{{"v":1,"agent":"claude"{producer},"event":"permission_request","session_id":"s","tool_name":"Bash","tool_input_preview":"ls"}}"#
        ))
    };
    let (warp, harp) = (request(""), request(r#","producer":"harp""#));
    let now = Instant::now();

    // Created by the Warp plugin's request: Harp's copy a moment later is the duplicate.
    let mut choice = PluginChoice::after(&warp, now);
    assert!(!keep_from_one_plugin(&mut choice, &harp, now));
    assert!(keep_from_one_plugin(&mut choice, &harp, now));
    assert!(!keep_from_one_plugin(&mut choice, &warp, now));

    // Created by Harp's request: the Warp plugin's copy is ignored from the start.
    let mut choice = PluginChoice::after(&harp, now);
    assert!(!keep_from_one_plugin(&mut choice, &warp, now));
    assert!(keep_from_one_plugin(&mut choice, &harp, now));

    // Unseeded, the creating request is unknown to the filter and Harp's copy counts again.
    let mut choice = PluginChoice::default();
    assert!(keep_from_one_plugin(&mut choice, &harp, now));
}

#[test]
fn only_harps_producer_marks_an_event_as_harps() {
    assert!(
        claude_event(r#"{"v":1,"agent":"claude","producer":"harp","event":"stop"}"#)
            .payload
            .from_harp_plugin
    );
    assert!(
        !claude_event(r#"{"v":1,"agent":"claude","producer":"warp","event":"stop"}"#)
            .payload
            .from_harp_plugin
    );
    assert!(
        !claude_event(r#"{"v":1,"agent":"claude","event":"stop"}"#)
            .payload
            .from_harp_plugin
    );
}
