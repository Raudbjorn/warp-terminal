//! Folds Responses API server-sent events into neutral [`AgentEvent`]s.
//!
//! Text and reasoning-summary deltas pass through immediately. Reasoning
//! items are emitted as [`AgentEvent::ReplayItem`] the moment they complete.
//! Tool calls accumulate (their metadata and arguments arrive in separate
//! events) and are emitted together, in output order, once the response
//! completes, which also guarantees they follow every replay item.
//!
//! Everything the stream announced is reconciled against the `output` of the
//! final `response.completed` payload, so providers that skip the incremental
//! events still produce the same result.

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use crate::{AgentEvent, LocalAgentError, LocalEndpointConfig, ToolCallRequest, openai};

/// `incomplete_details.reason` of a response cut off by the output token
/// limit. Treated like the Chat Completions `length` finish reason: a normal
/// end of turn rather than an error.
const MAX_OUTPUT_TOKENS_REASON: &str = "max_output_tokens";

/// How much of a detail-less failure event is echoed into the error text.
const MAX_FAILURE_EXCERPT_CHARS: usize = 200;

/// Separates the paragraphs of a multi-part reasoning summary.
const SUMMARY_PART_SEPARATOR: &str = "\n\n";

/// What one SSE message produced.
#[derive(Default)]
pub(super) struct Step {
    pub(super) events: Vec<AgentEvent>,
    /// The response is complete; the caller emits [`AgentEvent::Done`].
    pub(super) finished: bool,
}

impl Step {
    fn with_events(events: Vec<AgentEvent>) -> Self {
        Self {
            events,
            finished: false,
        }
    }
}

/// A function call being assembled from `output_item.added`,
/// `function_call_arguments.*`, and `output_item.done` events. Providers
/// disagree on which identifier each event carries, so a call is matched by
/// whichever of the three is present.
#[derive(Default)]
struct PendingCall {
    item_id: Option<String>,
    call_id: Option<String>,
    output_index: Option<u64>,
    name: Option<String>,
    arguments: String,
}

struct CallIds<'a> {
    item_id: Option<&'a str>,
    call_id: Option<&'a str>,
    output_index: Option<u64>,
}

impl<'a> CallIds<'a> {
    fn of_event(payload: &'a Value) -> Self {
        Self {
            item_id: non_empty_str(payload, "item_id"),
            call_id: non_empty_str(payload, "call_id"),
            output_index: payload.get("output_index").and_then(Value::as_u64),
        }
    }

    fn of_item(item: &'a Value, output_index: Option<u64>) -> Self {
        Self {
            item_id: non_empty_str(item, "id"),
            call_id: non_empty_str(item, "call_id"),
            output_index,
        }
    }

    fn is_empty(&self) -> bool {
        self.item_id.is_none() && self.call_id.is_none() && self.output_index.is_none()
    }

    fn matches(&self, call: &PendingCall) -> bool {
        let same = |ours: Option<&str>, theirs: &Option<String>| {
            ours.is_some() && ours == theirs.as_deref()
        };
        same(self.call_id, &call.call_id)
            || same(self.item_id, &call.item_id)
            || (self.output_index.is_some() && self.output_index == call.output_index)
    }
}

#[derive(Default)]
pub(super) struct ResponseStream {
    calls: Vec<PendingCall>,
    /// Ids of output items whose text already reached the caller as deltas.
    streamed_text: HashSet<String>,
    /// Same, for reasoning summaries.
    streamed_reasoning: HashSet<String>,
    /// Keys of reasoning items already emitted as replay items.
    replayed: HashSet<String>,
}

