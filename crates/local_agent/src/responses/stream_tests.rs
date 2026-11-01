use super::*;
use crate::EndpointSchema;

fn config() -> LocalEndpointConfig {
    LocalEndpointConfig {
        base_url: "https://example.com".to_string(),
        api_key: String::new(),
        model_slug: "test-model".to_string(),
        schema: EndpointSchema::Responses,
    }
}

/// Feeds events as the provider would and collects everything emitted.
fn run(events: Vec<Value>) -> Result<Vec<AgentEvent>, LocalAgentError> {
    let mut stream = ResponseStream::default();
    let mut emitted = vec![];
    for event in events {
        let step = stream.ingest("", &event.to_string(), &config())?;
        emitted.extend(step.events);
        if step.finished {
            break;
        }
    }
    Ok(emitted)
}

fn completed(output: Value) -> Value {
    json!({
        "type": "response.completed",
        "response": {
            "status": "completed",
            "output": output,
            "usage": { "input_tokens": 11, "output_tokens": 7 },
        },
    })
}

fn usage() -> AgentEvent {
    AgentEvent::Usage {
        input_tokens: 11,
        output_tokens: 7,
    }
}

fn text_delta(item_id: &str, delta: &str) -> Value {
    json!({ "type": "response.output_text.delta", "item_id": item_id, "delta": delta })
}

fn call(id: &str, name: &str, arguments: Value) -> AgentEvent {
    AgentEvent::ToolCall(ToolCallRequest {
        id: id.to_string(),
        name: name.to_string(),
        arguments,
    })
}

fn reasoning_item() -> Value {
    json!({
        "type": "reasoning",
        "id": "rs_1",
        "summary": [{ "type": "summary_text", "text": "Plan the listing." }],
        "encrypted_content": "enc_1",
    })
}

fn replayed_reasoning() -> AgentEvent {
    AgentEvent::ReplayItem(json!({
        "type": "reasoning",
        "summary": [{ "type": "summary_text", "text": "Plan the listing." }],
        "encrypted_content": "enc_1",
    }))
}

#[test]
fn text_deltas_pass_through_and_completion_reports_usage() {
    let events = run(vec![
        json!({ "type": "response.created", "response": {} }),
        text_delta("msg_1", "git "),
        text_delta("msg_1", "status"),
        json!({ "type": "response.output_text.done", "item_id": "msg_1", "text": "git status" }),
        completed(json!([{
            "type": "message", "id": "msg_1", "role": "assistant",
            "content": [{ "type": "output_text", "text": "git status" }],
        }])),
    ])
    .expect("stream should succeed");

    assert_eq!(
        events,
        vec![
            AgentEvent::TextDelta("git ".to_string()),
            AgentEvent::TextDelta("status".to_string()),
            usage(),
        ]
    );
}

#[test]
fn text_missing_from_the_stream_is_backfilled_from_the_completed_payload() {
    let events = run(vec![completed(json!([{
        "type": "message", "id": "msg_1", "role": "assistant",
        "content": [{ "type": "output_text", "text": "all at once" }],
    }]))])
    .expect("stream should succeed");

    assert_eq!(
        events,
        vec![AgentEvent::TextDelta("all at once".to_string()), usage()]
    );
}

#[test]
fn text_finished_by_output_item_done_is_not_repeated_by_the_completed_payload() {
    let message = json!({
        "type": "message", "id": "msg_1", "role": "assistant",
        "content": [{ "type": "output_text", "text": "hello" }],
    });

    let events = run(vec![
        json!({ "type": "response.output_item.done", "output_index": 0, "item": message }),
        completed(json!([message])),
    ])
    .expect("stream should succeed");

    assert_eq!(
        events,
        vec![AgentEvent::TextDelta("hello".to_string()), usage()]
    );
}

#[test]
fn refusals_are_surfaced_as_text() {
    let events = run(vec![
        json!({ "type": "response.refusal.delta", "item_id": "msg_1", "delta": "I can't." }),
        completed(json!([{
            "type": "message", "id": "msg_1", "role": "assistant",
            "content": [{ "type": "refusal", "refusal": "I can't." }],
        }])),
    ])
    .expect("stream should succeed");

    assert_eq!(
        events,
        vec![AgentEvent::TextDelta("I can't.".to_string()), usage()]
    );
}

