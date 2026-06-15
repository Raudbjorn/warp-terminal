use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    response::sse::{Event, Sse},
    routing::{get, post},
};
use futures_util::stream;
use serde_json::{Value, json};
use tokio::net::TcpListener;

use super::*;
use crate::config::{Config, LogLevel};

const MODEL: &str = "diffusiongemma-26B-A4B-it";
const PUZZLE_PROMPT: &str = "Can you Crack a 4 digit Code? I will give you 5 Hints:\n\
1st Hint:\n\
9 2 8 5\n\
One number is correct but not in the correct position.\n\
2nd Hint:\n\
1 9 3 7\n\
Two numbers are correct but both are not in the correct position.\n\
3rd Hint:\n\
5 2 0 1\n\
One number is correct and placed correctly.\n\
4th Hint:\n\
6 5 0 7\n\
No number is correct.\n\
5th Hint:\n\
8 5 2 4\n\
Two numbers are correct but both are misplaced.";

#[tokio::test]
async fn falls_back_to_reasoning_delta_when_stream_has_no_content() {
    let (base_url, request_bodies) = spawn_openai_compatible_server(vec![
        json!({
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "" }
            }]
        }),
        json!({
            "choices": [{
                "index": 0,
                "delta": { "reasoning": "The code is 3841." },
                "finish_reason": "length"
            }]
        }),
    ])
    .await;

    let streamed_chunks = Arc::new(Mutex::new(Vec::new()));
    let response = ProviderRuntime::new()
        .stream_chat_completion(&test_config(base_url), test_params(true), {
            let streamed_chunks = streamed_chunks.clone();
            move |chunk| streamed_chunks.lock().unwrap().push(chunk)
        })
        .await
        .unwrap();

    assert_eq!(response.content, "The code is 3841.");
    assert_eq!(streamed_chunks.lock().unwrap().join(""), response.content);

    let request_bodies = request_bodies.lock().unwrap();
    assert_eq!(request_bodies.len(), 1);
    let request_body = &request_bodies[0];
    assert_eq!(request_body["model"], MODEL);
    assert_eq!(request_body["stream"], true);
    assert_eq!(request_body["max_tokens"], 2048);
    assert!(
        request_body["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty())
    );
}

#[tokio::test]
async fn ignores_reasoning_delta_when_content_streams() {
    let (base_url, _request_bodies) = spawn_openai_compatible_server(vec![
        json!({
            "choices": [{
                "index": 0,
                "delta": { "reasoning": "internal scratchpad" }
            }]
        }),
        json!({
            "choices": [{
                "index": 0,
                "delta": { "content": "Final answer." }
            }]
        }),
    ])
    .await;

    let streamed_chunks = Arc::new(Mutex::new(Vec::new()));
    let response = ProviderRuntime::new()
        .stream_chat_completion(&test_config(base_url), test_params(false), {
            let streamed_chunks = streamed_chunks.clone();
            move |chunk| streamed_chunks.lock().unwrap().push(chunk)
        })
        .await
        .unwrap();

    assert_eq!(response.content, "Final answer.");
    assert_eq!(streamed_chunks.lock().unwrap().join(""), "Final answer.");
}

#[tokio::test]
async fn suppresses_harmony_thought_channel_content() {
    let (base_url, _request_bodies) = spawn_openai_compatible_server(vec![
        json!({
            "choices": [{
                "index": 0,
                "delta": { "content": "<|chan" }
            }]
        }),
        json!({
            "choices": [{
                "index": 0,
                "delta": { "content": "nel>thought\ninternal scratchpad\n<|channel>final\n" }
            }]
        }),
        json!({
            "choices": [{
                "index": 0,
                "delta": { "content": "Public answer." }
            }]
        }),
    ])
    .await;

    let streamed_chunks = Arc::new(Mutex::new(Vec::new()));
    let response = ProviderRuntime::new()
        .stream_chat_completion(&test_config(base_url), test_params(false), {
            let streamed_chunks = streamed_chunks.clone();
            move |chunk| streamed_chunks.lock().unwrap().push(chunk)
        })
        .await
        .unwrap();

    assert_eq!(response.content, "Public answer.");
    assert_eq!(streamed_chunks.lock().unwrap().join(""), "Public answer.");
    assert!(response.suppressed_internal_content);
}

