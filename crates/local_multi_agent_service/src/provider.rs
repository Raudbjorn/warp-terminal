use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{
    autocomplete::{
        AUTOCOMPLETE_MAX_TOKENS, AUTOCOMPLETE_MODEL_ALIAS, LocalCommandAutocompleteRequest,
        LocalCommandAutocompleteResponse, autocomplete_provider_messages,
        deterministic_autocomplete_command,
    },
    config::{
        Config, DEFAULT_CONTEXT_WINDOW_TOKENS, DEFAULT_MODEL, non_empty_str, trim_trailing_slash,
    },
    model::resolve_provider_model,
    request::{ContextImage, McpToolSummary},
};

const MODEL_CONTEXT_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ProviderToolCallEnvelope>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderToolCallEnvelope {
    pub id: String,
    pub r#type: String,
    pub function: ProviderToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderToolFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderToolCall {
    pub id: String,
    pub name: String,
    pub arguments_text: String,
}

#[derive(Debug, Clone)]
pub struct ProviderResponse {
    pub content: String,
    pub tool_calls: Vec<ProviderToolCall>,
    pub context_window_usage: Option<f32>,
    pub context_window_tokens: Option<u32>,
    pub suppressed_internal_content: bool,
}

#[derive(Debug, Clone)]
pub struct ChatCompletionParams {
    pub messages: Vec<ProviderChatMessage>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub local_model_aliases: Option<String>,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub mcp_tools: Vec<McpToolSummary>,
    pub enable_tools: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    InvalidApiKey,
    LlmUnavailable,
    ContextWindowExceeded,
    QuotaLimit,
    InternalError,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct LocalAgentError {
    pub message: String,
    pub finish_reason: FinishReason,
    pub model_name: Option<String>,
}

impl LocalAgentError {
    pub fn new(
        message: impl Into<String>,
        finish_reason: FinishReason,
        model_name: Option<String>,
    ) -> Self {
        Self {
            message: message.into(),
            finish_reason,
            model_name,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(message, FinishReason::InternalError, None)
    }
}

#[derive(Clone)]
pub struct ProviderRuntime {
    client: reqwest::Client,
    context_cache: Arc<Mutex<HashMap<String, ModelContextCacheEntry>>>,
}

#[derive(Clone)]
struct ModelContextCacheEntry {
    fetched_at: Instant,
    context_windows_by_model: HashMap<String, u32>,
}

impl ProviderRuntime {
    pub fn new() -> Self {
        crate::install_tls_provider();
        Self {
            client: reqwest::Client::new(),
            context_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn stream_chat_completion(
        &self,
        config: &Config,
        params: ChatCompletionParams,
        mut on_content_chunk: impl FnMut(String) + Send,
    ) -> Result<ProviderResponse, LocalAgentError> {
        let api_key = params
            .api_key
            .as_deref()
            .and_then(non_empty_str)
            .or(config.openai_api_key.as_deref().and_then(non_empty_str))
            .map(str::to_owned)
            .ok_or_else(|| {
                LocalAgentError::new(
                    "OPENAI_API_KEY is not set and the Warp request did not include an OpenAI key.",
                    FinishReason::InvalidApiKey,
                    params.model.clone(),
                )
            })?;
        let base_url = config.provider_base_url(params.base_url.as_deref());
        let model = resolve_provider_model(
            config.openai_model.as_deref(),
            params.model.as_deref(),
            params
                .local_model_aliases
                .as_deref()
                .and_then(non_empty_str)
                .or(config.local_model_aliases.as_deref()),
        )
        .map_err(|error| LocalAgentError::internal(error.to_string()))?;
        let context_window_tokens = self
            .context_window_tokens_for_model(config, &base_url, &api_key, &model)
            .await;
        let tools = (params.enable_tools && config.local_enable_tools).then(local_tool_schemas);
        let messages = build_provider_messages(
            [
                tools
                    .as_ref()
                    .map(|_| local_tool_use_system_prompt(&params.mcp_tools)),
                config.local_multi_agent_system_prompt.clone(),
            ],
            params.messages,
        );
        let request_body = json!({
            "model": model,
            "messages": messages,
            "temperature": params.temperature.unwrap_or(0.2),
            "max_tokens": params.max_tokens.unwrap_or(config.local_max_completion_tokens),
            "stream": true,
        });
        let request_body = if let Some(tools) = tools.as_ref() {
            merge_json_object(
                request_body,
                json!({
                    "tools": tools,
                    "tool_choice": "auto",
                }),
            )
        } else {
            request_body
        };

        let response = self
            .client
            .post(format!("{base_url}/chat/completions"))
            .bearer_auth(&api_key)
            .json(&request_body)
            .send()
            .await
            .map_err(|error| {
                LocalAgentError::new(
                    format!("OpenAI-compatible endpoint request failed: {error}"),
                    FinishReason::LlmUnavailable,
                    Some(model.clone()),
                )
            })?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(classify_provider_error(status, &body, &model));
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut content = String::new();
        let mut reasoning_fallback = String::new();
        let mut content_filter = HarmonyChannelContentFilter::default();
        let mut tool_calls: HashMap<usize, ProviderToolCall> = HashMap::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                LocalAgentError::new(
                    format!("OpenAI-compatible endpoint stream failed: {error}"),
                    FinishReason::LlmUnavailable,
                    Some(model.clone()),
                )
            })?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            let mut lines = buffer
                .split_inclusive(['\n'])
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if !buffer.ends_with('\n') {
                buffer = lines.pop().unwrap_or_default();
            } else {
                buffer.clear();
            }

            for line in lines {
                let trimmed = line.trim();
                if !trimmed.starts_with("data:") {
                    continue;
                }
                let data = trimmed.trim_start_matches("data:").trim();
                if data == "[DONE]" {
                    let final_chunk = content_filter.finish();
                    append_harmony_tool_calls(&mut tool_calls, &mut content_filter);
                    if !final_chunk.is_empty() {
                        reasoning_fallback.clear();
                        content.push_str(&final_chunk);
                        on_content_chunk(final_chunk);
                    }
                    if content.is_empty()
                        && !content_filter.suppressed_internal_content()
                        && !reasoning_fallback.is_empty()
                    {
                        let fallback = std::mem::take(&mut reasoning_fallback);
                        on_content_chunk(fallback.clone());
                        content = fallback;
                    }
                    return Ok(provider_response(
                        content,
                        tool_calls,
                        &messages,
                        tools.as_ref(),
                        context_window_tokens,
                        content_filter.suppressed_internal_content(),
                    ));
                }
                let parsed: Value = serde_json::from_str(data).map_err(|error| {
                    LocalAgentError::new(
                        format!(
                            "OpenAI-compatible endpoint returned malformed stream data: {error}"
                        ),
                        FinishReason::InternalError,
                        Some(model.clone()),
                    )
                })?;
                let raw_chunk = extract_streaming_content(&parsed);
                if !raw_chunk.is_empty() {
                    let chunk = content_filter.push(&raw_chunk);
                    append_harmony_tool_calls(&mut tool_calls, &mut content_filter);
                    if !chunk.is_empty() {
                        reasoning_fallback.clear();
                        content.push_str(&chunk);
                        on_content_chunk(chunk);
                    }
                } else if content.is_empty() && !content_filter.suppressed_internal_content() {
                    reasoning_fallback.push_str(&extract_streaming_reasoning(&parsed));
                }
                extract_streaming_tool_calls(&parsed, &mut tool_calls);
            }
        }

        let final_chunk = content_filter.finish();
        append_harmony_tool_calls(&mut tool_calls, &mut content_filter);
        if !final_chunk.is_empty() {
            reasoning_fallback.clear();
            content.push_str(&final_chunk);
            on_content_chunk(final_chunk);
        }

        if content.is_empty()
            && !content_filter.suppressed_internal_content()
            && !reasoning_fallback.is_empty()
        {
            let fallback = std::mem::take(&mut reasoning_fallback);
            on_content_chunk(fallback.clone());
            content = fallback;
        }

        Ok(provider_response(
            content,
            tool_calls,
            &messages,
            tools.as_ref(),
            context_window_tokens,
            content_filter.suppressed_internal_content(),
        ))
    }

    pub async fn command_autocomplete(
        &self,
        config: &Config,
        request: &LocalCommandAutocompleteRequest,
        api_key: Option<String>,
        base_url: Option<String>,
        local_model_aliases: Option<String>,
    ) -> Result<LocalCommandAutocompleteResponse, LocalAgentError> {
        if let Some(command) = deterministic_autocomplete_command(request) {
            return Ok(LocalCommandAutocompleteResponse::from_command(command));
        }

        let response = self
            .stream_chat_completion(
                config,
                ChatCompletionParams {
                    messages: autocomplete_provider_messages(request),
                    api_key,
                    base_url,
                    local_model_aliases,
                    model: Some(AUTOCOMPLETE_MODEL_ALIAS.to_string()),
                    max_tokens: Some(AUTOCOMPLETE_MAX_TOKENS),
                    temperature: Some(0.0),
                    mcp_tools: Vec::new(),
                    enable_tools: false,
                },
                |_| {},
            )
            .await?;

        Ok(LocalCommandAutocompleteResponse::from_raw_output(
            response.content,
            request,
        ))
    }

    pub async fn fetch_provider_models(&self, config: &Config) -> Vec<LocalModelConfig> {
        let Some(base_url) = config.openai_base_url.as_deref().and_then(non_empty_str) else {
            return fallback_local_models(config);
        };
        let base_url = trim_trailing_slash(base_url);
        let mut request = self
            .client
            .get(format!("{base_url}/models"))
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(api_key) = config.openai_api_key.as_deref().and_then(non_empty_str) {
            request = request.bearer_auth(api_key);
        }
        let Ok(response) = request.send().await else {
            return fallback_local_models(config);
        };
        if !response.status().is_success() {
            return fallback_local_models(config);
        }
        let Ok(payload) = response.json::<Value>().await else {
            return fallback_local_models(config);
        };
        let models = payload
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| match item {
                Value::String(id) => {
                    non_empty_str(id).map(|id| LocalModelConfig::new(id.to_owned()))
                }
                Value::Object(model) => model
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(non_empty_str)
                    .map(|id| LocalModelConfig::new(id.to_owned())),
                _ => None,
            })
            .collect::<Vec<_>>();
        if models.is_empty() {
            fallback_local_models(config)
        } else {
            models
        }
    }

    async fn context_window_tokens_for_model(
        &self,
        config: &Config,
        base_url: &str,
        api_key: &str,
        model: &str,
    ) -> Option<u32> {
        if let Some(configured) =
            configured_context_window_tokens(config.local_model_context_tokens.as_deref(), model)
        {
            return Some(configured);
        }
        let provider_models = self
            .fetch_provider_model_context_windows(base_url, api_key)
            .await;
        provider_models
            .get(model)
            .copied()
            .or_else(|| built_in_model_context_window(model))
            .or(Some(DEFAULT_CONTEXT_WINDOW_TOKENS))
    }

    async fn fetch_provider_model_context_windows(
        &self,
        base_url: &str,
        api_key: &str,
    ) -> HashMap<String, u32> {
        {
            let cache = self.context_cache.lock().await;
            if let Some(entry) = cache.get(base_url)
                && entry.fetched_at.elapsed() < MODEL_CONTEXT_CACHE_TTL
            {
                return entry.context_windows_by_model.clone();
            }
        }

        let mut context_windows_by_model = HashMap::new();
        let result = self
            .client
            .get(format!("{base_url}/models"))
            .bearer_auth(api_key)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await;
        if let Ok(response) = result
            && response.status().is_success()
            && let Ok(payload) = response.json::<Value>().await
            && let Some(data) = payload.get("data").and_then(Value::as_array)
        {
            for item in data {
                let Some(model) = item.as_object() else {
                    continue;
                };
                let Some(id) = model
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(non_empty_str)
                else {
                    continue;
                };
                if let Some(window) = context_window_from_provider_model(item) {
                    context_windows_by_model.insert(id.to_owned(), window);
                }
            }
        }

        self.context_cache.lock().await.insert(
            base_url.to_owned(),
            ModelContextCacheEntry {
                fetched_at: Instant::now(),
                context_windows_by_model: context_windows_by_model.clone(),
            },
        );
        context_windows_by_model
    }
}

impl Default for ProviderRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelConfig {
    pub id: String,
    pub display_name: String,
    pub base_model_name: String,
    pub credit_multiplier: Option<f64>,
    pub description: Option<String>,
    pub disable_reason: Option<String>,
    pub provider: String,
    pub reasoning_level: Option<String>,
    pub request_multiplier: f64,
    pub vision_supported: bool,
}

impl LocalModelConfig {
    fn new(id: String) -> Self {
        Self {
            display_name: id.clone(),
            base_model_name: id.clone(),
            id,
            credit_multiplier: None,
            description: None,
            disable_reason: None,
            provider: "UNKNOWN".to_owned(),
            reasoning_level: None,
            request_multiplier: 1.0,
            vision_supported: true,
        }
    }
}

pub fn provider_tool_call_message(tool_call: &ProviderToolCall) -> ProviderToolCallEnvelope {
    ProviderToolCallEnvelope {
        id: tool_call.id.clone(),
        r#type: "function".to_owned(),
        function: ProviderToolFunction {
            name: tool_call.name.clone(),
            arguments: tool_call.arguments_text.clone(),
        },
    }
}

pub fn user_text_message(content: impl Into<Value>) -> ProviderChatMessage {
    ProviderChatMessage {
        role: "user".to_owned(),
        content: Some(content.into()),
        tool_call_id: None,
        tool_calls: None,
    }
}

pub fn tool_result_message(tool_call_id: String, content: String) -> ProviderChatMessage {
    ProviderChatMessage {
        role: "tool".to_owned(),
        content: Some(Value::String(content)),
        tool_call_id: Some(tool_call_id),
        tool_calls: None,
    }
}

pub fn assistant_message(
    content: String,
    tool_calls: Vec<ProviderToolCall>,
) -> ProviderChatMessage {
    ProviderChatMessage {
        role: "assistant".to_owned(),
        content: Some(Value::String(content)),
        tool_call_id: None,
        tool_calls: (!tool_calls.is_empty())
            .then(|| tool_calls.iter().map(provider_tool_call_message).collect()),
    }
}

pub fn system_message(content: String) -> ProviderChatMessage {
    ProviderChatMessage {
        role: "system".to_owned(),
        content: Some(Value::String(content)),
        tool_call_id: None,
        tool_calls: None,
    }
}

pub fn content_with_images(text: String, images: &[ContextImage]) -> Value {
    if images.is_empty() {
        return Value::String(text);
    }
    Value::Array(
        std::iter::once(json!({ "type": "text", "text": text }))
            .chain(images.iter().map(|image| {
                json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:{};base64,{}", image.mime_type, image.data),
                    },
                })
            }))
            .collect(),
    )
}