impl ResponseStream {
    /// Handles one SSE message. `sse_event` is the `event:` field, used only
    /// when the payload itself carries no `type`.
    pub(super) fn ingest(
        &mut self,
        sse_event: &str,
        data: &str,
        config: &LocalEndpointConfig,
    ) -> Result<Step, LocalAgentError> {
        if data.trim() == "[DONE]" {
            return Ok(Step::default());
        }
        let payload: Value = serde_json::from_str(data)
            .map_err(|err| LocalAgentError::Parse(format!("malformed event: {err}: {data}")))?;
        let kind = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(sse_event);
        match kind {
            "response.output_text.delta" | "response.text.delta" | "response.refusal.delta" => {
                Ok(Step::with_events(self.text_delta(&payload)))
            }
            "response.reasoning_summary_part.added" => {
                Ok(Step::with_events(self.summary_part_added(&payload)))
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                Ok(Step::with_events(self.reasoning_delta(&payload)))
            }
            "response.output_item.added" => {
                self.item_added(&payload);
                Ok(Step::default())
            }
            "response.function_call_arguments.delta" => {
                self.arguments_delta(&payload);
                Ok(Step::default())
            }
            "response.function_call_arguments.done" => {
                self.arguments_done(&payload);
                Ok(Step::default())
            }
            "response.output_item.done" => Ok(Step::with_events(self.item_done(&payload))),
            "response.completed" => self.complete(&payload),
            "response.incomplete" => self.incomplete(&payload),
            "response.failed" | "error" => Err(failure_error(&payload, config)),
            // created, in_progress, content_part.*, output_text.done, ...
            _ => Ok(Step::default()),
        }
    }

    fn text_delta(&mut self, payload: &Value) -> Vec<AgentEvent> {
        let Some(delta) = non_empty_str(payload, "delta") else {
            return vec![];
        };
        self.streamed_text.insert(event_item_key(payload));
        vec![AgentEvent::TextDelta(delta.to_string())]
    }

    fn reasoning_delta(&mut self, payload: &Value) -> Vec<AgentEvent> {
        let Some(delta) = non_empty_str(payload, "delta") else {
            return vec![];
        };
        self.streamed_reasoning.insert(event_item_key(payload));
        vec![AgentEvent::ReasoningDelta(delta.to_string())]
    }

    /// A summary arrives as numbered parts; keep them apart as paragraphs.
    fn summary_part_added(&self, payload: &Value) -> Vec<AgentEvent> {
        let is_later_part = payload
            .get("summary_index")
            .and_then(Value::as_u64)
            .is_some_and(|index| index > 0);
        if is_later_part && self.streamed_reasoning.contains(&event_item_key(payload)) {
            vec![AgentEvent::ReasoningDelta(
                SUMMARY_PART_SEPARATOR.to_string(),
            )]
        } else {
            vec![]
        }
    }