#[tokio::test]
async fn parses_embedded_harmony_tool_call_content() {
    let (base_url, _request_bodies) = spawn_openai_compatible_server(vec![json!({
        "choices": [{
            "index": 0,
            "delta": {
                "content": "<|channel>thought\nNeed search.<|tool_call>call:call_mcp_tool{args:{query:<|\"|>what is btop<|\"|>},name:<|\"|>tavily-tavily_search<|\"|>}<tool_call|>"
            }
        }]
    })])
    .await;

    let streamed_chunks = Arc::new(Mutex::new(Vec::new()));
    let response = ProviderRuntime::new()
        .stream_chat_completion(&test_config(base_url), test_params(true), {
            let streamed_chunks = streamed_chunks.clone();
            move |chunk| streamed_chunks.lock().unwrap().push(chunk)
        })
        .await
        .unwrap();

    assert_eq!(response.content, "");
    assert!(streamed_chunks.lock().unwrap().is_empty());
    assert!(response.suppressed_internal_content);
    assert_eq!(response.tool_calls.len(), 1);
    let tool_call = &response.tool_calls[0];
    assert_eq!(tool_call.name, "call_mcp_tool");
    assert_eq!(
        serde_json::from_str::<Value>(&tool_call.arguments_text).unwrap(),
        json!({
            "args": { "query": "what is btop" },
            "name": "tavily-tavily_search"
        })
    );
}