pub fn messages_from_stored_conversation(
    messages: &[Value],
    max_messages: usize,
) -> Vec<ProviderChatMessage> {
    messages
        .iter()
        .filter_map(|value| {
            let mut message = serde_json::from_value::<ProviderChatMessage>(value.clone()).ok()?;
            if message.role == "assistant"
                && let Some(Value::String(content)) = message.content.take()
            {
                message.content = Some(Value::String(filter_harmony_channel_content(&content)));
            }
            let has_content = message
                .content
                .as_ref()
                .is_some_and(|content| provider_message_content_length(Some(content)) > 0);
            let has_tool_calls = message
                .tool_calls
                .as_ref()
                .is_some_and(|tool_calls| !tool_calls.is_empty());
            if message.role == "assistant" && !has_content && !has_tool_calls {
                return None;
            }
            Some(message)
        })
        .filter(|message| {
            matches!(
                message.role.as_str(),
                "system" | "user" | "assistant" | "tool"
            )
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .take(max_messages)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

pub fn provider_messages_to_json(messages: &[ProviderChatMessage]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|message| serde_json::to_value(message).ok())
        .collect()
}

fn local_tool_schemas() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "read_files",
                "description": "Read one or more text files from the user's current workspace or shell context.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "files": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "name": { "type": "string", "description": "A relative or absolute file path to read." },
                                    "line_ranges": {
                                        "type": "array",
                                        "items": {
                                            "type": "object",
                                            "additionalProperties": false,
                                            "properties": {
                                                "start": { "type": "integer", "minimum": 1 },
                                                "end": { "type": "integer", "minimum": 1 }
                                            },
                                            "required": ["start", "end"]
                                        }
                                    }
                                },
                                "required": ["name"]
                            }
                        }
                    },
                    "required": ["files"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "file_glob",
                "description": "Find files whose names match glob patterns.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "patterns": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                        "pattern": { "type": "string" },
                        "search_dir": { "type": "string", "description": "Directory to search. Defaults to the current directory." },
                        "max_matches": { "type": "integer", "minimum": 0 },
                        "max_depth": { "type": "integer", "minimum": 0 },
                        "min_depth": { "type": "integer", "minimum": 0 }
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "grep",
                "description": "Search for text or patterns in files under a path.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "queries": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                        "query": { "type": "string" },
                        "path": { "type": "string", "description": "File or directory to search. Defaults to the current directory." }
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "search_codebase",
                "description": "Search indexed codebase context for relevant files and snippets.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "query": { "type": "string" },
                        "path_filters": { "type": "array", "items": { "type": "string" } },
                        "codebase_path": { "type": "string" }
                    },
                    "required": ["query"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "run_shell_command",
                "description": "Run a shell command in the user's current terminal context.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "command": { "type": "string" },
                        "is_read_only": { "type": "boolean", "description": "Whether the command only reads state." },
                        "is_risky": { "type": "boolean", "description": "Whether the command may modify files, processes, or external state." },
                        "uses_pager": { "type": "boolean" },
                        "wait_until_complete": { "type": "boolean" }
                    },
                    "required": ["command"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "apply_file_diffs",
                "description": "Request edits to local files using search/replace diffs, file creation, deletion, or V4A hunks.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "summary": { "type": "string" },
                        "diffs": { "type": "array", "items": { "type": "object", "additionalProperties": false, "properties": {
                            "file_path": { "type": "string" },
                            "search": { "type": "string" },
                            "replace": { "type": "string" }
                        }, "required": ["file_path"] } },
                        "new_files": { "type": "array", "items": { "type": "object", "additionalProperties": false, "properties": {
                            "file_path": { "type": "string" },
                            "content": { "type": "string" }
                        }, "required": ["file_path", "content"] } },
                        "deleted_files": { "type": "array", "items": { "type": "object", "additionalProperties": false, "properties": {
                            "file_path": { "type": "string" }
                        }, "required": ["file_path"] } }
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "suggest_plan",
                "description": "Suggest a plan for the user to review before continuing.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "summary": { "type": "string" },
                        "tasks": { "type": "array", "minItems": 1, "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": { "description": { "type": "string" } },
                            "required": ["description"]
                        }}
                    },
                    "required": ["summary", "tasks"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "read_mcp_resource",
                "description": "Read one MCP resource by URI from the MCP resources listed in the request context.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "uri": { "type": "string", "description": "The exact MCP resource URI to read." },
                        "server_id": { "type": "string", "description": "Optional MCP server id from the request context." }
                    },
                    "required": ["uri"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "call_mcp_tool",
                "description": "Call one MCP tool from the MCP tools listed in the request context.",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "name": { "type": "string", "description": "The exact MCP tool name to call." },
                        "server_id": { "type": "string", "description": "Optional MCP server id from the request context." },
                        "args": { "type": "object", "description": "JSON object arguments for the MCP tool.", "additionalProperties": true }
                    },
                    "required": ["name"]
                }
            }
        }),
    ]
}

