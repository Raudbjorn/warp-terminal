//! Wire-level tests for the Responses engine: a mockito server (or a raw TCP
//! listener where connection behavior matters) plays the endpoint, and the
//! whole HTTP/SSE path is exercised through the crate's public entry points.

use std::time::Duration;

use futures::StreamExt;
use mockito::{Matcher, Mock, Server, ServerGuard};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::*;
use crate::{EndpointSchema, ToolCallRequest, complete_text};

/// Generous bound for things that should happen immediately; only reached on
/// failure.
const PROMPT: Duration = Duration::from_secs(5);

fn config_for_url(base_url: String) -> LocalEndpointConfig {
    LocalEndpointConfig {
        base_url,
        api_key: "test-key".to_string(),
        model_slug: "test-model".to_string(),
        schema: EndpointSchema::Responses,
    }
}

fn config_for(server: &ServerGuard) -> LocalEndpointConfig {
    config_for_url(server.url())
}

fn user_transcript() -> Vec<ChatMessage> {
    vec![
        ChatMessage::System("You are a coding agent.".to_string()),
        ChatMessage::User("hi".to_string()),
    ]
}

fn shell_tool() -> ToolDefinition {
    ToolDefinition {
        name: "run_shell_command".to_string(),
        description: "Run a shell command".to_string(),
        parameters: json!({
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"],
        }),
    }
}

/// Backoff short enough that retrying tests do not wait.
fn fast_policy() -> RetryPolicy {
    RetryPolicy {
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(5),
        jitter_fraction: 0.0,
        ..RetryPolicy::default()
    }
}

fn sse_event(event: &Value) -> String {
    format!(
        "event: {}\ndata: {event}\n\n",
        event["type"].as_str().unwrap_or_default()
    )
}

fn sse_body(events: &[Value]) -> String {
    events.iter().map(sse_event).collect()
}

fn text_delta(delta: &str) -> Value {
    json!({ "type": "response.output_text.delta", "item_id": "msg_1", "delta": delta })
}

fn completed(output: Value) -> Value {
    json!({
        "type": "response.completed",
        "response": {
            "status": "completed",
            "output": output,
            "usage": { "input_tokens": 12, "output_tokens": 3 },
        },
    })
}

/// A complete one-message response.
fn text_response(text: &str) -> String {
    sse_body(&[
        json!({ "type": "response.created", "response": {} }),
        text_delta(text),
        completed(json!([{
            "type": "message", "id": "msg_1", "role": "assistant",
            "content": [{ "type": "output_text", "text": text }],
        }])),
    ])
}

fn responses_mock(server: &mut ServerGuard) -> Mock {
    server
        .mock("POST", "/v1/responses")
        .match_header("authorization", "Bearer test-key")
}

async fn collect(
    transcript: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
    config: LocalEndpointConfig,
) -> Vec<Result<AgentEvent, LocalAgentError>> {
    crate::run_turn(transcript, tools, config).collect().await
}

fn expect_ok(results: Vec<Result<AgentEvent, LocalAgentError>>) -> Vec<AgentEvent> {
    results
        .into_iter()
        .map(|result| result.expect("expected only Ok events"))
        .collect()
}

#[tokio::test]
async fn schema_dispatch_streams_text_usage_and_done_from_the_responses_endpoint() {
    let mut server = Server::new_async().await;
    let mock = responses_mock(&mut server)
        .match_header("content-type", "application/json")
        .match_body(Matcher::PartialJson(json!({
            "model": "test-model",
            "instructions": "You are a coding agent.",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "hi" }],
            }],
            "stream": true,
            "store": false,
        })))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(text_response("ok"))
        .create_async()
        .await;

    let events = expect_ok(collect(user_transcript(), vec![], config_for(&server)).await);

    mock.assert_async().await;
    assert_eq!(
        events,
        vec![
            AgentEvent::TextDelta("ok".to_string()),
            AgentEvent::Usage {
                input_tokens: 12,
                output_tokens: 3,
            },
            AgentEvent::Done,
        ]
    );
}