#[test]
fn reasoning_summary_parts_are_kept_apart() {
    let events = run(vec![
        json!({ "type": "response.reasoning_summary_part.added", "item_id": "rs_1", "summary_index": 0 }),
        json!({ "type": "response.reasoning_summary_text.delta", "item_id": "rs_1", "summary_index": 0, "delta": "First." }),
        json!({ "type": "response.reasoning_summary_part.added", "item_id": "rs_1", "summary_index": 1 }),
        json!({ "type": "response.reasoning_summary_text.delta", "item_id": "rs_1", "summary_index": 1, "delta": "Second." }),
        completed(json!([])),
    ])
    .expect("stream should succeed");

    assert_eq!(
        events,
        vec![
            AgentEvent::ReasoningDelta("First.".to_string()),
            AgentEvent::ReasoningDelta("\n\n".to_string()),
            AgentEvent::ReasoningDelta("Second.".to_string()),
            usage(),
        ]
    );
}

#[test]
fn a_completed_reasoning_item_is_replayable_without_its_id() {
    let events = run(vec![
        json!({ "type": "response.reasoning_summary_text.delta", "item_id": "rs_1", "summary_index": 0, "delta": "Plan the listing." }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": reasoning_item() }),
        completed(json!([reasoning_item()])),
    ])
    .expect("stream should succeed");

    assert_eq!(
        events,
        vec![
            AgentEvent::ReasoningDelta("Plan the listing.".to_string()),
            replayed_reasoning(),
            usage(),
        ]
    );
}

#[test]
fn reasoning_not_streamed_is_backfilled_once() {
    let events = run(vec![
        json!({ "type": "response.output_item.done", "output_index": 0, "item": reasoning_item() }),
        completed(json!([reasoning_item()])),
    ])
    .expect("stream should succeed");

    assert_eq!(
        events,
        vec![
            AgentEvent::ReasoningDelta("Plan the listing.".to_string()),
            replayed_reasoning(),
            usage(),
        ]
    );
}

#[test]
fn an_empty_summary_is_still_replayed_as_an_empty_array() {
    let item =
        json!({ "type": "reasoning", "id": "rs_2", "summary": [], "encrypted_content": "enc_2" });

    let events = run(vec![completed(json!([item]))]).expect("stream should succeed");

    assert_eq!(
        events,
        vec![
            AgentEvent::ReplayItem(json!({
                "type": "reasoning",
                "summary": [],
                "encrypted_content": "enc_2",
            })),
            usage(),
        ]
    );
}

#[test]
fn reasoning_without_encrypted_content_is_not_replayable() {
    let item = json!({ "type": "reasoning", "id": "rs_3", "summary": [] });

    let events = run(vec![completed(json!([item]))]).expect("stream should succeed");

    assert_eq!(events, vec![usage()]);
}

#[test]
fn reasoning_content_is_carried_into_the_replay_item() {
    let item = json!({
        "type": "reasoning", "id": "rs_4", "summary": [], "encrypted_content": "enc_4",
        "content": [{ "type": "reasoning_text", "text": "raw" }],
    });

    let events = run(vec![completed(json!([item]))]).expect("stream should succeed");

    assert_eq!(
        events[1],
        AgentEvent::ReplayItem(json!({
            "type": "reasoning",
            "summary": [],
            "encrypted_content": "enc_4",
            "content": [{ "type": "reasoning_text", "text": "raw" }],
        }))
    );
}

#[test]
fn a_streamed_function_call_is_emitted_with_its_provider_call_id_after_replay_items() {
    let events = run(vec![
        json!({ "type": "response.output_item.added", "output_index": 0, "item": {
            "type": "reasoning", "id": "rs_1", "summary": [],
        }}),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": reasoning_item() }),
        json!({ "type": "response.output_item.added", "output_index": 1, "item": {
            "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "run_shell_command", "arguments": "",
        }}),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1", "output_index": 1, "delta": "{\"command\":" }),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1", "output_index": 1, "delta": "\"ls\"}" }),
        json!({ "type": "response.function_call_arguments.done", "item_id": "fc_1", "output_index": 1, "arguments": "{\"command\":\"ls\"}" }),
        json!({ "type": "response.output_item.done", "output_index": 1, "item": {
            "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "run_shell_command", "arguments": "{\"command\":\"ls\"}",
        }}),
        completed(json!([reasoning_item(), {
            "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "run_shell_command", "arguments": "{\"command\":\"ls\"}",
        }])),
    ])
    .expect("stream should succeed");

    assert_eq!(
        events,
        vec![
            AgentEvent::ReasoningDelta("Plan the listing.".to_string()),
            replayed_reasoning(),
            call("call_1", "run_shell_command", json!({ "command": "ls" })),
            usage(),
        ]
    );
}