fn local_tool_use_system_prompt(mcp_tools: &[McpToolSummary]) -> String {
    let mut tool_names = mcp_tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    tool_names.sort_unstable();
    tool_names.dedup();
    let mcp_tool_text = if tool_names.is_empty() {
        String::new()
    } else {
        format!(
            "\nAvailable MCP tool names from request context include: {}.",
            tool_names.join(", ")
        )
    };
    [
        "Use only the OpenAI function tools explicitly provided in this request.",
        "To call any MCP tool, always use the provided call_mcp_tool function.",
        "Do not emit provider tool calls named after MCP tools directly, such as list_issues or search_docs.",
        "For call_mcp_tool, set name to the exact MCP tool name from the request context and pass the MCP tool arguments in args.",
        "If the MCP context includes a server_id for the desired tool, include that server_id. This is required when multiple MCP servers expose the same tool name.",
        &mcp_tool_text,
    ]
    .join("\n")
}

fn build_provider_messages(
    system_prompt_contents: impl IntoIterator<Item = Option<String>>,
    conversation_messages: Vec<ProviderChatMessage>,
) -> Vec<ProviderChatMessage> {
    let mut system_contents = system_prompt_contents.into_iter().collect::<Vec<_>>();
    let mut non_system_messages = Vec::new();

    for message in conversation_messages {
        if message.role == "system" {
            system_contents.push(provider_system_content(message.content.as_ref()));
        } else {
            non_system_messages.push(message);
        }
    }

    let system_content = system_contents
        .into_iter()
        .flatten()
        .filter_map(|content| non_empty_str(&content).map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n\n");
    if system_content.is_empty() {
        non_system_messages
    } else {
        std::iter::once(system_message(system_content))
            .chain(non_system_messages)
            .collect()
    }
}

fn provider_system_content(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(content)) => non_empty_str(content).map(str::to_owned),
        Some(Value::Array(parts)) => {
            let text = parts
                .iter()
                .filter_map(|part| {
                    part.as_object()
                        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                        .and_then(|part| part.get("text"))
                        .and_then(Value::as_str)
                })
                .collect::<Vec<_>>()
                .join("\n");
            non_empty_str(&text).map(str::to_owned)
        }
        _ => None,
    }
}