#[tokio::test]
async fn the_chat_schema_still_talks_to_chat_completions() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body("data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
        .create_async()
        .await;
    let config = LocalEndpointConfig {
        schema: EndpointSchema::ChatCompletions,
        ..config_for(&server)
    };

    let events = expect_ok(collect(user_transcript(), vec![], config).await);

    mock.assert_async().await;
    assert_eq!(
        events,
        vec![AgentEvent::TextDelta("ok".to_string()), AgentEvent::Done]
    );
}

#[tokio::test]
async fn a_full_responses_url_is_used_as_is() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("POST", "/gateway/responses")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(text_response("ok"))
        .create_async()
        .await;

    let events = expect_ok(
        collect(
            user_transcript(),
            vec![],
            config_for_url(format!("{}/gateway/responses", server.url())),
        )
        .await,
    );

    mock.assert_async().await;
    assert_eq!(events.last(), Some(&AgentEvent::Done));
}

#[tokio::test]
async fn sends_reasoning_tools_and_session_headers() {
    let mut server = Server::new_async().await;
    let mock = responses_mock(&mut server)
        .match_header(
            "session_id",
            Matcher::Regex("^warp-local-[0-9a-f]{16}$".to_string()),
        )
        .match_body(Matcher::PartialJson(json!({
            "model": "gpt-5.3-codex",
            "reasoning": { "effort": "high", "summary": "auto" },
            "include": ["reasoning.encrypted_content"],
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "tools": [{
                "type": "function",
                "name": "run_shell_command",
                "description": "Run a shell command",
                "strict": false,
            }],
        })))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(text_response("ok"))
        .create_async()
        .await;
    let config = LocalEndpointConfig {
        model_slug: "gpt-5.3-codex-high".to_string(),
        ..config_for(&server)
    };

    let events = expect_ok(collect(user_transcript(), vec![shell_tool()], config).await);

    mock.assert_async().await;
    assert_eq!(events.last(), Some(&AgentEvent::Done));
}

/// Replays what a tool-calling turn emitted into the next turn's transcript,
/// the way a host does.
fn follow_up_transcript(first: Vec<ChatMessage>, events: &[AgentEvent]) -> Vec<ChatMessage> {
    let replay_items = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ReplayItem(item) => Some(item.clone()),
            _ => None,
        })
        .collect();
    let tool_calls: Vec<ToolCallRequest> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCall(call) => Some(call.clone()),
            _ => None,
        })
        .collect();
    let mut transcript = first;
    transcript.push(ChatMessage::Tool {
        tool_call_id: tool_calls[0].id.clone(),
        content: "file.txt".to_string(),
    });
    transcript.insert(
        transcript.len() - 1,
        ChatMessage::Assistant {
            text: String::new(),
            tool_calls,
            replay_items,
        },
    );
    transcript
}

