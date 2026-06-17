use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use parking_lot::{Mutex, RwLock};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{
    ai::local_opencode::{OpenCodeError, OpenCodeSidecarPool},
    ai::predict::{
        generate_ai_input_suggestions::{
            GenerateAIInputSuggestionsRequest, GenerateAIInputSuggestionsResponseV2,
        },
        predict_am_queries::{PredictAMQueriesRequest, PredictAMQueriesResponse},
    },
    ai_assistant::{execution_context::WarpAiExecutionContext, AIGeneratedCommand},
    drive::workflows::ai_assist::{GeneratedArgument, GeneratedCommandMetadata},
    settings::{LocalOpenAISettingsSnapshot, LocalProviderKind},
};

#[derive(Clone)]
pub(crate) struct LocalOpenAIClient {
    config: Arc<RwLock<LocalOpenAISettingsSnapshot>>,
    // `OpenCodeSidecarPool` is already `Clone` + internally synchronized, so no
    // outer mutex is needed (it would just serialize concurrent requests).
    opencode_pool: OpenCodeSidecarPool,
    /// Per-client override for the working directory the OpenCode
    /// sidecar pool keys on. `None` means "use the process CWD at
    /// request time". A future refactor can wire this through to a
    /// per-tab working directory.
    opencode_working_dir: Arc<RwLock<Option<PathBuf>>>,
    /// Per-conversation Responses-API reasoning-item carryover. The
    /// key is the conversation identifier supplied by the caller;
    /// the value is the running list of reasoning items the local
    /// provider returned and that the next turn must round-trip.
    /// A new entry is created lazily on the first request for a
    /// given key; an entry is dropped when the caller invokes
    /// [`LocalOpenAIClient::clear_reasoning_state`].
    #[allow(dead_code)]
    reasoning_cache: Arc<Mutex<HashMap<String, Vec<ReasoningItem>>>>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum LocalOpenAIError {
    #[error("local OpenAI-compatible provider is not enabled")]
    NotConfigured,
    #[error("local OpenAI-compatible provider did not return a message")]
    EmptyResponse,
    #[error("local OpenAI-compatible provider returned invalid JSON")]
    InvalidJson(#[from] serde_json::Error),
    #[error("local OpenAI-compatible provider request failed")]
    Request(#[from] http_client::Error),
    #[error("local OpenAI-compatible provider returned an error status")]
    Status(#[from] http_client::ResponseError),
    #[error("local OpenAI-compatible provider response could not be decoded")]
    Decode(#[from] reqwest::Error),
    #[error("local OpenAI-compatible provider's OpenCode sidecar could not be started: {0}")]
    OpenCode(#[from] OpenCodeError),
    #[error("local OpenAI-compatible provider's Responses API response was malformed: {0}")]
    #[allow(dead_code)]
    ResponsesShape(String),
}

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    stream: bool,
}

#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

/// Request body for the OpenAI Responses API (`POST /v1/responses`).
/// We carry the input as a heterogeneous list of items because the
/// Responses API round-trips reasoning items as peers of messages;
/// a plain `Vec<ChatMessage>` would lose the reasoning side.
#[derive(Serialize)]
#[allow(dead_code)]
struct ResponsesApiRequest {
    model: String,
    input: Vec<InputItem>,
    temperature: f32,
    stream: bool,
}

/// A single entry in the Responses API `input` array.
///
/// We accept only the two shapes we need: a user message and a
/// reasoning item that carries forward the previous turn's
/// reasoning context. Other item types (function_call_output, etc.)
/// can be added when callers need them.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
enum InputItem {
    /// `{"type": "message", "role": "user", "content": "..."}`.
    /// The role is currently always `user`; system prompts are
    /// inlined as a leading `user` message in this implementation.
    Message { role: String, content: String },
    /// `{"type": "reasoning", "id": "rs_...", "encrypted_content": "..."}`.
    /// Round-tripped from the previous turn's `output` array so the
    /// model can continue reasoning chains that include
    /// `encrypted_content` (the OpenAI Responses API requires
    /// encrypted reasoning to be carried back verbatim for
    /// continued tool-using flows).
    Reasoning {
        #[serde(rename = "id")]
        id: String,
        /// Encrypted content is opaque to us; we never decrypt it
        /// locally. We must send back the exact bytes the server
        /// returned, so the field is stored and forwarded as
        /// `Option<String>` to allow older providers that do not
        /// support reasoning carryover to omit it.
        #[serde(skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
    },
}

/// A reasoning item returned by a Responses API provider.
/// Stored in the per-conversation cache and round-tripped on the
/// next turn.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[allow(dead_code)]
pub(crate) struct ReasoningItem {
    pub id: String,
    /// Encrypted content is opaque; we forward it verbatim on the
    /// next turn. `None` is allowed for providers that surface only
    /// the reasoning identifier and expect us to drop the
    /// reasoning chain.
    pub encrypted_content: Option<String>,
}

/// Response body for the OpenAI Responses API. We only model the
/// fields we use; unknown fields are silently ignored so that
/// future Responses API additions do not break parsing.
#[derive(Deserialize)]
#[allow(dead_code)]
struct ResponsesApiResponse {
    /// The `output` array, which is the Responses API's analogue of
    /// `choices`. The model places the assistant's reply and any
    /// reasoning items here.
    output: Vec<ResponsesOutputItem>,
}

/// One entry in the Responses API `output` array. The only shape
/// we currently handle is the reasoning item; the assistant's
/// text reply is left for callers that need it.
#[derive(Deserialize)]
#[allow(dead_code)]
struct ResponsesOutputItem {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    encrypted_content: Option<String>,
    /// Plain text content for non-reasoning output items. We
    /// currently only look at reasoning items; this field is
    /// parsed for forward-compat.
    #[serde(default)]
    content: Vec<ResponsesOutputContent>,
}

/// A content entry in a Responses API output item.
#[derive(Deserialize)]
#[allow(dead_code)]
struct ResponsesOutputContent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
}

#[derive(Deserialize)]
struct ChatCompletionChoice {
    message: ChatCompletionMessage,
}

#[derive(Deserialize)]
struct ChatCompletionMessage {
    content: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CommandGenerationJson {
    Wrapped {
        commands: Vec<LocalGeneratedCommand>,
    },
    Bare(Vec<LocalGeneratedCommand>),
}

#[derive(Deserialize)]
struct LocalGeneratedCommand {
    command: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    parameters: Vec<LocalGeneratedCommandParameter>,
}

#[derive(Deserialize)]
struct LocalGeneratedCommandParameter {
    #[serde(alias = "name")]
    id: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct LocalGeneratedMetadata {
    #[serde(alias = "parameterized_command")]
    command: String,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default, alias = "parameters")]
    arguments: Vec<LocalGeneratedArgument>,
}

#[derive(Deserialize)]
struct LocalGeneratedArgument {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default, alias = "value")]
    default_value: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PredictionJson {
    Suggestion { suggestion: String },
    Command { command: String },
    String(String),
}

/// Upper bound for the local-provider request timeout (1 hour). Values loaded from
/// `settings.toml` or set programmatically bypass the UI validation, so every call
/// path clamps here to keep `reqwest`/`tokio` timer math from overflowing.
pub(crate) const MAX_LOCAL_OPENAI_TIMEOUT_MS: u64 = 3_600_000;

fn clamp_timeout(mut config: LocalOpenAISettingsSnapshot) -> LocalOpenAISettingsSnapshot {
    config.timeout_ms = config.timeout_ms.clamp(1, MAX_LOCAL_OPENAI_TIMEOUT_MS);
    config
}

impl LocalOpenAIClient {
    pub(crate) fn new(config: LocalOpenAISettingsSnapshot) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            opencode_pool: OpenCodeSidecarPool::new(),
            opencode_working_dir: Arc::new(RwLock::new(None)),
            reasoning_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn set_config(&self, config: LocalOpenAISettingsSnapshot) {
        let old_config = self.config.read().clone();
        let provider_kind = config.provider_kind;
        *self.config.write() = config;
        // Clear the sidecar pool when:
        // 1. Switching away from OpenCode (avoid leaking unused children), or
        // 2. OpenCode settings change (command/args) so stale sidecars are dropped.
        // `clear()` is synchronous (parking_lot lock) so this runs directly on
        // the UI thread — no Tokio runtime required, and no fire-and-forget task
        // that could be dropped before it runs.
        let should_clear = provider_kind != LocalProviderKind::OpenCode
            || (provider_kind == LocalProviderKind::OpenCode
                && (old_config.opencode_command != self.config.read().opencode_command
                    || old_config.opencode_args != self.config.read().opencode_args));
        if should_clear {
            self.opencode_pool.clear();
        }
    }

    /// Override the working directory the OpenCode sidecar pool keys on.
    /// Pass `None` to use the process current working directory at
    /// request time.
    #[allow(dead_code)]
    pub(crate) fn set_opencode_working_dir(&self, cwd: Option<PathBuf>) {
        *self.opencode_working_dir.write() = cwd;
    }

    pub(crate) async fn generate_commands_from_natural_language(
        &self,
        http_client: &http_client::Client,
        prompt: String,
        ai_execution_context: Option<WarpAiExecutionContext>,
    ) -> Result<Vec<AIGeneratedCommand>, LocalOpenAIError> {
        let config = self.configured_for_command()?;
        let mut user_prompt = format!("User request:\n{prompt}");
        if let Some(context) = ai_execution_context.and_then(|context| context.to_json_string()) {
            user_prompt.push_str("\n\nExecution context JSON:\n");
            user_prompt.push_str(&context);
        }

        let content = self
            .chat_completion(
                http_client,
                &config,
                config.command_model.clone(),
                vec![
                    ChatMessage {
                        role: "system",
                        content: "Translate natural language into shell commands. Return only JSON in this shape: {\"commands\":[{\"command\":\"...\",\"description\":\"...\",\"parameters\":[{\"id\":\"name\",\"description\":\"...\"}]}]}. Use an empty parameters array when no parameters are needed.".to_string(),
                    },
                    ChatMessage {
                        role: "user",
                        content: user_prompt,
                    },
                ],
            )
            .await?;

        let parsed: CommandGenerationJson = parse_json_content(&content)?;
        let commands = match parsed {
            CommandGenerationJson::Wrapped { commands } | CommandGenerationJson::Bare(commands) => {
                commands
            }
        };

        Ok(commands
            .into_iter()
            .map(|command| {
                AIGeneratedCommand::new(
                    command.command.clone(),
                    if command.description.is_empty() {
                        command.command
                    } else {
                        command.description
                    },
                    command
                        .parameters
                        .into_iter()
                        .map(|parameter| (parameter.id, parameter.description))
                        .collect(),
                )
            })
            .collect())
    }

    pub(crate) async fn generate_metadata_for_command(
        &self,
        http_client: &http_client::Client,
        command: String,
    ) -> Result<GeneratedCommandMetadata, LocalOpenAIError> {
        let config = self.configured_for_command()?;
        let content = self
            .chat_completion(
                http_client,
                &config,
                config.command_model.clone(),
                vec![
                    ChatMessage {
                        role: "system",
                        content: "Generate workflow metadata for a shell command. Return only JSON in this shape: {\"command\":\"parameterized command\",\"title\":\"short title\",\"description\":\"one sentence\",\"arguments\":[{\"name\":\"arg\",\"description\":\"...\",\"default_value\":\"\"}]}.".to_string(),
                    },
                    ChatMessage {
                        role: "user",
                        content: command,
                    },
                ],
            )
            .await?;

        let metadata: LocalGeneratedMetadata = parse_json_content(&content)?;
        Ok(GeneratedCommandMetadata {
            command: metadata.command,
            title: metadata.title,
            description: metadata.description,
            arguments: metadata
                .arguments
                .into_iter()
                .map(|argument| GeneratedArgument {
                    name: argument.name,
                    description: argument.description,
                    default_value: argument.default_value,
                })
                .collect(),
        })
    }

    pub(crate) async fn predict_am_queries(
        &self,
        http_client: &http_client::Client,
        request: &PredictAMQueriesRequest,
    ) -> Result<PredictAMQueriesResponse, LocalOpenAIError> {
        let config = self.configured_for_prediction()?;
        let content = self
            .chat_completion(
                http_client,
                &config,
                config.prediction_model.clone(),
                vec![
                    ChatMessage {
                        role: "system",
                        content: "Predict the next natural-language agent query from recent terminal context. Return only JSON in this shape: {\"suggestion\":\"...\"}.".to_string(),
                    },
                    ChatMessage {
                        role: "user",
                        content: serde_json::to_string(request)?,
                    },
                ],
            )
            .await?;
        let prediction: PredictionJson = parse_json_content(&content)?;
        Ok(PredictAMQueriesResponse {
            suggestion: prediction.into_string(),
        })
    }

    pub(crate) async fn generate_ai_input_suggestions(
        &self,
        http_client: &http_client::Client,
        request: &GenerateAIInputSuggestionsRequest,
    ) -> Result<GenerateAIInputSuggestionsResponseV2, LocalOpenAIError> {
        let config = self.configured_for_prediction()?;
        let content = self
            .chat_completion(
                http_client,
                &config,
                config.prediction_model.clone(),
                vec![
                    ChatMessage {
                        role: "system",
                        content: "Predict the next shell command from terminal context and command history. Return only JSON in this shape: {\"command\":\"...\"}.".to_string(),
                    },
                    ChatMessage {
                        role: "user",
                        content: serde_json::to_string(request)?,
                    },
                ],
            )
            .await?;
        let prediction: PredictionJson = parse_json_content(&content)?;
        let command = prediction.into_string();
        Ok(GenerateAIInputSuggestionsResponseV2 {
            commands: if command.is_empty() {
                vec![]
            } else {
                vec![command.clone()]
            },
            ai_queries: vec![],
            most_likely_action: command,
        })
    }

    async fn chat_completion(
        &self,
        http_client: &http_client::Client,
        config: &LocalOpenAISettingsSnapshot,
        model: String,
        messages: Vec<ChatMessage>,
    ) -> Result<String, LocalOpenAIError> {
        let (base_url, api_key) = match config.provider_kind {
            LocalProviderKind::OpenAICompatible => (config.base_url.clone(), config.api_key.clone()),
            LocalProviderKind::OpenCode => {
                let working_dir = self
                    .opencode_working_dir
                    .read()
                    .clone()
                    .or_else(|| std::env::current_dir().ok())
                    .ok_or(OpenCodeError::NoWorkingDirectory)?;
                let sidecar = self
                    .opencode_pool
                    .get_or_spawn(
                        &config.opencode_command,
                        &config.opencode_args,
                        &working_dir,
                    )
                    .await?;
                // OpenCode ships an OpenAI-compatible /v1/chat/completions
                // endpoint without an API key by default. We still pass
                // the user-configured key through if one is set, since
                // some self-hosted OpenCode builds gate `/v1` behind a
                // bearer token.
                (sidecar.base_url().to_string(), config.api_key.clone())
            }
        };

        let mut request_builder = http_client
            .post(chat_completions_url(&base_url))
            .timeout(Duration::from_millis(config.timeout_ms))
            .json(&ChatCompletionRequest {
                model,
                messages,
                temperature: 0.2,
                stream: false,
            });

        if !api_key.trim().is_empty() {
            request_builder = request_builder.bearer_auth(api_key.trim());
        }

        let response: ChatCompletionResponse = request_builder
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        response
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or(LocalOpenAIError::EmptyResponse)
    }

    /// Issue a Responses API request to the local provider, optionally
    /// carrying forward reasoning items from prior turns for the
    /// same conversation.
    ///
    /// Returns the assistant's plain text reply. Reasoning items
    /// returned by the provider are stored in the per-conversation
    /// cache for the next call.
    #[allow(dead_code)]
    pub(crate) async fn chat_completion_responses(
        &self,
        http_client: &http_client::Client,
        conversation_id: &str,
        user_message: String,
        system_message: Option<String>,
    ) -> Result<String, LocalOpenAIError> {
        let config = self.configured()?;
        if !config.use_responses_api {
            return Err(LocalOpenAIError::ResponsesShape(
                "responses API is not enabled in local_openai settings".to_string(),
            ));
        }

        let (base_url, api_key) = self.responses_endpoint(&config).await?;

        // Build the input list: system prompt (if any) -> cached
        // reasoning items -> user message. The Responses API treats
        // system prompts as a leading user message with developer
        // role, but for cross-provider compatibility we just inline
        // the system text as a user message; local providers
        // generally do not honor the `developer` role anyway.
        let mut input: Vec<InputItem> = Vec::new();
        if let Some(system) = system_message {
            input.push(InputItem::Message {
                role: "user".to_string(),
                content: system,
            });
        }
        {
            let cache = self.reasoning_cache.lock();
            if let Some(items) = cache.get(conversation_id) {
                for item in items {
                    input.push(InputItem::Reasoning {
                        id: item.id.clone(),
                        encrypted_content: item.encrypted_content.clone(),
                    });
                }
            }
        }
        input.push(InputItem::Message {
            role: "user".to_string(),
            content: user_message,
        });

        let mut request_builder = http_client
            .post(responses_url(&base_url))
            .timeout(Duration::from_millis(config.timeout_ms))
            .json(&ResponsesApiRequest {
                model: config.command_model.clone(),
                input,
                temperature: 0.2,
                stream: false,
            });

        if !api_key.trim().is_empty() {
            request_builder = request_builder.bearer_auth(api_key.trim());
        }

        let response: ResponsesApiResponse = request_builder
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        // Update the cache with any reasoning items the server
        // returned. Old reasoning items are kept alongside new ones
        // so the conversation's full chain is preserved; providers
        // that trim their own output are responsible for not
        // returning stale items.
        let new_items: Vec<ReasoningItem> = response
            .output
            .iter()
            .filter_map(|item| {
                if item.kind == "reasoning" {
                    item.id.as_ref().map(|id| ReasoningItem {
                        id: id.clone(),
                        encrypted_content: item.encrypted_content.clone(),
                    })
                } else {
                    None
                }
            })
            .collect();
        if !new_items.is_empty() {
            self.reasoning_cache
                .lock()
                .entry(conversation_id.to_string())
                .or_default()
                .extend(new_items);
        }

        // Extract the first text reply from the output array. The
        // Responses API places non-reasoning items in `output[i]`
        // with `content` blocks; we look for the first text content.
        response
            .output
            .into_iter()
            .find_map(|item| {
                item.content.into_iter().find_map(|c| match (c.kind.as_str(), c.text) {
                    ("output_text", Some(text)) => Some(text),
                    _ => None,
                })
            })
            .ok_or_else(|| {
                LocalOpenAIError::ResponsesShape(
                    "responses API output had no `output_text` content".to_string(),
                )
            })
    }

    /// Drop the cached reasoning items for a conversation. Callers
    /// should invoke this when a conversation ends or is reset.
    #[allow(dead_code)]
    pub(crate) fn clear_reasoning_state(&self, conversation_id: &str) {
        self.reasoning_cache.lock().remove(conversation_id);
    }

    /// Resolve the base URL and API key for a Responses API request.
    /// Mirrors the dispatch in [`Self::chat_completion`] but returns
    /// the raw tuple so the caller can pick the right path
    /// (`/v1/responses` vs `/v1/chat/completions`).
    #[allow(dead_code)]
    async fn responses_endpoint(
        &self,
        config: &LocalOpenAISettingsSnapshot,
    ) -> Result<(String, String), LocalOpenAIError> {
        match config.provider_kind {
            LocalProviderKind::OpenAICompatible => {
                Ok((config.base_url.clone(), config.api_key.clone()))
            }
            LocalProviderKind::OpenCode => {
                let working_dir = self
                    .opencode_working_dir
                    .read()
                    .clone()
                    .or_else(|| std::env::current_dir().ok())
                    .ok_or_else(|| {
                        OpenCodeError::BadAnnouncement(
                            "no working directory available for OpenCode sidecar".to_string(),
                        )
                    })?;
                let sidecar = self.opencode_pool
                    .get_or_spawn(&config.opencode_command, &config.opencode_args, &working_dir)
                    .await?;
                Ok((sidecar.base_url().to_string(), config.api_key.clone()))
            }
        }
    }

    fn configured(&self) -> Result<LocalOpenAISettingsSnapshot, LocalOpenAIError> {
    /// Returns the current snapshot if it is runnable for the **command** path
    /// (natural-language → commands, command metadata). The `prediction_model`
    /// may be empty — only `command_model` is required here so the command path
    /// doesn't get blocked by an unset prediction model.
    fn configured_for_command(&self) -> Result<LocalOpenAISettingsSnapshot, LocalOpenAIError> {
        let config = self.config.read();
        if !config.enabled
            || config.command_model.trim().is_empty()
        {
            return Err(LocalOpenAIError::NotConfigured);
        }
        // Clone only once validated, and clamp the timeout for settings-file values.
        Ok(clamp_timeout(config.clone()))
    }

    /// Returns the current snapshot if it is runnable for the **prediction** path
    /// (agent-mode query prediction, AI input suggestions). Requires the
    /// `prediction_model` to be set.
    fn configured_for_prediction(&self) -> Result<LocalOpenAISettingsSnapshot, LocalOpenAIError> {
        let config = self.config.read();
        if !config.enabled
            || config.base_url.trim().is_empty()
            || config.prediction_model.trim().is_empty()
        {
            return Err(LocalOpenAIError::NotConfigured);
        }
        match config.provider_kind {
            LocalProviderKind::OpenAICompatible => {
                if config.base_url.trim().is_empty() {
                    return Err(LocalOpenAIError::NotConfigured);
                }
            }
            LocalProviderKind::OpenCode => {
                if config.opencode_command.trim().is_empty() {
                    return Err(LocalOpenAIError::NotConfigured);
                }
            }
        }
        // Clone only once validated, and clamp the timeout for settings-file values.
        Ok(clamp_timeout(config.clone()))
    }
}

impl PredictionJson {
    fn into_string(self) -> String {
        match self {
            Self::Suggestion { suggestion } => suggestion,
            Self::Command { command } => command,
            Self::String(value) => value,
        }
    }
}

fn chat_completions_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else if trimmed.ends_with("/v1") {
        format!("{trimmed}/chat/completions")
    } else {
        format!("{trimmed}/v1/chat/completions")
    }
}

/// Build the URL for the OpenAI Responses API. Mirrors
/// `chat_completions_url` so the two helpers stay in sync: the
/// Responses API uses `/v1/responses` rather than
/// `/v1/chat/completions`, and we accept either suffix or a bare
/// `/v1` prefix.
#[allow(dead_code)]
fn responses_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/responses") {
        trimmed.to_string()
    } else if trimmed.ends_with("/v1") {
        format!("{trimmed}/responses")
    } else {
        format!("{trimmed}/v1/responses")
    }
}

fn parse_json_content<T: DeserializeOwned>(content: &str) -> Result<T, serde_json::Error> {
    let trimmed = content.trim();
    if let Ok(value) = serde_json::from_str(trimmed) {
        return Ok(value);
    }

    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    if let Ok(value) = serde_json::from_str(unfenced) {
        return Ok(value);
    }

    let json_start = unfenced.find(['{', '[']).unwrap_or(0);
    let json_end = unfenced
        .rfind(['}', ']'])
        .map(|index| index + 1)
        .unwrap_or(unfenced.len());
    // Guard against malformed output where a closing bracket precedes the opening one
    // (`json_start > json_end`), which would panic on the slice.
    if json_start < json_end {
        serde_json::from_str(&unfenced[json_start..json_end])
    } else {
        serde_json::from_str(unfenced)
    }
}

impl From<LocalOpenAIError> for crate::ai_assistant::GenerateCommandsFromNaturalLanguageError {
    fn from(value: LocalOpenAIError) -> Self {
        match value {
            LocalOpenAIError::NotConfigured => Self::LocalProviderNotConfigured,
            LocalOpenAIError::InvalidJson(_) | LocalOpenAIError::EmptyResponse => {
                Self::LocalProviderInvalidResponse
            }
            LocalOpenAIError::Request(_)
            | LocalOpenAIError::Status(_)
            | LocalOpenAIError::Decode(_)
            | LocalOpenAIError::OpenCode(_)
            | LocalOpenAIError::ResponsesShape(_) => Self::LocalProviderError,
        }
    }
}

impl From<LocalOpenAIError> for crate::drive::workflows::ai_assist::GeneratedCommandMetadataError {
    fn from(value: LocalOpenAIError) -> Self {
        match value {
            LocalOpenAIError::NotConfigured => Self::LocalProviderNotConfigured,
            LocalOpenAIError::InvalidJson(_) | LocalOpenAIError::EmptyResponse => {
                Self::LocalProviderInvalidResponse
            }
            LocalOpenAIError::Request(_)
            | LocalOpenAIError::Status(_)
            | LocalOpenAIError::Decode(_)
            | LocalOpenAIError::OpenCode(_)
            | LocalOpenAIError::ResponsesShape(_) => Self::LocalProviderError,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_chat_completion_urls() {
        assert_eq!(
            chat_completions_url("http://127.0.0.1:11434/v1"),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("http://127.0.0.1:11434"),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("http://127.0.0.1:11434/v1/chat/completions"),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
    }

    #[test]
    fn parses_fenced_json_content() {
        let parsed: PredictionJson =
            parse_json_content("```json\n{\"suggestion\":\"git status\"}\n```").unwrap();
        assert_eq!(parsed.into_string(), "git status");
    }
}