    fn item_added(&mut self, payload: &Value) {
        let Some(item) = payload.get("item") else {
            return;
        };
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            return;
        }
        let output_index = payload.get("output_index").and_then(Value::as_u64);
        self.absorb_function_call(item, output_index);
    }

    fn arguments_delta(&mut self, payload: &Value) {
        let Some(delta) = payload.get("delta").and_then(Value::as_str) else {
            return;
        };
        self.call_mut(&CallIds::of_event(payload))
            .arguments
            .push_str(delta);
    }

    fn arguments_done(&mut self, payload: &Value) {
        let call = self.call_mut(&CallIds::of_event(payload));
        call.name = non_empty_str(payload, "name")
            .map(str::to_string)
            .or(call.name.take());
        if let Some(arguments) = non_empty_str(payload, "arguments") {
            call.arguments = arguments.to_string();
        }
    }

    fn item_done(&mut self, payload: &Value) -> Vec<AgentEvent> {
        let Some(item) = payload.get("item") else {
            return vec![];
        };
        let output_index = payload.get("output_index").and_then(Value::as_u64);
        self.finish_item(item, output_index)
    }

    /// Reconciles one finished output item with what was streamed for it.
    fn finish_item(&mut self, item: &Value, output_index: Option<u64>) -> Vec<AgentEvent> {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                self.absorb_function_call(item, output_index);
                vec![]
            }
            Some("reasoning") => self.finish_reasoning(item),
            Some("message") => self.finish_message(item),
            _ => vec![],
        }
    }

    fn finish_reasoning(&mut self, item: &Value) -> Vec<AgentEvent> {
        let mut events = vec![];
        let key = output_item_key(item);
        if !self.streamed_reasoning.contains(&key)
            && let Some(text) = reasoning_text(item)
        {
            self.streamed_reasoning.insert(key);
            events.push(AgentEvent::ReasoningDelta(text));
        }
        if let Some(replay) = replay_item(item)
            && self.replayed.insert(replay_key(item))
        {
            events.push(AgentEvent::ReplayItem(replay));
        }
        events
    }

    fn finish_message(&mut self, item: &Value) -> Vec<AgentEvent> {
        let key = output_item_key(item);
        if self.streamed_text.contains(&key) {
            return vec![];
        }
        let text = message_text(item);
        if text.is_empty() {
            return vec![];
        }
        self.streamed_text.insert(key);
        vec![AgentEvent::TextDelta(text)]
    }

    fn absorb_function_call(&mut self, item: &Value, output_index: Option<u64>) {
        let call = self.call_mut(&CallIds::of_item(item, output_index));
        if let Some(name) = non_empty_str(item, "name") {
            call.name = Some(name.to_string());
        }
        // The streamed fragments are authoritative unless there are none.
        if call.arguments.is_empty()
            && let Some(arguments) = non_empty_str(item, "arguments")
        {
            call.arguments = arguments.to_string();
        }
    }

    /// Finds the pending call an event refers to, or starts tracking it, and
    /// records every identifier the event adds. An event carrying no
    /// identifier at all can only refer to the most recent call.
    fn call_mut(&mut self, ids: &CallIds<'_>) -> &mut PendingCall {
        let known = if ids.is_empty() {
            self.calls.len().checked_sub(1)
        } else {
            self.calls.iter().position(|call| ids.matches(call))
        };
        let index = known.unwrap_or_else(|| {
            self.calls.push(PendingCall::default());
            self.calls.len() - 1
        });
        let call = &mut self.calls[index];
        call.item_id = ids.item_id.map(str::to_string).or(call.item_id.take());
        call.call_id = ids.call_id.map(str::to_string).or(call.call_id.take());
        call.output_index = ids.output_index.or(call.output_index);
        call
    }

    fn complete(&mut self, payload: &Value) -> Result<Step, LocalAgentError> {
        let response = payload.get("response").unwrap_or(payload);
        let mut events = vec![];
        let output = response.get("output").and_then(Value::as_array);
        for (index, item) in output.into_iter().flatten().enumerate() {
            events.extend(self.finish_item(item, u64::try_from(index).ok()));
        }
        events.extend(self.flush_tool_calls()?);
        if let Some(usage) = usage_event(response) {
            events.push(usage);
        }
        Ok(Step {
            events,
            finished: true,
        })
    }

    fn incomplete(&mut self, payload: &Value) -> Result<Step, LocalAgentError> {
        let reason = payload
            .pointer("/response/incomplete_details/reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if reason == MAX_OUTPUT_TOKENS_REASON {
            log::warn!("Responses output was cut off by the output token limit");
            return self.complete(payload);
        }
        Err(LocalAgentError::ErrorStatus {
            status: 400,
            body: format!("response incomplete: {reason}"),
        })
    }

    fn flush_tool_calls(&mut self) -> Result<Vec<AgentEvent>, LocalAgentError> {
        let mut calls = std::mem::take(&mut self.calls);
        calls.sort_by_key(|call| call.output_index.unwrap_or(u64::MAX));
        calls.into_iter().map(tool_call_event).collect()
    }
}

fn tool_call_event(call: PendingCall) -> Result<AgentEvent, LocalAgentError> {
    // Some servers omit `call_id` and expect the item id to be echoed back.
    let id = call
        .call_id
        .or(call.item_id)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| LocalAgentError::Parse("tool call missing required id".to_string()))?;
    let name = call
        .name
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| {
            LocalAgentError::Parse("tool call missing required function name".to_string())
        })?;
    let arguments = if call.arguments.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(&call.arguments).map_err(|err| {
            LocalAgentError::Parse(format!("tool call {name} has malformed arguments: {err}"))
        })?
    };
    Ok(AgentEvent::ToolCall(ToolCallRequest {
        id,
        name,
        arguments,
    }))
}

fn usage_event(response: &Value) -> Option<AgentEvent> {
    let usage = response.get("usage")?;
    Some(AgentEvent::Usage {
        input_tokens: usage.get("input_tokens")?.as_u64()?,
        output_tokens: usage.get("output_tokens")?.as_u64()?,
    })
}