#[tokio::test]
async fn a_tool_turn_round_trips_reasoning_into_the_follow_up_request() {
    let reasoning = json!({
        "type": "reasoning", "id": "rs_1", "encrypted_content": "enc_1",
        "summary": [{ "type": "summary_text", "text": "Plan." }],
    });
    let function_call = json!({
        "type": "function_call", "id": "fc_1", "call_id": "call_1",
        "name": "run_shell_command", "arguments": "{\"command\":\"ls\"}",
    });
    let first_turn = sse_body(&[
        json!({ "type": "response.output_item.done", "output_index": 0, "item": reasoning }),
        json!({ "type": "response.output_item.added", "output_index": 1, "item": {
            "type": "function_call", "id": "fc_1", "call_id": "call_1",
            "name": "run_shell_command", "arguments": "",
        }}),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1", "output_index": 1, "delta": "{\"command\":\"ls\"}" }),
        // Real providers can omit the name here.
        json!({ "type": "response.function_call_arguments.done", "item_id": "fc_1", "output_index": 1, "arguments": "{\"command\":\"ls\"}" }),
        json!({ "type": "response.output_item.done", "output_index": 1, "item": function_call }),
        completed(json!([reasoning, function_call])),
    ]);
    let mut server = Server::new_async().await;
    let first_mock = responses_mock(&mut server)
        .match_body(Matcher::PartialJson(
            json!({ "input": [{ "role": "user" }] }),
        ))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(first_turn)
        .create_async()
        .await;
    let follow_up_mock = responses_mock(&mut server)
        .match_body(Matcher::PartialJson(json!({
            "input": [
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] },
                // No id: a stateless request cannot resolve one.
                {
                    "type": "reasoning",
                    "summary": [{ "type": "summary_text", "text": "Plan." }],
                    "encrypted_content": "enc_1",
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "run_shell_command",
                    "arguments": "{\"command\":\"ls\"}",
                },
                { "type": "function_call_output", "call_id": "call_1", "output": "file.txt" },
            ],
        })))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(text_response("done"))
        .create_async()
        .await;
    let config = LocalEndpointConfig {
        model_slug: "gpt-5.2-medium".to_string(),
        ..config_for(&server)
    };
    let user_only = vec![ChatMessage::User("hi".to_string())];

    let events = expect_ok(collect(user_only.clone(), vec![shell_tool()], config.clone()).await);

    first_mock.assert_async().await;
    assert_eq!(
        events,
        vec![
            AgentEvent::ReasoningDelta("Plan.".to_string()),
            AgentEvent::ReplayItem(json!({
                "type": "reasoning",
                "summary": [{ "type": "summary_text", "text": "Plan." }],
                "encrypted_content": "enc_1",
            })),
            AgentEvent::ToolCall(ToolCallRequest {
                id: "call_1".to_string(),
                name: "run_shell_command".to_string(),
                arguments: json!({ "command": "ls" }),
            }),
            AgentEvent::Usage {
                input_tokens: 12,
                output_tokens: 3,
            },
            AgentEvent::Done,
        ]
    );

    let follow_up = follow_up_transcript(user_only, &events);
    let second = expect_ok(collect(follow_up, vec![shell_tool()], config).await);

    follow_up_mock.assert_async().await;
    assert_eq!(second.last(), Some(&AgentEvent::Done));
}

#[tokio::test]
async fn complete_text_works_against_a_responses_endpoint() {
    let mut server = Server::new_async().await;
    let mock = responses_mock(&mut server)
        .match_body(Matcher::PartialJson(
            json!({ "model": "test-model", "stream": true }),
        ))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_body(&[
            text_delta("git "),
            text_delta("status"),
            completed(json!([])),
        ]))
        .create_async()
        .await;

    let text = complete_text(user_transcript(), config_for(&server))
        .await
        .expect("completion should succeed");

    mock.assert_async().await;
    assert_eq!(text, "git status");
}

#[tokio::test]
async fn maps_401_to_invalid_api_key_without_retrying() {
    let mut server = Server::new_async().await;
    let mock = responses_mock(&mut server)
        .with_status(401)
        .with_body(r#"{"error":{"message":"Incorrect API key provided"}}"#)
        .expect(1)
        .create_async()
        .await;

    let results = collect(user_transcript(), vec![], config_for(&server)).await;

    mock.assert_async().await;
    assert_eq!(results.len(), 1);
    assert!(
        matches!(
            &results[0],
            Err(LocalAgentError::InvalidApiKey { model_slug }) if model_slug == "test-model"
        ),
        "{results:?}"
    );
}

#[tokio::test]
async fn maps_context_overflow_400_to_context_window_exceeded() {
    let mut server = Server::new_async().await;
    let _mock = responses_mock(&mut server)
        .with_status(400)
        .with_body(r#"{"error":{"message":"Your input exceeds the context window of this model.","code":"context_length_exceeded"}}"#)
        .create_async()
        .await;

    let results = collect(user_transcript(), vec![], config_for(&server)).await;

    assert!(
        matches!(
            &results[..],
            [Err(LocalAgentError::ContextWindowExceeded { .. })]
        ),
        "{results:?}"
    );
}

#[tokio::test]
async fn a_stream_ending_before_completion_is_a_transport_error_and_never_done() {
    let mut server = Server::new_async().await;
    let mock = responses_mock(&mut server)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_body(&[text_delta("partial")]))
        // Output was already yielded, so this must not be retried.
        .expect(1)
        .create_async()
        .await;

    let results = collect(user_transcript(), vec![], config_for(&server)).await;

    mock.assert_async().await;
    assert_eq!(
        results[0]
            .as_ref()
            .expect("first event should be the delta"),
        &AgentEvent::TextDelta("partial".to_string())
    );
    assert!(
        matches!(results.last(), Some(Err(LocalAgentError::Transport(_)))),
        "{results:?}"
    );
    assert!(
        !results
            .iter()
            .any(|result| matches!(result, Ok(AgentEvent::Done)))
    );
}

#[tokio::test]
async fn a_failed_response_after_output_is_surfaced_not_retried() {
    let mut server = Server::new_async().await;
    let mock = responses_mock(&mut server)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_body(&[
            text_delta("partial"),
            json!({
                "type": "response.failed",
                "response": { "error": { "code": "server_error", "message": "boom" } },
            }),
        ]))
        .expect(1)
        .create_async()
        .await;

    let results = collect(user_transcript(), vec![], config_for(&server)).await;

    mock.assert_async().await;
    assert!(
        matches!(
            results.last(),
            Some(Err(LocalAgentError::ErrorStatus { status: 500, .. }))
        ),
        "{results:?}"
    );
}

