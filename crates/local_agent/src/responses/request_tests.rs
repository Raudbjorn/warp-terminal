use serde_json::json;

use super::*;
use crate::EndpointSchema;

fn config(model_slug: &str) -> LocalEndpointConfig {
    LocalEndpointConfig {
        base_url: "https://example.com".to_string(),
        api_key: "key".to_string(),
        model_slug: model_slug.to_string(),
        schema: EndpointSchema::Responses,
    }
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

fn body(transcript: &[ChatMessage], tools: &[ToolDefinition], slug: &str) -> Value {
    serde_json::to_value(build_request(transcript, tools, &config(slug)))
        .expect("request should serialize")
}

fn simple_transcript() -> Vec<ChatMessage> {
    vec![
        ChatMessage::System("You are a coding agent.".to_string()),
        ChatMessage::User("hi".to_string()),
    ]
}

#[test]
fn endpoint_url_handles_base_urls_with_and_without_a_version() {
    for (base, expected) in [
        (
            "https://api.openai.com",
            "https://api.openai.com/v1/responses",
        ),
        (
            "https://api.openai.com/",
            "https://api.openai.com/v1/responses",
        ),
        ("https://example.com/v1", "https://example.com/v1/responses"),
        (
            "https://example.com/v1/",
            "https://example.com/v1/responses",
        ),
        (
            "https://example.com/v4/",
            "https://example.com/v4/responses",
        ),
        (
            "https://example.com/openai",
            "https://example.com/openai/v1/responses",
        ),
        (
            "https://example.com/openai/v1",
            "https://example.com/openai/v1/responses",
        ),
        (
            "  https://example.com/v1  ",
            "https://example.com/v1/responses",
        ),
    ] {
        assert_eq!(endpoint_url(base), expected, "base {base:?}");
    }
}

#[test]
fn endpoint_url_keeps_a_full_responses_url_and_converts_a_chat_url() {
    for (base, expected) in [
        (
            "https://example.com/v1/responses",
            "https://example.com/v1/responses",
        ),
        (
            "https://example.com/v1/responses/",
            "https://example.com/v1/responses",
        ),
        (
            "https://example.com/responses",
            "https://example.com/responses",
        ),
        (
            "https://example.com/v1/chat/completions",
            "https://example.com/v1/responses",
        ),
    ] {
        assert_eq!(endpoint_url(base), expected, "base {base:?}");
    }
}

#[test]
fn a_plain_request_is_stateless_and_streaming_without_tool_parameters() {
    let body = body(&simple_transcript(), &[], "llama-3.3-70b");

    assert_eq!(body["model"], "llama-3.3-70b");
    assert_eq!(body["instructions"], "You are a coding agent.");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(
        body["input"],
        json!([{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": "hi" }],
        }])
    );
    // The API rejects these on a request without tools, and reasoning
    // parameters on a model that is not known to support them.
    for absent in [
        "reasoning",
        "include",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
    ] {
        assert!(body.get(absent).is_none(), "{absent} should be omitted");
    }
}

#[test]
fn tools_are_flat_function_tools_with_parallel_calls_disabled() {
    let body = body(&simple_transcript(), &[shell_tool()], "llama-3.3-70b");

    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(
        body["tools"],
        json!([{
            "type": "function",
            "name": "run_shell_command",
            "description": "Run a shell command",
            "parameters": {
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"],
            },
            "strict": false,
        }])
    );
}

#[test]
fn an_effort_suffixed_slug_becomes_a_model_plus_reasoning_config() {
    let body = body(&simple_transcript(), &[], "gpt-5.3-codex-high");

    assert_eq!(body["model"], "gpt-5.3-codex");
    assert_eq!(
        body["reasoning"],
        json!({ "effort": "high", "summary": "auto" })
    );
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
}

#[test]
fn a_known_reasoning_model_without_a_suffix_replays_reasoning_but_sets_no_effort() {
    let body = body(&simple_transcript(), &[], "gpt-5.2");

    assert_eq!(body["model"], "gpt-5.2");
    assert!(body.get("reasoning").is_none());
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
}

#[test]
fn assistant_turns_replay_reasoning_then_text_then_calls() {
    let reasoning = json!({
        "type": "reasoning",
        "summary": [],
        "encrypted_content": "enc",
    });
    let transcript = vec![
        ChatMessage::User("list files".to_string()),
        ChatMessage::Assistant {
            text: "Looking.".to_string(),
            tool_calls: vec![ToolCallRequest {
                id: "call_1".to_string(),
                name: "run_shell_command".to_string(),
                arguments: json!({ "command": "ls" }),
            }],
            replay_items: vec![reasoning.clone()],
        },
        ChatMessage::Tool {
            tool_call_id: "call_1".to_string(),
            content: "file.txt".to_string(),
        },
    ];

    let body = body(&transcript, &[shell_tool()], "gpt-5.2");

    assert_eq!(
        body["input"],
        json!([
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "list files" }],
            },
            reasoning,
            {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "Looking.", "annotations": [] }],
            },
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "run_shell_command",
                "arguments": "{\"command\":\"ls\"}",
            },
            { "type": "function_call_output", "call_id": "call_1", "output": "file.txt" },
        ])
    );
}