fn provider_response(
    content: String,
    tool_calls: HashMap<usize, ProviderToolCall>,
    messages: &[ProviderChatMessage],
    tools: Option<&Vec<Value>>,
    context_window_tokens: Option<u32>,
    suppressed_internal_content: bool,
) -> ProviderResponse {
    let mut tool_calls = tool_calls
        .into_values()
        .filter(|tool_call| !tool_call.id.is_empty() && !tool_call.name.is_empty())
        .collect::<Vec<_>>();
    tool_calls.sort_by(|left, right| left.id.cmp(&right.id));
    let context_window_usage = estimate_context_window_usage(
        messages,
        tools,
        &content,
        &tool_calls,
        context_window_tokens,
    );
    ProviderResponse {
        content,
        tool_calls,
        context_window_usage,
        context_window_tokens,
        suppressed_internal_content,
    }
}

#[derive(Debug, Default)]
struct HarmonyChannelContentFilter {
    mode: HarmonyChannelMode,
    pending: String,
    tool_calls: Vec<ProviderToolCall>,
    suppressed_internal_content: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum HarmonyChannelMode {
    #[default]
    Public,
    Internal,
}

#[derive(Debug, Clone, Copy)]
struct HarmonyChannelMarker {
    token: &'static str,
    mode: HarmonyChannelMode,
}

const HARMONY_CHANNEL_MARKERS: &[HarmonyChannelMarker] = &[
    HarmonyChannelMarker {
        token: "<|channel>thought",
        mode: HarmonyChannelMode::Internal,
    },
    HarmonyChannelMarker {
        token: "<channel>thought",
        mode: HarmonyChannelMode::Internal,
    },
    HarmonyChannelMarker {
        token: "<|channel>analysis",
        mode: HarmonyChannelMode::Internal,
    },
    HarmonyChannelMarker {
        token: "<channel>analysis",
        mode: HarmonyChannelMode::Internal,
    },
    HarmonyChannelMarker {
        token: "<|channel>final",
        mode: HarmonyChannelMode::Public,
    },
    HarmonyChannelMarker {
        token: "<channel>final",
        mode: HarmonyChannelMode::Public,
    },
];

const HARMONY_TOOL_CALL_OPENERS: &[&str] = &["<|tool_call>", "<tool_call>"];
const HARMONY_TOOL_CALL_CLOSERS: &[&str] = &["<tool_call|>", "</tool_call>"];

impl HarmonyChannelContentFilter {
    fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        self.drain(false)
    }