#[test]
fn parses_harmony_tool_call_string_with_unescaped_quotes() {
    let tool_call = parse_harmony_tool_call_payload(
        r#"call:run_shell_command{command:<|"|>grep "boto3" s3_report_workflow.py<|"|>}"#,
    )
    .unwrap();

    assert_eq!(tool_call.name, "run_shell_command");
    assert_eq!(
        serde_json::from_str::<Value>(&tool_call.arguments_text).unwrap(),
        json!({ "command": r#"grep "boto3" s3_report_workflow.py"# })
    );
}

#[tokio::test]
async fn command_autocomplete_uses_alias_without_tools() {
    let (base_url, request_bodies) = spawn_openai_compatible_server(vec![json!({
        "choices": [{
            "index": 0,
            "delta": { "content": "git checkout feature/local-autocomplete" }
        }]
    })])
    .await;

    let request = crate::autocomplete::LocalCommandAutocompleteRequest {
        prefix: "git checkout ".to_string(),
        cwd: Some("/repo".to_string()),
        shell: Some("zsh".to_string()),
        file_candidates: vec!["Cargo.toml".to_string()],
        ..Default::default()
    };

    let mut config = test_config(base_url);
    config.local_model_aliases = Some(format!(r#"{{"auto-autocomplete":"{MODEL}"}}"#));

    let response = ProviderRuntime::new()
        .command_autocomplete(&config, &request, None, None, None)
        .await
        .unwrap();

    assert_eq!(
        response.most_likely_action,
        "git checkout feature/local-autocomplete"
    );

    let request_bodies = request_bodies.lock().unwrap();
    assert_eq!(request_bodies.len(), 1);
    let request_body = &request_bodies[0];
    assert_eq!(request_body["model"], MODEL);
    assert_eq!(request_body["stream"], true);
    assert_eq!(request_body["max_tokens"], 256);
    assert_eq!(request_body["temperature"], 0.0);
    assert!(request_body.get("tools").is_none());
    assert!(
        request_body["messages"]
            .to_string()
            .contains("git checkout ")
    );
}

#[tokio::test]
async fn command_autocomplete_uses_request_alias_override() {
    let (base_url, request_bodies) = spawn_openai_compatible_server(vec![json!({
        "choices": [{
            "index": 0,
            "delta": { "content": "git status" }
        }]
    })])
    .await;

    let request = crate::autocomplete::LocalCommandAutocompleteRequest {
        prefix: "git ".to_string(),
        ..Default::default()
    };

    let mut config = test_config(base_url);
    config.local_model_aliases = Some(r#"{"auto-autocomplete":"config/model"}"#.to_string());

    let response = ProviderRuntime::new()
        .command_autocomplete(
            &config,
            &request,
            None,
            None,
            Some(r#"{"auto-autocomplete":"request/model"}"#.to_string()),
        )
        .await
        .unwrap();

    assert_eq!(response.most_likely_action, "git status");
    let request_bodies = request_bodies.lock().unwrap();
    assert_eq!(request_bodies[0]["model"], "request/model");
}

#[tokio::test]
async fn command_autocomplete_short_circuits_docker_ps_context() {
    let (base_url, request_bodies) = spawn_openai_compatible_server(Vec::new()).await;
    let request = crate::autocomplete::LocalCommandAutocompleteRequest {
        prefix: "docker logs -f e".to_string(),
        recent_blocks: vec![crate::autocomplete::AutocompleteBlockContext {
            command: "docker ps".to_string(),
            output: Some(
                "CONTAINER ID   IMAGE                            COMMAND                  CREATED       STATUS        PORTS     NAMES\n\
5a8038634ee7   rhasspy/wyoming-whisper          \"bash docker_run.sh\"    6 weeks ago   Up 13 min              wyoming-whisper\n\
eb48dced80cd   supabase/edge-runtime:v1.69.28   \"edge-runtime start\"    5 months ago  Up 13 min              supabase-edge-functions"
                    .to_string(),
            ),
            ..Default::default()
        }],
        ..Default::default()
    };

    let response = ProviderRuntime::new()
        .command_autocomplete(&test_config(base_url), &request, None, None, None)
        .await
        .unwrap();

    assert_eq!(response.most_likely_action, "docker logs -f eb48dced80cd");
    assert!(response.raw_output.is_empty());
    assert!(request_bodies.lock().unwrap().is_empty());
}

async fn spawn_openai_compatible_server(chunks: Vec<Value>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let request_bodies = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/v1/models",
            get(|| async {
                Json(json!({
                    "object": "list",
                    "data": [{
                        "id": MODEL,
                        "object": "model",
                        "max_model_len": 262144
                    }]
                }))
            }),
        )
        .route(
            "/v1/chat/completions",
            post({
                let request_bodies = request_bodies.clone();
                move |Json(body): Json<Value>| {
                    let chunks = chunks.clone();
                    let request_bodies = request_bodies.clone();
                    async move {
                        request_bodies.lock().unwrap().push(body);
                        let events = chunks
                            .into_iter()
                            .map(|chunk| {
                                Ok::<_, Infallible>(Event::default().data(chunk.to_string()))
                            })
                            .chain(std::iter::once(Ok(Event::default().data("[DONE]"))));
                        Sse::new(stream::iter(events))
                    }
                }
            }),
        );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/v1"), request_bodies)
}

fn test_config(base_url: String) -> Config {
    Config {
        host: "127.0.0.1".to_owned(),
        port: 0,
        openai_api_key: Some("test-key".to_owned()),
        openai_base_url: Some(base_url),
        openai_model: None,
        local_model_aliases: None,
        local_model_list: MODEL.to_owned(),
        local_enable_tools: true,
        local_max_history_messages: 80,
        local_max_completion_tokens: 2048,
        local_model_context_tokens: None,
        local_graphql_db_path: ":memory:".to_owned(),
        local_service_log_path: None,
        log_level: LogLevel::Error,
        local_multi_agent_system_prompt: None,
        local_config_hash: None,
        warp_url_scheme: "warp".to_owned(),
    }
}

fn test_params(enable_tools: bool) -> ChatCompletionParams {
    ChatCompletionParams {
        messages: vec![user_text_message(PUZZLE_PROMPT)],
        api_key: None,
        base_url: None,
        local_model_aliases: None,
        model: Some(MODEL.to_owned()),
        max_tokens: None,
        temperature: None,
        mcp_tools: Vec::new(),
        enable_tools,
    }
}