#[test]
fn arguments_done_without_a_name_falls_back_to_the_item_metadata() {
    let events = run(vec![
        json!({ "type": "response.output_item.added", "output_index": 0, "item": {
            "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "read_files", "arguments": "",
        }}),
        // No name and no call_id: only the item id and index identify the call.
        json!({ "type": "response.function_call_arguments.done", "item_id": "fc_1", "arguments": "{\"paths\":[\"a\"]}" }),
        completed(json!([])),
    ])
    .expect("stream should succeed");

    assert_eq!(
        events,
        vec![
            call("call_1", "read_files", json!({ "paths": ["a"] })),
            usage()
        ]
    );
}

#[test]
fn output_item_done_supplies_authoritative_ids() {
    let events = run(vec![
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1", "output_index": 0, "delta": "{}" }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": {
            "type": "function_call", "id": "fc_1", "call_id": "call_9", "name": "t", "arguments": "{}",
        }}),
        completed(json!([])),
    ])
    .expect("stream should succeed");

    assert_eq!(events, vec![call("call_9", "t", json!({})), usage()]);
}

#[test]
fn a_function_call_only_in_the_completed_payload_is_still_emitted() {
    let events = run(vec![completed(json!([{
        "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "t", "arguments": "{\"a\":1}",
    }]))])
    .expect("stream should succeed");

    assert_eq!(
        events,
        vec![call("call_1", "t", json!({ "a": 1 })), usage()]
    );
}

#[test]
fn a_call_without_call_id_echoes_the_item_id() {
    let events = run(vec![completed(json!([{
        "type": "function_call", "id": "fc_1", "name": "t", "arguments": "{}",
    }]))])
    .expect("stream should succeed");

    assert_eq!(events, vec![call("fc_1", "t", json!({})), usage()]);
}

#[test]
fn multiple_calls_keep_output_order() {
    let events = run(vec![
        json!({ "type": "response.output_item.done", "output_index": 1, "item": {
            "type": "function_call", "id": "fc_b", "call_id": "call_b", "name": "b", "arguments": "{}",
        }}),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": {
            "type": "function_call", "id": "fc_a", "call_id": "call_a", "name": "a", "arguments": "{}",
        }}),
        completed(json!([])),
    ])
    .expect("stream should succeed");

    assert_eq!(
        events,
        vec![
            call("call_a", "a", json!({})),
            call("call_b", "b", json!({})),
            usage()
        ]
    );
}

#[test]
fn empty_arguments_become_an_empty_object() {
    let events = run(vec![completed(json!([{
        "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "t", "arguments": "",
    }]))])
    .expect("stream should succeed");

    assert_eq!(events, vec![call("call_1", "t", json!({})), usage()]);
}