    fn finish(&mut self) -> String {
        self.drain(true)
    }

    fn suppressed_internal_content(&self) -> bool {
        self.suppressed_internal_content
    }

    fn take_tool_calls(&mut self) -> Vec<ProviderToolCall> {
        std::mem::take(&mut self.tool_calls)
    }

    fn drain(&mut self, final_flush: bool) -> String {
        let mut output = String::new();
        loop {
            match find_next_harmony_content_item(&self.pending) {
                Some(HarmonyContentItem::Channel { index, marker }) => {
                    self.emit_pending_prefix(index, &mut output);
                    self.pending.drain(..marker.token.len());
                    self.mode = marker.mode;
                    if self.mode == HarmonyChannelMode::Public {
                        trim_pending_channel_separator(&mut self.pending);
                    }
                    continue;
                }
                Some(HarmonyContentItem::ToolCall { index, opener }) => {
                    self.emit_pending_prefix(index, &mut output);
                    if let Some(close) = find_harmony_tool_call_close(&self.pending, opener.len()) {
                        let payload_start = opener.len();
                        let payload_end = close.index;
                        let payload = self.pending[payload_start..payload_end].to_owned();
                        self.pending.drain(..payload_end + close.token.len());
                        self.suppressed_internal_content = true;
                        if let Some(tool_call) = parse_harmony_tool_call_payload(&payload) {
                            self.tool_calls.push(tool_call);
                        }
                        continue;
                    }

                    if final_flush {
                        let len = self.pending.len();
                        self.pending.drain(..len);
                        self.suppressed_internal_content = true;
                    }
                    break;
                }
                None => {}
            }

            let safe_len = if final_flush {
                self.pending.len()
            } else {
                self.pending.len() - harmony_special_prefix_suffix_len(&self.pending)
            };
            self.emit_pending_prefix(safe_len, &mut output);
            break;
        }
        output
    }

    fn emit_pending_prefix(&mut self, len: usize, output: &mut String) {
        if len == 0 {
            return;
        }
        let prefix = self.pending[..len].to_owned();
        self.pending.drain(..len);
        match self.mode {
            HarmonyChannelMode::Public => output.push_str(&prefix),
            HarmonyChannelMode::Internal => self.suppressed_internal_content = true,
        }
    }
}

enum HarmonyContentItem {
    Channel {
        index: usize,
        marker: HarmonyChannelMarker,
    },
    ToolCall {
        index: usize,
        opener: &'static str,
    },
}