/// A completed reasoning item in the form the next request replays it: no
/// `id` (a `store: false` request cannot resolve one, and the API rejects it
/// as an unknown item) and an explicit `summary`, even when empty. Items
/// without `encrypted_content` carry nothing replayable.
fn replay_item(item: &Value) -> Option<Value> {
    let encrypted_content = non_empty_str(item, "encrypted_content")?;
    let mut replay = json!({
        "type": "reasoning",
        "summary": item.get("summary").filter(|summary| summary.is_array()).cloned().unwrap_or_else(|| json!([])),
        "encrypted_content": encrypted_content,
    });
    let content = item
        .get("content")
        .filter(|content| content.as_array().is_some_and(|parts| !parts.is_empty()));
    if let (Some(content), Some(object)) = (content, replay.as_object_mut()) {
        object.insert("content".to_string(), content.clone());
    }
    Some(replay)
}

/// Identifies a reasoning item across `output_item.done` and the completed
/// payload.
fn replay_key(item: &Value) -> String {
    non_empty_str(item, "id")
        .or_else(|| non_empty_str(item, "encrypted_content"))
        .unwrap_or_default()
        .to_string()
}

/// The readable text of a reasoning item: its summary, else its raw content.
fn reasoning_text(item: &Value) -> Option<String> {
    ["summary", "content"].into_iter().find_map(|field| {
        let text = item
            .get(field)
            .and_then(Value::as_array)?
            .iter()
            .filter(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("summary_text" | "reasoning_text")
                )
            })
            .filter_map(|part| non_empty_str(part, "text"))
            .collect::<Vec<_>>()
            .join(SUMMARY_PART_SEPARATOR);
        (!text.is_empty()).then_some(text)
    })
}

/// The text of an assistant message item, including refusals.
fn message_text(item: &Value) -> String {
    item.get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| match part.get("type").and_then(Value::as_str) {
            Some("output_text" | "text") => non_empty_str(part, "text"),
            Some("refusal") => non_empty_str(part, "refusal"),
            _ => None,
        })
        .collect()
}

/// The output item a streamed event belongs to. Providers that omit ids all
/// share the empty key, which still keeps streamed and backfilled text from
/// being emitted twice.
fn event_item_key(payload: &Value) -> String {
    non_empty_str(payload, "item_id")
        .unwrap_or_default()
        .to_string()
}

/// The key under which [`event_item_key`] files the deltas of this item.
fn output_item_key(item: &Value) -> String {
    non_empty_str(item, "id").unwrap_or_default().to_string()
}

fn non_empty_str<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

/// The message for a failure event that carries none: a fixed sentence and a
/// bounded excerpt of the event, which can embed the whole response.
fn undescribed_failure(payload: &Value) -> String {
    let raw = payload.to_string();
    let excerpt: String = raw.chars().take(MAX_FAILURE_EXCERPT_CHARS).collect();
    let ellipsis = if excerpt.len() < raw.len() { "..." } else { "" };
    format!("the provider reported a failure without details: {excerpt}{ellipsis}")
}

/// Maps a `response.failed` / `error` event onto [`LocalAgentError`] the way
/// the HTTP status mapping does for a rejected request. Codes that signal a
/// transient provider problem become the matching retryable status.
fn failure_error(payload: &Value, config: &LocalEndpointConfig) -> LocalAgentError {
    let error = payload
        .pointer("/response/error")
        .or_else(|| payload.get("error"))
        .filter(|error| error.is_object())
        .unwrap_or(payload);
    let code = non_empty_str(error, "code")
        .or_else(|| non_empty_str(error, "type"))
        .unwrap_or_default();
    let message = non_empty_str(error, "message")
        .map(str::to_string)
        .unwrap_or_else(|| undescribed_failure(payload));
    if openai::looks_like_context_overflow(&format!("{code} {message}")) {
        return LocalAgentError::ContextWindowExceeded { message };
    }
    let status = match code {
        "invalid_api_key" | "incorrect_api_key" | "authentication_error" => {
            return LocalAgentError::InvalidApiKey {
                model_slug: config.model_slug.clone(),
            };
        }
        "rate_limit_exceeded" => 429,
        "server_error" | "internal_error" => 500,
        "service_unavailable" | "overloaded" => 503,
        _ => 400,
    };
    LocalAgentError::ErrorStatus {
        status,
        body: message,
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