#[test]
fn malformed_arguments_are_a_parse_error() {
    let result = run(vec![completed(json!([{
        "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "t", "arguments": "{\"a\":",
    }]))]);

    assert!(
        matches!(result, Err(LocalAgentError::Parse(_))),
        "{result:?}"
    );
}

#[test]
fn a_call_without_a_name_is_a_parse_error() {
    let result = run(vec![completed(json!([{
        "type": "function_call", "id": "fc_1", "call_id": "call_1", "arguments": "{}",
    }]))]);

    assert!(
        matches!(result, Err(LocalAgentError::Parse(_))),
        "{result:?}"
    );
}

#[test]
fn the_event_type_falls_back_to_the_sse_event_name() {
    let mut stream = ResponseStream::default();

    let step = stream
        .ingest(
            "response.output_text.delta",
            r#"{"item_id":"msg_1","delta":"hi"}"#,
            &config(),
        )
        .expect("event should parse");

    assert_eq!(step.events, vec![AgentEvent::TextDelta("hi".to_string())]);
}

#[test]
fn unknown_events_and_done_sentinels_are_ignored() {
    let mut stream = ResponseStream::default();

    for (name, data) in [
        ("response.in_progress", r#"{"type":"response.in_progress"}"#),
        (
            "response.some_future_event",
            r#"{"type":"response.some_future_event"}"#,
        ),
        ("message", "[DONE]"),
    ] {
        let step = stream
            .ingest(name, data, &config())
            .expect("event should be ignored");
        assert!(step.events.is_empty() && !step.finished, "{name}");
    }
}

#[test]
fn undecodable_event_data_is_a_parse_error() {
    let result = ResponseStream::default().ingest("", "{not json", &config());

    assert!(matches!(result, Err(LocalAgentError::Parse(_))));
}

#[test]
fn max_output_tokens_ends_the_turn_normally() {
    let mut stream = ResponseStream::default();
    let _ = stream.ingest("", &text_delta("msg_1", "cut off").to_string(), &config());

    let step = stream
        .ingest(
            "",
            &json!({
                "type": "response.incomplete",
                "response": {
                    "incomplete_details": { "reason": "max_output_tokens" },
                    "output": [],
                    "usage": { "input_tokens": 11, "output_tokens": 7 },
                },
            })
            .to_string(),
            &config(),
        )
        .expect("a length cutoff is not an error");

    assert!(step.finished);
    assert_eq!(step.events, vec![usage()]);
}

#[test]
fn other_incomplete_reasons_are_errors() {
    let result = run(vec![json!({
        "type": "response.incomplete",
        "response": { "incomplete_details": { "reason": "content_filter" } },
    })]);

    assert!(
        matches!(&result, Err(LocalAgentError::ErrorStatus { status: 400, body }) if body.contains("content_filter")),
        "{result:?}"
    );
}

fn failure(code: &str, message: &str) -> Value {
    json!({
        "type": "response.failed",
        "response": { "status": "failed", "error": { "code": code, "message": message } },
    })
}

#[test]
fn failures_map_onto_the_same_errors_as_http_statuses() {
    let context = run(vec![failure(
        "context_length_exceeded",
        "Your input exceeds the context window of this model.",
    )]);
    assert!(
        matches!(context, Err(LocalAgentError::ContextWindowExceeded { .. })),
        "{context:?}"
    );

    let auth = run(vec![failure(
        "invalid_api_key",
        "Incorrect API key provided",
    )]);
    assert!(
        matches!(&auth, Err(LocalAgentError::InvalidApiKey { model_slug }) if model_slug == "test-model"),
        "{auth:?}"
    );

    for (code, expected) in [
        ("rate_limit_exceeded", 429),
        ("server_error", 500),
        ("overloaded", 503),
        ("invalid_prompt", 400),
    ] {
        let result = run(vec![failure(code, "nope")]);
        assert!(
            matches!(&result, Err(LocalAgentError::ErrorStatus { status, body }) if *status == expected && body == "nope"),
            "{code}: {result:?}"
        );
    }
}

#[test]
fn error_events_are_read_from_the_top_level_or_a_nested_error() {
    let top_level = run(vec![json!({
        "type": "error", "code": "rate_limit_exceeded", "message": "slow down", "param": null,
    })]);
    assert!(
        matches!(&top_level, Err(LocalAgentError::ErrorStatus { status: 429, body }) if body == "slow down"),
        "{top_level:?}"
    );

    let nested = run(vec![json!({
        "type": "error",
        "error": { "type": "invalid_request_error", "message": "maximum context length is 8192 tokens" },
    })]);
    assert!(
        matches!(nested, Err(LocalAgentError::ContextWindowExceeded { .. })),
        "{nested:?}"
    );
}

#[test]
fn a_failure_without_a_message_reports_the_raw_event() {
    let result = run(vec![
        json!({ "type": "response.failed", "response": { "status": "failed" } }),
    ]);

    assert!(
        matches!(&result, Err(LocalAgentError::ErrorStatus { body, .. }) if body.contains("response.failed")),
        "{result:?}"
    );
}
