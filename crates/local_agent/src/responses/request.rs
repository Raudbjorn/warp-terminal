//! Responses API request building: endpoint URL, `input` items, and the
//! stateless-replay request parameters.

use serde::Serialize;
use serde_json::{Value, json};

use super::model::{self, ReasoningConfig};
use super::tools;
use crate::{ChatMessage, LocalEndpointConfig, ToolCallRequest, ToolDefinition};

/// Extra output the provider must return so reasoning items can be replayed
/// on the next stateless (`store: false`) turn.
const REASONING_INCLUDE: &[&str] = &["reasoning.encrypted_content"];

/// Header mirroring `prompt_cache_key`, which some Responses gateways use to
/// route requests of one session to the same cache.
pub(super) const SESSION_ID_HEADER: &str = "Session_id";

const PROMPT_CACHE_KEY_PREFIX: &str = "warp-local-";

const FNV64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV64_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Resolves the Responses URL for an endpoint. Users configure a base URL,
/// the full `/responses` URL, or a Chat Completions URL left over from
/// another schema. As with the Chat engine, `/responses` is appended to the
/// base path as given: `https://host/v1` and `https://host/openai/v1` gain
/// only `/responses`, and a gateway path such as
/// `https://gateway.example/v1/acct/openai` is never rewritten. Only a bare
/// host (`https://host`) gets the `/v1` that OpenAI-style servers expect.
/// A query string or fragment (e.g. Azure's `?api-version=...`) rides along
/// after the path untouched.
pub(crate) fn endpoint_url(base_url: &str) -> String {
    let base_url = base_url.trim();
    let (location, suffix) = base_url.split_at(base_url.find(['?', '#']).unwrap_or(base_url.len()));
    let (origin, path) = split_origin(location.trim_end_matches('/'));
    if let Some(chat_base) = path.strip_suffix("/chat/completions") {
        format!("{origin}{chat_base}/responses{suffix}")
    } else if path.is_empty() {
        format!("{origin}/v1/responses{suffix}")
    } else if path.ends_with("/responses") {
        format!("{origin}{path}{suffix}")
    } else {
        format!("{origin}{path}/responses{suffix}")
    }
}

/// Splits `scheme://authority` from the path that follows it.
fn split_origin(url: &str) -> (&str, &str) {
    let authority_start = url.find("://").map_or(0, |scheme_end| scheme_end + 3);
    match url[authority_start..].find('/') {
        Some(path_start) => url.split_at(authority_start + path_start),
        None => (url, ""),
    }
}

#[derive(Serialize)]
pub(crate) struct ResponsesRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    include: &'static [&'static str],
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    input: Vec<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    // Both are rejected by the API on a request without tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    /// Always `false` when tools are present: the host executes a turn's tool
    /// calls one at a time, and a second `run_shell_command` issued in the same
    /// response would be cancelled by its serial command executor.
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    /// Stateless: history is replayed from the transcript on every turn.
    store: bool,
    stream: bool,
}

impl ResponsesRequest {
    /// The `Session_id` header value, mirroring `prompt_cache_key`.
    pub(super) fn session_id(&self) -> Option<&str> {
        self.prompt_cache_key.as_deref()
    }
}

pub(crate) fn build_request(
    transcript: &[ChatMessage],
    tool_definitions: &[ToolDefinition],
    config: &LocalEndpointConfig,
) -> ResponsesRequest {
    let resolved = model::resolve_model(&config.model_slug);
    let (instructions, input) = split_instructions(transcript);
    let has_tools = !tool_definitions.is_empty();
    ResponsesRequest {
        model: resolved.model,
        reasoning: resolved.reasoning,
        prompt_cache_key: prompt_cache_key(transcript),
        include: if resolved.is_reasoning_model {
            REASONING_INCLUDE
        } else {
            &[]
        },
        instructions,
        input,
        tools: tool_definitions.iter().map(tools::function_tool).collect(),
        tool_choice: has_tools.then_some("auto"),
        parallel_tool_calls: has_tools.then_some(false),
        store: false,
        stream: true,
    }
}

/// Leading system messages become `instructions`; the rest become `input`
/// items, with any later system message kept in place as a `system` item.
fn split_instructions(transcript: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
    let leading = transcript
        .iter()
        .take_while(|message| matches!(message, ChatMessage::System(_)))
        .count();
    let instructions = transcript[..leading]
        .iter()
        .filter_map(|message| match message {
            ChatMessage::System(text) if !text.is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let input = transcript[leading..].iter().flat_map(input_items).collect();
    ((!instructions.is_empty()).then_some(instructions), input)
}

fn input_items(message: &ChatMessage) -> Vec<Value> {
    match message {
        ChatMessage::System(text) => vec![text_message("system", "input_text", text)],
        ChatMessage::User(text) => vec![text_message("user", "input_text", text)],
        ChatMessage::Assistant {
            text,
            tool_calls,
            replay_items,
        } => {
            // Output order of the original response: reasoning, text, calls.
            let mut items = replay_items.clone();
            if !text.is_empty() {
                items.push(assistant_message(text));
            }
            items.extend(tool_calls.iter().map(function_call_item));
            items
        }
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => vec![json!({
            "type": "function_call_output",
            "call_id": tool_call_id,
            "output": content,
        })],
    }
}

fn text_message(role: &str, content_type: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": [{ "type": content_type, "text": text }],
    })
}

fn assistant_message(text: &str) -> Value {
    json!({
        "type": "message",
        "role": "assistant",
        "content": [{ "type": "output_text", "text": text, "annotations": [] }],
    })
}

fn function_call_item(call: &ToolCallRequest) -> Value {
    json!({
        "type": "function_call",
        "call_id": call.id,
        "name": call.name,
        "arguments": call.arguments.to_string(),
    })
}

/// Derives a cache-routing key that stays stable for the life of a
/// conversation: a hash of its first user message, which every later turn's
/// transcript still starts with. The system prompt is deliberately left out
/// because it embeds environment details that change between turns.
fn prompt_cache_key(transcript: &[ChatMessage]) -> Option<String> {
    let first_user_text = transcript.iter().find_map(|message| match message {
        ChatMessage::User(text) => Some(text),
        _ => None,
    })?;
    Some(format!(
        "{PROMPT_CACHE_KEY_PREFIX}{:016x}",
        fnv1a_64(first_user_text.as_bytes())
    ))
}

/// FNV-1a, hand-rolled because the key must stay stable across Rust releases
/// (`DefaultHasher` makes no such promise).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV64_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV64_PRIME)
    })
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