async fn run_with_policy(
    server: &ServerGuard,
    policy: RetryPolicy,
) -> Vec<Result<AgentEvent, LocalAgentError>> {
    run_turn_with_policy(user_transcript(), vec![], config_for(server), policy)
        .collect()
        .await
}

#[tokio::test]
async fn retries_a_transient_status_before_any_event() {
    let mut server = Server::new_async().await;
    let unavailable = responses_mock(&mut server)
        .with_status(503)
        .with_body("upstream unavailable")
        .expect(1)
        .create_async()
        .await;
    let ok = responses_mock(&mut server)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(text_response("ok"))
        .expect(1)
        .create_async()
        .await;

    let events = expect_ok(run_with_policy(&server, fast_policy()).await);

    unavailable.assert_async().await;
    ok.assert_async().await;
    assert_eq!(
        events.first(),
        Some(&AgentEvent::TextDelta("ok".to_string()))
    );
    assert_eq!(events.last(), Some(&AgentEvent::Done));
}

#[tokio::test]
async fn retries_a_stream_that_closes_before_sending_anything() {
    let mut server = Server::new_async().await;
    let empty = responses_mock(&mut server)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body("")
        .expect(1)
        .create_async()
        .await;
    let ok = responses_mock(&mut server)
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(text_response("ok"))
        .expect(1)
        .create_async()
        .await;

    let events = expect_ok(run_with_policy(&server, fast_policy()).await);

    empty.assert_async().await;
    ok.assert_async().await;
    assert_eq!(events.last(), Some(&AgentEvent::Done));
}

#[tokio::test]
async fn gives_up_after_the_last_attempt_with_the_final_error() {
    let mut server = Server::new_async().await;
    let policy = fast_policy();
    let mock = responses_mock(&mut server)
        .with_status(503)
        .with_body("still down")
        .expect(policy.max_attempts)
        .create_async()
        .await;

    let results = run_with_policy(&server, policy).await;

    mock.assert_async().await;
    assert!(
        matches!(&results[..], [Err(LocalAgentError::ErrorStatus { status: 503, body })] if body == "still down"),
        "{results:?}"
    );
}

#[tokio::test]
async fn permanent_errors_are_not_retried() {
    let mut server = Server::new_async().await;
    let mock = responses_mock(&mut server)
        .with_status(404)
        .with_body("no such model")
        .expect(1)
        .create_async()
        .await;

    let results = run_with_policy(&server, fast_policy()).await;

    mock.assert_async().await;
    assert!(matches!(
        &results[..],
        [Err(LocalAgentError::ErrorStatus { status: 404, .. })]
    ));
}

#[tokio::test]
async fn dropping_the_stream_during_retry_backoff_sends_no_further_request() {
    const BACKOFF: Duration = Duration::from_millis(500);
    let mut server = Server::new_async().await;
    let mock = responses_mock(&mut server)
        .with_status(503)
        .expect(1)
        .create_async()
        .await;
    let policy = RetryPolicy {
        initial_backoff: BACKOFF,
        max_backoff: BACKOFF,
        jitter_fraction: 0.0,
        ..RetryPolicy::default()
    };
    let mut stream = run_turn_with_policy(user_transcript(), vec![], config_for(&server), policy);

    // The first attempt fails at once, leaving the engine asleep in backoff.
    let polled = timeout(BACKOFF / 2, stream.next()).await;
    assert!(
        polled.is_err(),
        "the stream should be waiting out the backoff"
    );
    mock.assert_async().await;
    drop(stream);

    // Were the sleep still running, the retry would land in this window.
    tokio::time::sleep(BACKOFF * 2).await;
    mock.assert_async().await;
}

