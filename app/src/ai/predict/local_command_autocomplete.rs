use serde::{Deserialize, Serialize};
use url::Url;
use warp_completer::completer::CompletionContext;

use crate::completer::SessionContext;

const MAX_FILE_CANDIDATES: usize = 80;
const OPENAI_BASE_URL_HEADER: &str = "X-Warp-OpenAI-Base-URL";
const LOCAL_MODEL_ALIASES_HEADER: &str = "X-Warp-Local-Model-Aliases";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalCommandAutocompleteProviderSettings {
    pub openai_api_key: Option<String>,
    pub openai_base_url: Option<String>,
    pub local_model_aliases: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutocompleteBlockContext {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalCommandAutocompleteRequest {
    pub prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_blocks: Vec<AutocompleteBlockContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_candidates: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalCommandAutocompleteResponse {
    pub commands: Vec<String>,
    pub most_likely_action: String,
    pub raw_output: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub parse_status: String,
}

impl LocalCommandAutocompleteResponse {
    pub fn command_suggestion(&self, prefix: &str) -> Option<&str> {
        let command = self.most_likely_action.trim();
        if command.is_empty()
            || command.contains('\n')
            || command.contains('\r')
            || command == prefix
            || !command.starts_with(prefix)
        {
            return None;
        }
        Some(command)
    }

    pub fn should_skip_completion_spec_validation(&self) -> bool {
        matches!(self.source.as_str(), "deterministic" | "fallback")
    }
}

pub async fn request_local_command_autocomplete(
    root_url: Url,
    request: &LocalCommandAutocompleteRequest,
    provider_settings: LocalCommandAutocompleteProviderSettings,
) -> anyhow::Result<LocalCommandAutocompleteResponse> {
    let client = reqwest::Client::new();
    let url = format!(
        "{}/ai/local-command-autocomplete",
        root_url.as_str().trim_end_matches('/')
    );
    let mut request_builder = client.post(url).json(request);
    if let Some(openai_api_key) = provider_settings.openai_api_key.as_deref() {
        request_builder = request_builder.bearer_auth(openai_api_key);
    }
    if let Some(openai_base_url) = provider_settings.openai_base_url.as_deref() {
        request_builder = request_builder.header(OPENAI_BASE_URL_HEADER, openai_base_url);
    }
    if let Some(local_model_aliases) = provider_settings.local_model_aliases.as_deref() {
        request_builder = request_builder.header(LOCAL_MODEL_ALIASES_HEADER, local_model_aliases);
    }
    Ok(request_builder
        .send()
        .await?
        .error_for_status()?
        .json::<LocalCommandAutocompleteResponse>()
        .await?)
}

pub async fn file_candidates_for_context(completion_context: &SessionContext) -> Vec<String> {
    let Some(path_context) = completion_context.path_completion_context() else {
        return Vec::new();
    };
    let entries = path_context
        .list_directory_entries(path_context.pwd().to_path_buf())
        .await;
    entries
        .iter()
        .take(MAX_FILE_CANDIDATES)
        .map(|entry| entry.file_name.clone())
        .collect()
}

#[cfg(test)]
#[path = "local_command_autocomplete_tests.rs"]
mod tests;