impl HarmonyContentItem {
    fn index(&self) -> usize {
        match self {
            HarmonyContentItem::Channel { index, .. }
            | HarmonyContentItem::ToolCall { index, .. } => *index,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct HarmonyToolCallClose {
    index: usize,
    token: &'static str,
}

fn find_next_harmony_content_item(input: &str) -> Option<HarmonyContentItem> {
    HARMONY_CHANNEL_MARKERS
        .iter()
        .filter_map(|marker| {
            input
                .find(marker.token)
                .map(|index| HarmonyContentItem::Channel {
                    index,
                    marker: *marker,
                })
        })
        .chain(HARMONY_TOOL_CALL_OPENERS.iter().filter_map(|opener| {
            input
                .find(opener)
                .map(|index| HarmonyContentItem::ToolCall {
                    index,
                    opener: *opener,
                })
        }))
        .min_by_key(HarmonyContentItem::index)
}

fn find_harmony_tool_call_close(input: &str, search_start: usize) -> Option<HarmonyToolCallClose> {
    HARMONY_TOOL_CALL_CLOSERS
        .iter()
        .filter_map(|token| {
            input[search_start..]
                .find(token)
                .map(|index| HarmonyToolCallClose {
                    index: search_start + index,
                    token: *token,
                })
        })
        .min_by_key(|close| close.index)
}

fn harmony_special_prefix_suffix_len(input: &str) -> usize {
    HARMONY_CHANNEL_MARKERS
        .iter()
        .map(|marker| marker.token)
        .chain(HARMONY_TOOL_CALL_OPENERS.iter().copied())
        .flat_map(|token| (1..token.len()).map(move |len| &token[..len]))
        .filter(|prefix| input.ends_with(prefix))
        .map(str::len)
        .max()
        .unwrap_or_default()
}

fn trim_pending_channel_separator(input: &mut String) {
    let trimmed = input.trim_start_matches(['\n', '\r']);
    let trim_len = input.len() - trimmed.len();
    if trim_len > 0 {
        input.drain(..trim_len);
    }
}

fn filter_harmony_channel_content(content: &str) -> String {
    let mut filter = HarmonyChannelContentFilter::default();
    let mut filtered = filter.push(content);
    filtered.push_str(&filter.finish());
    filtered
}

fn append_harmony_tool_calls(
    accumulated: &mut HashMap<usize, ProviderToolCall>,
    filter: &mut HarmonyChannelContentFilter,
) {
    for tool_call in filter.take_tool_calls() {
        let index = accumulated
            .keys()
            .copied()
            .max()
            .map(|index| index + 1)
            .unwrap_or_default();
        accumulated.insert(index, tool_call);
    }
}

fn parse_harmony_tool_call_payload(payload: &str) -> Option<ProviderToolCall> {
    let payload = payload.trim().strip_prefix("call:")?.trim();
    let name_end = payload.find('{')?;
    let name = non_empty_str(payload[..name_end].trim())?.to_owned();
    let arguments = payload[name_end..].trim();
    let arguments = parse_harmony_tool_call_value(arguments)
        .or_else(|| serde_json::from_str(arguments).ok())
        .unwrap_or_else(|| Value::Object(Default::default()));
    Some(ProviderToolCall {
        id: format!("harmony-tool-{}", uuid::Uuid::new_v4()),
        name,
        arguments_text: serde_json::to_string(&arguments).ok()?,
    })
}

fn parse_harmony_tool_call_value(input: &str) -> Option<Value> {
    let mut parser = HarmonyToolCallValueParser { input, index: 0 };
    let value = parser.parse_value()?;
    parser.skip_whitespace();
    (parser.index == input.len()).then_some(value)
}

struct HarmonyToolCallValueParser<'a> {
    input: &'a str,
    index: usize,
}

impl HarmonyToolCallValueParser<'_> {
    fn parse_value(&mut self) -> Option<Value> {
        self.skip_whitespace();
        if self.rest().starts_with('{') {
            return self.parse_object();
        }
        if self.rest().starts_with('[') {
            return self.parse_array();
        }
        if self.rest().starts_with("<|\"|>") {
            return self.parse_harmony_string().map(Value::String);
        }
        if self.rest().starts_with('"') {
            return self.parse_json_string().map(Value::String);
        }
        self.parse_bare_value()
    }

    fn parse_object(&mut self) -> Option<Value> {
        self.consume_char('{')?;
        let mut object = serde_json::Map::new();
        loop {
            self.skip_whitespace();
            if self.consume_char('}').is_some() {
                break;
            }
            let key = self.parse_key()?;
            self.skip_whitespace();
            self.consume_char(':')?;
            let value = self.parse_value()?;
            object.insert(key, value);
            self.skip_whitespace();
            if self.consume_char(',').is_some() {
                continue;
            }
            self.consume_char('}')?;
            break;
        }
        Some(Value::Object(object))
    }

    fn parse_array(&mut self) -> Option<Value> {
        self.consume_char('[')?;
        let mut values = Vec::new();
        loop {
            self.skip_whitespace();
            if self.consume_char(']').is_some() {
                break;
            }
            values.push(self.parse_value()?);
            self.skip_whitespace();
            if self.consume_char(',').is_some() {
                continue;
            }
            self.consume_char(']')?;
            break;
        }
        Some(Value::Array(values))
    }

    fn parse_key(&mut self) -> Option<String> {
        self.skip_whitespace();
        if self.rest().starts_with("<|\"|>") {
            return self.parse_harmony_string();
        }
        if self.rest().starts_with('"') {
            return self.parse_json_string();
        }
        let start = self.index;
        let end = self
            .rest()
            .find(':')
            .unwrap_or_else(|| self.rest().len());
        self.index += end;
        non_empty_str(self.input[start..self.index].trim()).map(str::to_owned)
    }

    fn parse_harmony_string(&mut self) -> Option<String> {
        let marker = "<|\"|>";
        self.consume_str(marker)?;
        let start = self.index;
        let end = self.rest().find(marker)?;
        let value = self.input[start..start + end].to_owned();
        self.index = start + end + marker.len();
        Some(value)
    }

    fn parse_json_string(&mut self) -> Option<String> {
        let start = self.index;
        self.consume_char('"')?;
        let mut escaped = false;
        while let Some(ch) = self.next_char() {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => {
                    return serde_json::from_str(&self.input[start..self.index]).ok();
                }
                _ => {}
            }
        }
        None
    }