/// A raw HTTP/1.1 server that accepts one connection and then goes silent,
/// reporting when the client closes the connection.
struct StalledServer {
    base_url: String,
    request_received: oneshot::Receiver<()>,
    connection_closed: oneshot::Receiver<()>,
}

/// Where the server stalls: before answering at all, or after the first event
/// of an otherwise healthy response.
#[derive(Clone, Copy)]
enum Stall {
    BeforeResponse,
    AfterFirstEvent,
}

async fn stalled_server(stall: Stall) -> StalledServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let base_url = format!("http://{}", listener.local_addr().expect("local addr"));
    let (request_tx, request_received) = oneshot::channel();
    let (closed_tx, connection_closed) = oneshot::channel();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = vec![0u8; 16 * 1024];
        let mut head = Vec::new();
        while !head.windows(4).any(|window| window == b"\r\n\r\n") {
            match socket.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(read) => head.extend_from_slice(&buffer[..read]),
            }
        }
        let _ = request_tx.send(());
        if matches!(stall, Stall::AfterFirstEvent) {
            let event = sse_event(&text_delta("first"));
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n{event}\r\n",
                event.len()
            );
            if socket.write_all(response.as_bytes()).await.is_err() {
                return;
            }
        }
        // Silence from here on; a read of 0 bytes or an error means the
        // client hung up.
        loop {
            match socket.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = closed_tx.send(());
    });
    StalledServer {
        base_url,
        request_received,
        connection_closed,
    }
}

/// The control for the drop assertions below: a live stream keeps its
/// connection open, so a close afterwards is caused by the drop.
async fn assert_stays_open(connection_closed: &mut oneshot::Receiver<()>) {
    assert!(
        timeout(Duration::from_millis(200), connection_closed)
            .await
            .is_err(),
        "the connection must stay open while the stream is alive"
    );
}

async fn assert_closes(connection_closed: oneshot::Receiver<()>) {
    timeout(PROMPT, connection_closed)
        .await
        .expect("dropping the stream should close the connection")
        .expect("server task should be alive");
}

#[tokio::test]
async fn dropping_the_stream_aborts_a_request_the_server_never_answers() {
    let server = stalled_server(Stall::BeforeResponse).await;
    let mut stream = crate::run_turn(user_transcript(), vec![], config_for_url(server.base_url));

    // Drive the request out; the server reads it and says nothing.
    let polled = timeout(Duration::from_millis(300), stream.next()).await;
    assert!(polled.is_err(), "no event can arrive from a silent server");
    timeout(PROMPT, server.request_received)
        .await
        .expect("the server should have seen the request")
        .expect("server task should be alive");
    let mut connection_closed = server.connection_closed;
    assert_stays_open(&mut connection_closed).await;
    drop(stream);

    assert_closes(connection_closed).await;
}

#[tokio::test]
async fn dropping_the_stream_aborts_a_response_that_stalls_midway() {
    let server = stalled_server(Stall::AfterFirstEvent).await;
    let mut stream = crate::run_turn(user_transcript(), vec![], config_for_url(server.base_url));

    let first = timeout(PROMPT, stream.next())
        .await
        .expect("the first event should arrive")
        .expect("the stream should not be empty")
        .expect("the first event should be Ok");
    assert_eq!(first, AgentEvent::TextDelta("first".to_string()));
    // The server now stalls; the engine is parked waiting for more.
    let polled = timeout(Duration::from_millis(300), stream.next()).await;
    assert!(polled.is_err(), "the stream should be stalled");
    let mut connection_closed = server.connection_closed;
    assert_stays_open(&mut connection_closed).await;
    drop(stream);

    assert_closes(connection_closed).await;
}