#[test]
fn an_assistant_turn_with_only_tool_calls_has_no_empty_message() {
    let transcript = vec![ChatMessage::Assistant {
        text: String::new(),
        tool_calls: vec![ToolCallRequest {
            id: "call_1".to_string(),
            name: "t".to_string(),
            arguments: json!({}),
        }],
        replay_items: vec![],
    }];

    let body = body(&transcript, &[], "m");

    assert_eq!(body["input"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["input"][0]["type"], "function_call");
}

#[test]
fn only_leading_system_messages_become_instructions() {
    let transcript = vec![
        ChatMessage::System("rules".to_string()),
        ChatMessage::System("more rules".to_string()),
        ChatMessage::User("hi".to_string()),
        ChatMessage::System("late note".to_string()),
    ];

    let body = body(&transcript, &[], "m");

    assert_eq!(body["instructions"], "rules\n\nmore rules");
    assert_eq!(
        body["input"][1],
        json!({
            "type": "message",
            "role": "system",
            "content": [{ "type": "input_text", "text": "late note" }],
        })
    );
}

#[test]
fn a_transcript_without_system_messages_sends_no_instructions() {
    let body = body(&[ChatMessage::User("hi".to_string())], &[], "m");

    assert!(body.get("instructions").is_none());
}

#[test]
fn prompt_cache_key_is_stable_across_turns_of_one_conversation() {
    let first_turn = simple_transcript();
    let mut later_turn = vec![ChatMessage::System("a different system prompt".to_string())];
    later_turn.extend(first_turn[1..].iter().cloned());
    later_turn.push(ChatMessage::Assistant {
        text: "hello".to_string(),
        tool_calls: vec![],
        replay_items: vec![],
    });
    later_turn.push(ChatMessage::User("and again".to_string()));

    let first = body(&first_turn, &[], "m");
    let later = body(&later_turn, &[], "m");
    let other = body(&[ChatMessage::User("something else".to_string())], &[], "m");

    let key = first["prompt_cache_key"]
        .as_str()
        .expect("key should be set");
    assert!(key.starts_with("warp-local-"));
    assert_eq!(later["prompt_cache_key"], key);
    assert_ne!(other["prompt_cache_key"], key);
}

#[test]
fn prompt_cache_key_is_absent_without_a_user_message() {
    let body = body(&[ChatMessage::System("rules".to_string())], &[], "m");

    assert!(body.get("prompt_cache_key").is_none());
}

#[test]
fn session_id_mirrors_the_prompt_cache_key() {
    let request = build_request(&simple_transcript(), &[], &config("m"));

    assert_eq!(
        request.session_id().map(str::to_string),
        serde_json::to_value(&request).expect("request should serialize")["prompt_cache_key"]
            .as_str()
            .map(str::to_string)
    );
}

#[test]
fn fnv1a_matches_the_reference_vectors() {
    assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
}