    fn parse_bare_value(&mut self) -> Option<Value> {
        let start = self.index;
        let end = self
            .rest()
            .find(|ch| matches!(ch, ',' | '}' | ']'))
            .unwrap_or_else(|| self.rest().len());
        self.index += end;
        let value = self.input[start..self.index].trim();
        if value.is_empty() {
            return None;
        }
        match value {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            "null" => Some(Value::Null),
            _ => serde_json::from_str(value)
                .ok()
                .or_else(|| Some(Value::String(value.to_owned()))),
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.rest().chars().next() {
            if !ch.is_whitespace() {
                break;
            }
            self.index += ch.len_utf8();
        }
    }

    fn consume_str(&mut self, expected: &str) -> Option<()> {
        self.rest().starts_with(expected).then(|| {
            self.index += expected.len();
        })
    }

    fn consume_char(&mut self, expected: char) -> Option<()> {
        let ch = self.rest().chars().next()?;
        (ch == expected).then(|| {
            self.index += ch.len_utf8();
        })
    }

    fn next_char(&mut self) -> Option<char> {
        let ch = self.rest().chars().next()?;
        self.index += ch.len_utf8();
        Some(ch)
    }

    fn rest(&self) -> &str {
        &self.input[self.index..]
    }
}

fn extract_streaming_content(payload: &Value) -> String {
    payload
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|choice| choice.get("delta")?.get("content")?.as_str())
        .collect()
}

fn extract_streaming_reasoning(payload: &Value) -> String {
    payload
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|choice| {
            let delta = choice.get("delta")?;
            delta
                .get("reasoning")
                .or_else(|| delta.get("reasoning_content"))
                .or_else(|| delta.get("reasoningContent"))
                .and_then(Value::as_str)
        })
        .collect()
}

fn extract_streaming_tool_calls(
    payload: &Value,
    accumulated: &mut HashMap<usize, ProviderToolCall>,
) {
    let Some(choices) = payload.get("choices").and_then(Value::as_array) else {
        return;
    };
    for choice in choices {
        let Some(tool_calls) = choice
            .get("delta")
            .and_then(|delta| delta.get("tool_calls"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for tool_call_delta in tool_calls {
            let Some(index) = tool_call_delta
                .get("index")
                .and_then(Value::as_u64)
                .map(|value| value as usize)
            else {
                continue;
            };
            let existing = accumulated
                .entry(index)
                .or_insert_with(|| ProviderToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments_text: String::new(),
                });
            if let Some(id) = tool_call_delta.get("id").and_then(Value::as_str) {
                existing.id = id.to_owned();
            }
            if let Some(name) = tool_call_delta
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
            {
                existing.name.push_str(name);
            }
            if let Some(arguments) = tool_call_delta
                .get("function")
                .and_then(|function| function.get("arguments"))
                .and_then(Value::as_str)
            {
                existing.arguments_text.push_str(arguments);
            }
        }
    }
}

fn classify_provider_error(status: u16, body: &str, model: &str) -> LocalAgentError {
    let lower = body.to_ascii_lowercase();
    let message = format!("OpenAI-compatible endpoint returned {status}: {body}");
    if status == 401
        || status == 403
        || lower.contains("invalid api key")
        || lower.contains("invalid_api_key")
        || lower.contains("incorrect api key")
        || lower.contains("unauthorized")
    {
        return LocalAgentError::new(message, FinishReason::InvalidApiKey, Some(model.to_owned()));
    }
    if status == 429 {
        return LocalAgentError::new(message, FinishReason::QuotaLimit, Some(model.to_owned()));
    }
    if status == 413
        || lower.contains("context window")
        || lower.contains("context_window")
        || lower.contains("context length")
        || lower.contains("maximum context")
        || lower.contains("too many tokens")
        || lower.contains("token limit")
        || lower.contains("input is too long")
    {
        return LocalAgentError::new(
            message,
            FinishReason::ContextWindowExceeded,
            Some(model.to_owned()),
        );
    }
    if matches!(status, 408 | 500 | 502 | 503 | 504) {
        return LocalAgentError::new(
            message,
            FinishReason::LlmUnavailable,
            Some(model.to_owned()),
        );
    }
    LocalAgentError::new(message, FinishReason::InternalError, Some(model.to_owned()))
}

fn configured_context_window_tokens(raw: Option<&str>, model: &str) -> Option<u32> {
    let raw = raw.and_then(non_empty_str)?;
    if let Some(value) = finite_positive_number(raw) {
        return Some(value);
    }
    let parsed = serde_json::from_str::<Value>(raw).ok()?;
    let object = parsed.as_object()?;
    object
        .get(model)
        .and_then(value_positive_u32)
        .or_else(|| object.get("default").and_then(value_positive_u32))
}

fn context_window_from_provider_model(model: &Value) -> Option<u32> {
    let direct_keys = [
        "context_length",
        "contextLength",
        "max_context_length",
        "maxContextLength",
        "max_model_len",
        "maxModelLen",
        "max_sequence_length",
        "maxSequenceLength",
        "n_ctx",
        "nCtx",
    ];
    for key in direct_keys {
        if let Some(value) = model.get(key).and_then(value_positive_u32) {
            return Some(value);
        }
    }
    for key in ["metadata", "model_info", "modelInfo"] {
        if let Some(value) = model.get(key)
            && let Some(nested) = context_window_from_provider_model(value)
        {
            return Some(nested);
        }
    }
    None
}

fn value_positive_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0),
        Value::String(value) => finite_positive_number(value),
        _ => None,
    }
}

fn finite_positive_number(value: &str) -> Option<u32> {
    value.trim().parse::<u32>().ok().filter(|value| *value > 0)
}

fn built_in_model_context_window(model: &str) -> Option<u32> {
    (model == DEFAULT_MODEL).then_some(262_144)
}

