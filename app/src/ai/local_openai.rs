use std::{sync::Arc, time::Duration};

use parking_lot::RwLock;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{
    ai::predict::{
        generate_ai_input_suggestions::{
            GenerateAIInputSuggestionsRequest, GenerateAIInputSuggestionsResponseV2,
        },
        predict_am_queries::{PredictAMQueriesRequest, PredictAMQueriesResponse},
    },
    ai_assistant::{execution_context::WarpAiExecutionContext, AIGeneratedCommand},
    drive::workflows::ai_assist::{GeneratedArgument, GeneratedCommandMetadata},
    settings::LocalOpenAISettingsSnapshot,
};

#[derive(Clone)]
pub(crate) struct LocalOpenAIClient {
    config: Arc<RwLock<LocalOpenAISettingsSnapshot>>,
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

impl LocalOpenAIClient {
    pub(crate) fn new(config: LocalOpenAISettingsSnapshot) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
        }
    }

    pub(crate) fn set_config(&self, config: LocalOpenAISettingsSnapshot) {
        *self.config.write() = config;
    }

    pub(crate) async fn generate_commands_from_natural_language(
        &self,
        http_client: &http_client::Client,
        prompt: String,
        ai_execution_context: Option<WarpAiExecutionContext>,
    ) -> Result<Vec<AIGeneratedCommand>, LocalOpenAIError> {
        let config = self.configured()?;
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
        let config = self.configured()?;
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
        let config = self.configured()?;
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
        let config = self.configured()?;
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
        let mut request_builder = http_client
            .post(chat_completions_url(&config.base_url))
            .timeout(Duration::from_millis(config.timeout_ms))
            .json(&ChatCompletionRequest {
                model,
                messages,
                temperature: 0.2,
                stream: false,
            });

        if !config.api_key.trim().is_empty() {
            request_builder = request_builder.bearer_auth(config.api_key.trim());
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

    fn configured(&self) -> Result<LocalOpenAISettingsSnapshot, LocalOpenAIError> {
        let config = self.config.read().clone();
        if !config.enabled
            || config.base_url.trim().is_empty()
            || config.command_model.trim().is_empty()
            || config.prediction_model.trim().is_empty()
        {
            return Err(LocalOpenAIError::NotConfigured);
        }
        Ok(config)
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
            | LocalOpenAIError::Decode(_) => Self::LocalProviderError,
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
            | LocalOpenAIError::Decode(_) => Self::LocalProviderError,
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