fn fallback_local_models(config: &Config) -> Vec<LocalModelConfig> {
    parse_local_model_list(config.local_model_list.as_str()).unwrap_or_else(|_| {
        vec![LocalModelConfig::new(
            config
                .openai_model
                .clone()
                .unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
        )]
    })
}

fn parse_local_model_list(raw: &str) -> anyhow::Result<Vec<LocalModelConfig>> {
    let raw = non_empty_str(raw).unwrap_or(DEFAULT_MODEL);
    let values = if raw.starts_with('[') {
        serde_json::from_str::<Vec<Value>>(raw)?
    } else {
        raw.split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| Value::String(value.to_owned()))
            .collect()
    };
    let mut models = Vec::new();
    for value in values {
        match value {
            Value::String(id) => models.push(LocalModelConfig::new(id)),
            Value::Object(object) => {
                let id = object
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(non_empty_str)
                    .unwrap_or(DEFAULT_MODEL)
                    .to_owned();
                let mut model = LocalModelConfig::new(id.clone());
                model.display_name = object
                    .get("displayName")
                    .or_else(|| object.get("display_name"))
                    .and_then(Value::as_str)
                    .and_then(non_empty_str)
                    .unwrap_or(&id)
                    .to_owned();
                model.base_model_name = object
                    .get("baseModelName")
                    .or_else(|| object.get("base_model_name"))
                    .and_then(Value::as_str)
                    .and_then(non_empty_str)
                    .unwrap_or(&model.display_name)
                    .to_owned();
                model.description = object
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                model.disable_reason = object
                    .get("disableReason")
                    .or_else(|| object.get("disable_reason"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                model.reasoning_level = object
                    .get("reasoningLevel")
                    .or_else(|| object.get("reasoning_level"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                model.provider = normalize_model_provider(
                    object
                        .get("provider")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                );
                model.request_multiplier = object
                    .get("requestMultiplier")
                    .or_else(|| object.get("request_multiplier"))
                    .and_then(Value::as_f64)
                    .unwrap_or(1.0)
                    .max(1.0);
                model.credit_multiplier = object
                    .get("creditMultiplier")
                    .or_else(|| object.get("credit_multiplier"))
                    .and_then(Value::as_f64);
                model.vision_supported = object
                    .get("visionSupported")
                    .or_else(|| object.get("vision_supported"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                models.push(model);
            }
            _ => {}
        }
    }
    if models.is_empty() {
        anyhow::bail!("LOCAL_MODEL_LIST must include at least one model.");
    }
    Ok(models)
}

fn normalize_model_provider(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "anthropic" => "ANTHROPIC",
        "google" => "GOOGLE",
        "openai" => "OPENAI",
        "xai" => "XAI",
        _ => "UNKNOWN",
    }
    .to_owned()
}

fn merge_json_object(mut left: Value, right: Value) -> Value {
    if let (Some(left), Some(right)) = (left.as_object_mut(), right.as_object()) {
        left.extend(right.clone());
    }
    left
}

fn provider_message_content_length(content: Option<&Value>) -> usize {
    match content {
        Some(Value::String(content)) => content.len(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        part.get("image_url")
                            .and_then(|image| image.get("url"))
                            .and_then(Value::as_str)
                    })
                    .map(str::len)
                    .unwrap_or_default()
            })
            .sum(),
        _ => 0,
    }
}

fn provider_tool_call_content_length(tool_calls: &[ProviderToolCall]) -> usize {
    tool_calls
        .iter()
        .map(|tool_call| tool_call.id.len() + tool_call.name.len() + tool_call.arguments_text.len())
        .sum()
}

fn estimate_context_window_usage(
    messages: &[ProviderChatMessage],
    tools: Option<&Vec<Value>>,
    assistant_content: &str,
    assistant_tool_calls: &[ProviderToolCall],
    context_window_tokens: Option<u32>,
) -> Option<f32> {
    let context_window_tokens = context_window_tokens?;
    let message_chars: usize = messages
        .iter()
        .map(|message| {
            let tool_calls = message
                .tool_calls
                .as_ref()
                .map(|tool_calls| {
                    tool_calls
                        .iter()
                        .map(|tool_call| ProviderToolCall {
                            id: tool_call.id.clone(),
                            name: tool_call.function.name.clone(),
                            arguments_text: tool_call.function.arguments.clone(),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            message.role.len()
                + provider_message_content_length(message.content.as_ref())
                + provider_tool_call_content_length(&tool_calls)
        })
        .sum();
    let tools_chars = tools
        .map(|tools| serde_json::to_string(tools).unwrap_or_default().len())
        .unwrap_or_default();
    let assistant_chars =
        assistant_content.len() + provider_tool_call_content_length(assistant_tool_calls);
    let estimated_tokens = (message_chars + tools_chars + assistant_chars).div_ceil(4) as f32;
    Some((estimated_tokens / context_window_tokens as f32).min(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_system_messages() {
        let messages = build_provider_messages(
            [Some("tools".to_owned()), Some("custom".to_owned())],
            vec![
                system_message("history".to_owned()),
                user_text_message("hi"),
            ],
        );

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(
            messages[0].content,
            Some(Value::String("tools\n\ncustom\n\nhistory".to_owned()))
        );
    }

    #[test]
    fn parses_context_window_override_map() {
        assert_eq!(
            configured_context_window_tokens(Some(r#"{"model":1234,"default":99}"#), "model"),
            Some(1234)
        );
        assert_eq!(
            configured_context_window_tokens(Some(r#"{"default":99}"#), "other"),
            Some(99)
        );
    }

    #[test]
    fn fallback_models_parse_csv() {
        let models = parse_local_model_list("a,b").unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod provider_tests;
