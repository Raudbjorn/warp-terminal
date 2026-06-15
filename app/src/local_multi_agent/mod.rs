#![cfg(not(target_family = "wasm"))]

use std::{
    collections::{BTreeMap, BTreeSet},
    net::TcpListener,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use async_process::{Child, Stdio};
use command::r#async::Command;
use futures::stream::AbortHandle;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;
use warp_core::user_preferences::GetUserPreferences;
use warpui::{r#async::Timer, AppContext, Entity, ModelContext, SingletonEntity};

use crate::{
    ai::llms::LLMPreferences,
    server::server_api::{no_cloud_mode_enabled, LOCAL_NO_CLOUD_SERVER_ROOT_URL},
    ChannelState,
};

const PREF_KEY: &str = "LocalMultiAgentConfig";
const BUNDLED_SERVICE_DIR: &str = "bundled/local-multi-agent";
#[cfg(windows)]
const SERVICE_BINARY_NAME: &str = "warp-local-multi-agent.exe";
#[cfg(not(windows))]
const SERVICE_BINARY_NAME: &str = "warp-local-multi-agent";
const CONFIG_SCHEMA_JSON: &str =
    include_str!("../../../crates/local_multi_agent_service/config-schema.json");
const RESTART_DEBOUNCE: Duration = Duration::from_millis(500);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_LOCAL_MAX_COMPLETION_TOKENS: u32 = 2048;

pub const LOCAL_MODEL_ALIAS_IDS: [&str; 5] = [
    "auto",
    "auto-efficient",
    "auto-coding",
    "auto-reasoning",
    "auto-autocomplete",
];

static LOCAL_ROOT_URL: LazyLock<parking_lot::RwLock<Option<Url>>> =
    LazyLock::new(|| parking_lot::RwLock::new(None));

#[derive(Debug, Clone, Deserialize)]
struct LocalMultiAgentConfigSchema {
    defaults: LocalMultiAgentConfigSchemaDefaults,
}

#[derive(Debug, Clone, Deserialize)]
struct LocalMultiAgentConfigSchemaDefaults {
    #[serde(rename = "HOST")]
    host: String,
    #[serde(rename = "PORT")]
    port: u16,
    #[serde(rename = "OPENAI_BASE_URL")]
    openai_base_url: String,
    #[serde(rename = "OPENAI_MODEL")]
    openai_model: String,
    #[serde(rename = "LOCAL_MODEL_ALIASES")]
    local_model_aliases: String,
    #[serde(rename = "LOCAL_MODEL_LIST")]
    local_model_list: String,
    #[serde(rename = "LOCAL_ENABLE_TOOLS")]
    local_enable_tools: bool,
    #[serde(rename = "LOCAL_MAX_HISTORY_MESSAGES")]
    local_max_history_messages: u16,
    #[serde(rename = "LOCAL_MAX_COMPLETION_TOKENS")]
    local_max_completion_tokens: u32,
    #[serde(rename = "LOCAL_MODEL_CONTEXT_TOKENS")]
    local_model_context_tokens: String,
    #[serde(rename = "LOCAL_GRAPHQL_DB_PATH")]
    local_graphql_db_path: String,
    #[serde(rename = "LOG_LEVEL")]
    log_level: String,
    #[serde(rename = "LOCAL_SERVICE_LOG_PATH")]
    local_service_log_path: String,
    #[serde(rename = "LOCAL_MULTI_AGENT_SYSTEM_PROMPT")]
    local_multi_agent_system_prompt: String,
}

fn config_schema_defaults() -> LocalMultiAgentConfigSchemaDefaults {
    serde_json::from_str::<LocalMultiAgentConfigSchema>(CONFIG_SCHEMA_JSON)
        .expect("local multi-agent config schema must be valid JSON")
        .defaults
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalMultiAgentConfig {
    pub host: String,
    pub port: u16,
    pub openai_base_url: Option<String>,
    pub openai_model: Option<String>,
    pub local_model_aliases: String,
    pub local_model_list: String,
    pub local_enable_tools: bool,
    pub local_max_history_messages: u16,
    #[serde(default = "default_local_max_completion_tokens")]
    pub local_max_completion_tokens: u32,
    pub local_model_context_tokens: Option<String>,
    pub local_graphql_db_path: Option<String>,
    pub log_level: String,
    pub local_service_log_path: Option<String>,
    pub local_multi_agent_system_prompt: String,
}

impl Default for LocalMultiAgentConfig {
    fn default() -> Self {
        let defaults = config_schema_defaults();
        Self {
            host: defaults.host,
            port: defaults.port,
            openai_base_url: non_empty(defaults.openai_base_url),
            openai_model: non_empty(defaults.openai_model),
            local_model_aliases: defaults.local_model_aliases,
            local_model_list: defaults.local_model_list,
            local_enable_tools: defaults.local_enable_tools,
            local_max_history_messages: defaults.local_max_history_messages,
            local_max_completion_tokens: defaults.local_max_completion_tokens,
            local_model_context_tokens: non_empty(defaults.local_model_context_tokens),
            local_graphql_db_path: non_empty(defaults.local_graphql_db_path),
            log_level: defaults.log_level,
            local_service_log_path: non_empty(defaults.local_service_log_path),
            local_multi_agent_system_prompt: defaults.local_multi_agent_system_prompt,
        }
    }
}

impl LocalMultiAgentConfig {
    pub fn root_url(&self) -> Result<Url, LocalMultiAgentConfigError> {
        Url::parse(&format!("http://{}:{}", self.host.trim(), self.port))
            .map_err(|source| LocalMultiAgentConfigError::InvalidRootUrl { source })
    }

    pub fn validate(&self) -> Result<(), LocalMultiAgentConfigError> {
        if self.host.trim().is_empty() {
            return Err(LocalMultiAgentConfigError::InvalidHost);
        }
        if self.port == 0 {
            return Err(LocalMultiAgentConfigError::InvalidPort);
        }
        if self.local_max_history_messages < 4 {
            return Err(LocalMultiAgentConfigError::InvalidMaxHistoryMessages);
        }
        if self.local_max_completion_tokens == 0 {
            return Err(LocalMultiAgentConfigError::InvalidMaxCompletionTokens);
        }
        if !matches!(
            self.log_level.trim().to_ascii_lowercase().as_str(),
            "error" | "warn" | "info" | "debug"
        ) {
            return Err(LocalMultiAgentConfigError::InvalidLogLevel);
        }
        if let Some(base_url) = self.openai_base_url.as_deref().and_then(non_empty_str) {
            validate_absolute_http_url(base_url)?;
        }
        if !self.local_model_aliases.trim().is_empty() {
            let value: serde_json::Value = serde_json::from_str(&self.local_model_aliases)
                .map_err(|source| LocalMultiAgentConfigError::InvalidModelAliases { source })?;
            if !value.is_object() {
                return Err(LocalMultiAgentConfigError::ModelAliasesMustBeObject);
            }
        }
        if let Some(context_tokens) = &self.local_model_context_tokens {
            validate_context_tokens(context_tokens)?;
        }
        self.root_url()?;
        Ok(())
    }

    pub fn provider_base_url(&self) -> &str {
        self.openai_base_url
            .as_deref()
            .and_then(non_empty_str)
            .unwrap_or(DEFAULT_OPENAI_BASE_URL)
    }

    pub fn model_aliases(&self) -> Result<BTreeMap<String, String>, LocalMultiAgentConfigError> {
        parse_model_aliases(&self.local_model_aliases)
    }

    pub fn set_model_alias(
        &mut self,
        alias: &str,
        model: &str,
    ) -> Result<(), LocalMultiAgentConfigError> {
        let mut aliases = self.model_aliases()?;
        aliases.insert(alias.to_string(), model.to_string());
        self.local_model_aliases = serde_json::to_string(&aliases).unwrap_or_default();
        Ok(())
    }

    fn apply_discovered_models(
        &mut self,
        models: &[String],
    ) -> Result<bool, LocalMultiAgentConfigError> {
        let Some(first_model) = models.first() else {
            return Ok(false);
        };

        let mut changed = false;
        if self
            .openai_model
            .as_deref()
            .is_none_or(|model| !models.iter().any(|candidate| candidate == model))
        {
            self.openai_model = Some(first_model.clone());
            changed = true;
        }

        let model_list = models.join(",");
        if self.local_model_list != model_list {
            self.local_model_list = model_list;
            changed = true;
        }

        let mut aliases = self.model_aliases()?;
        for alias in LOCAL_MODEL_ALIAS_IDS {
            let alias_missing_or_invalid = aliases
                .get(alias)
                .is_none_or(|model| !models.iter().any(|candidate| candidate == model));
            if alias_missing_or_invalid {
                aliases.insert(alias.to_string(), first_model.clone());
                changed = true;
            }
        }
        if changed {
            self.local_model_aliases = serde_json::to_string(&aliases).unwrap_or_default();
        }

        Ok(changed)
    }

    pub fn model_choices(&self, discovered_models: &[String]) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut choices = Vec::new();
        for model in discovered_models
            .iter()
            .cloned()
            .chain(parse_model_list_ids(&self.local_model_list))
            .chain(self.openai_model.clone())
            .chain(
                self.model_aliases()
                    .unwrap_or_default()
                    .into_values()
                    .collect::<Vec<_>>(),
            )
        {
            let model = model.trim().to_string();
            if !model.is_empty() && seen.insert(model.clone()) {
                choices.push(model);
            }
        }
        choices
    }

    pub fn config_hash(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(json);
        hex::encode(&hasher.finalize()[..8])
    }

    pub fn env(&self, openai_api_key: Option<&str>) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert("HOST".to_string(), self.host.trim().to_string());
        env.insert("PORT".to_string(), self.port.to_string());
        env.insert(
            "WARP_URL_SCHEME".to_string(),
            ChannelState::url_scheme().to_string(),
        );
        env.insert(
            "LOCAL_ENABLE_TOOLS".to_string(),
            self.local_enable_tools.to_string(),
        );
        env.insert(
            "LOCAL_MAX_HISTORY_MESSAGES".to_string(),
            self.local_max_history_messages.to_string(),
        );
        env.insert(
            "LOCAL_MAX_COMPLETION_TOKENS".to_string(),
            self.local_max_completion_tokens.to_string(),
        );
        env.insert("LOG_LEVEL".to_string(), self.log_level.trim().to_string());
        env.insert(
            "LOCAL_MODEL_ALIASES".to_string(),
            self.local_model_aliases.trim().to_string(),
        );
        env.insert(
            "LOCAL_MODEL_LIST".to_string(),
            self.local_model_list.trim().to_string(),
        );
        env.insert(
            "LOCAL_GRAPHQL_DB_PATH".to_string(),
            self.local_graphql_db_path
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(default_graphql_db_path),
        );
        env.insert(
            "LOCAL_SERVICE_LOG_PATH".to_string(),
            self.local_service_log_path
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(default_log_path),
        );
        env.insert(
            "LOCAL_MULTI_AGENT_SYSTEM_PROMPT".to_string(),
            self.local_multi_agent_system_prompt.clone(),
        );
        if let Some(value) = self.openai_model.as_deref().and_then(non_empty_str) {
            env.insert("OPENAI_MODEL".to_string(), value.to_string());
        }
        if let Some(value) = self
            .local_model_context_tokens
            .as_deref()
            .and_then(non_empty_str)
        {
            env.insert("LOCAL_MODEL_CONTEXT_TOKENS".to_string(), value.to_string());
        }
        if let Some(value) = openai_api_key.and_then(non_empty_str) {
            env.insert("OPENAI_API_KEY".to_string(), value.to_string());
        }
        if let Some(value) = self.openai_base_url.as_deref().and_then(non_empty_str) {
            env.insert("OPENAI_BASE_URL".to_string(), value.to_string());
        }
        env
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LocalMultiAgentConfigError {
    #[error("Host is required.")]
    InvalidHost,
    #[error("Port must be between 1 and 65535.")]
    InvalidPort,
    #[error("Maximum history messages must be at least 4.")]
    InvalidMaxHistoryMessages,
    #[error("Maximum completion tokens must be a positive number.")]
    InvalidMaxCompletionTokens,
    #[error("Log level must be one of error, warn, info, or debug.")]
    InvalidLogLevel,
    #[error("OpenAI Base URL must be an absolute http(s) URL.")]
    InvalidOpenAiBaseUrl,
    #[error("Model aliases must be valid JSON: {source}")]
    InvalidModelAliases { source: serde_json::Error },
    #[error("Model aliases must be a JSON object.")]
    ModelAliasesMustBeObject,
    #[error("Context tokens must be a positive integer or a JSON object.")]
    InvalidContextTokens,
    #[error("Context tokens JSON is invalid: {source}")]
    InvalidContextTokensJson { source: serde_json::Error },
    #[error("Could not build local root URL: {source}")]
    InvalidRootUrl { source: url::ParseError },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalMultiAgentStatus {
    Disabled,
    Starting,
    Running {
        root_url: Url,
        pid: Option<u32>,
        config_hash: String,
    },
    Restarting,
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalMultiAgentTestStatus {
    NotRun,
    Testing,
    Passed { model_count: usize },
    Failed { message: String },
}

impl LocalMultiAgentStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Disabled => "Disabled".to_string(),
            Self::Starting => "Starting".to_string(),
            Self::Running { root_url, .. } => format!("Running at {}", root_url.as_str()),
            Self::Restarting => "Restarting".to_string(),
            Self::Failed { message } => format!("Failed: {message}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalMultiAgentManagerEvent {
    ConfigChanged,
    StatusChanged,
    TestStatusChanged,
}

#[derive(Debug, Deserialize)]
struct LocalMultiAgentHealthResponse {
    ok: bool,
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProviderModelsResponse {
    data: Vec<serde_json::Value>,
}

pub struct LocalMultiAgentManager {
    config: LocalMultiAgentConfig,
    status: LocalMultiAgentStatus,
    test_status: LocalMultiAgentTestStatus,
    discovered_models: Vec<String>,
    child: Option<Child>,
    restart_debounce: Option<AbortHandle>,
    openai_api_key: Option<String>,
}

impl LocalMultiAgentManager {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let config = load_config_with_legacy_migration(ctx);
        let status = if no_cloud_mode_enabled() {
            LocalMultiAgentStatus::Starting
        } else {
            LocalMultiAgentStatus::Disabled
        };
        if let Ok(root_url) = config.root_url() {
            set_global_root_url(Some(root_url.clone()));
            if no_cloud_mode_enabled() {
                let _ = ChannelState::override_server_root_url(root_url.to_string());
            }
        }

        let mut manager = Self {
            config,
            status,
            test_status: LocalMultiAgentTestStatus::NotRun,
            discovered_models: Vec::new(),
            child: None,
            restart_debounce: None,
            openai_api_key: None,
        };
        if no_cloud_mode_enabled() {
            manager.schedule_restart(ctx);
        }
        manager
    }

    pub fn config(&self) -> &LocalMultiAgentConfig {
        &self.config
    }

    pub fn status(&self) -> &LocalMultiAgentStatus {
        &self.status
    }

    pub fn test_status(&self) -> &LocalMultiAgentTestStatus {
        &self.test_status
    }

    pub fn discovered_models(&self) -> &[String] {
        &self.discovered_models
    }

    pub fn root_url(&self) -> Option<Url> {
        self.config.root_url().ok()
    }

    pub fn global_root_url() -> Option<Url> {
        LOCAL_ROOT_URL.read().clone()
    }

    pub fn update_provider_config(
        &mut self,
        openai_api_key: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.openai_api_key == openai_api_key {
            return;
        }
        self.openai_api_key = openai_api_key;
        self.schedule_restart(ctx);
    }

    pub fn set_config(
        &mut self,
        config: LocalMultiAgentConfig,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), LocalMultiAgentConfigError> {
        config.validate()?;
        if self.config == config {
            return Ok(());
        }
        self.config = config;
        save_config(&self.config, ctx);
        self.discovered_models.clear();
        self.test_status = LocalMultiAgentTestStatus::NotRun;
        if let Ok(root_url) = self.config.root_url() {
            set_global_root_url(Some(root_url.clone()));
            if no_cloud_mode_enabled() {
                let _ = ChannelState::override_server_root_url(root_url.to_string());
            }
        }
        ctx.emit(LocalMultiAgentManagerEvent::ConfigChanged);
        ctx.emit(LocalMultiAgentManagerEvent::TestStatusChanged);
        self.refresh_llm_preferences(ctx);
        self.schedule_restart(ctx);
        Ok(())
    }

    pub fn set_openai_model(
        &mut self,
        model: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), LocalMultiAgentConfigError> {
        let mut config = self.config.clone();
        config.openai_model = model.and_then(non_empty);
        config.validate()?;
        if self.config.openai_model == config.openai_model {
            return Ok(());
        }
        self.config = config;
        save_config(&self.config, ctx);
        ctx.emit(LocalMultiAgentManagerEvent::ConfigChanged);
        self.refresh_llm_preferences(ctx);
        self.schedule_restart(ctx);
        Ok(())
    }

    pub fn restart_with_config(&mut self, ctx: &mut ModelContext<Self>) {
        self.schedule_restart(ctx);
    }

    pub fn shutdown(&mut self, ctx: &mut ModelContext<Self>) {
        if let Some(handle) = self.restart_debounce.take() {
            handle.abort();
        }
        self.stop_child();
        self.status = LocalMultiAgentStatus::Disabled;
        set_global_root_url(None);
        ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
    }

    pub fn health_check(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(root_url) = self.root_url() else {
            self.set_failed("Invalid local service URL.".to_string(), ctx);
            return;
        };
        let current_status = self.status.clone();
        let _ = ctx.spawn(
            async move { health_check(root_url.clone()).await.map(|_| root_url) },
            move |manager, result, ctx| {
                match result {
                    Ok(root_url) => {
                        if !matches!(manager.status, LocalMultiAgentStatus::Running { .. }) {
                            manager.status = current_status;
                        }
                        log::info!("Local multi-agent health check succeeded at {root_url}");
                    }
                    Err(error) => {
                        manager.set_failed(format!("Health check failed: {error:#}"), ctx);
                    }
                }
                ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
            },
        );
    }

    pub fn test_backend(&mut self, ctx: &mut ModelContext<Self>) {
        if let Err(error) = self.config.validate() {
            self.test_status = LocalMultiAgentTestStatus::Failed {
                message: error.to_string(),
            };
            ctx.emit(LocalMultiAgentManagerEvent::TestStatusChanged);
            return;
        }

        let Some(root_url) = self.root_url() else {
            self.test_status = LocalMultiAgentTestStatus::Failed {
                message: "Invalid local service URL.".to_string(),
            };
            ctx.emit(LocalMultiAgentManagerEvent::TestStatusChanged);
            return;
        };

        let provider_base_url = self.config.provider_base_url().to_string();
        let openai_api_key = self.openai_api_key.clone();
        let service_config = self.config.clone();
        let service_openai_api_key = openai_api_key.clone();
        self.test_status = LocalMultiAgentTestStatus::Testing;
        ctx.emit(LocalMultiAgentManagerEvent::TestStatusChanged);

        let _ = ctx.spawn(
            async move {
                let start_result = match health_check(root_url.clone()).await {
                    Ok(()) => None,
                    Err(error) => {
                        log::info!(
                            "Local multi-agent service was not healthy during test; attempting restart: {error:#}"
                        );
                        Some(start_service(service_config, service_openai_api_key).await?)
                    }
                };
                let models =
                    fetch_provider_models(&provider_base_url, openai_api_key.as_deref()).await?;
                Ok::<_, anyhow::Error>((start_result, models))
            },
            |manager, result, ctx| {
                match result {
                    Ok((start_result, models)) => {
                        if let Some(start_result) = start_result {
                            manager.apply_start_result(start_result, ctx);
                            ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
                        }
                        let model_count = models.len();
                        manager.discovered_models = models.clone();
                        match manager.config.apply_discovered_models(&models) {
                            Ok(true) => {
                                save_config(&manager.config, ctx);
                                ctx.emit(LocalMultiAgentManagerEvent::ConfigChanged);
                                manager.schedule_restart(ctx);
                            }
                            Ok(false) => {}
                            Err(error) => {
                                manager.test_status = LocalMultiAgentTestStatus::Failed {
                                    message: error.to_string(),
                                };
                                ctx.emit(LocalMultiAgentManagerEvent::TestStatusChanged);
                                return;
                            }
                        }
                        manager.test_status = LocalMultiAgentTestStatus::Passed { model_count };
                        manager.refresh_llm_preferences(ctx);
                    }
                    Err(error) => {
                        manager.test_status = LocalMultiAgentTestStatus::Failed {
                            message: format!("{error:#}"),
                        };
                    }
                }
                ctx.emit(LocalMultiAgentManagerEvent::TestStatusChanged);
            },
        );
    }

    pub fn test_profile_backend(
        &mut self,
        profile_key: String,
        provider_base_url: Option<String>,
        openai_api_key: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        if let Err(error) = self.config.validate() {
            self.test_status = LocalMultiAgentTestStatus::Failed {
                message: error.to_string(),
            };
            ctx.emit(LocalMultiAgentManagerEvent::TestStatusChanged);
            return;
        }

        let Some(root_url) = self.root_url() else {
            self.test_status = LocalMultiAgentTestStatus::Failed {
                message: "Invalid local service URL.".to_string(),
            };
            ctx.emit(LocalMultiAgentManagerEvent::TestStatusChanged);
            return;
        };

        let provider_base_url = provider_base_url
            .as_deref()
            .and_then(non_empty_str)
            .unwrap_or(DEFAULT_OPENAI_BASE_URL)
            .to_string();
        let service_config = self.config.clone();
        let service_openai_api_key = openai_api_key.clone();
        self.test_status = LocalMultiAgentTestStatus::Testing;
        ctx.emit(LocalMultiAgentManagerEvent::TestStatusChanged);

        let _ = ctx.spawn(
            async move {
                let start_result = match health_check(root_url.clone()).await {
                    Ok(()) => None,
                    Err(error) => {
                        log::info!(
                            "Local multi-agent service was not healthy during profile test; attempting restart: {error:#}"
                        );
                        Some(start_service(service_config, service_openai_api_key).await?)
                    }
                };
                let models =
                    fetch_provider_models(&provider_base_url, openai_api_key.as_deref()).await?;
                Ok::<_, anyhow::Error>((start_result, models))
            },
            move |manager, result, ctx| {
                match result {
                    Ok((start_result, models)) => {
                        if let Some(start_result) = start_result {
                            manager.apply_start_result(start_result, ctx);
                            ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
                        }

                        let model_count = models.len();
                        manager.discovered_models = models.clone();

                        let profile_settings = ::ai::api_keys::ApiKeyManager::as_ref(ctx)
                            .keys()
                            .profile_settings(&profile_key);
                        let mut profile_config = manager.config.clone();
                        profile_config.local_model_aliases =
                            profile_settings.local_model_aliases.clone();
                        profile_config.local_model_list = profile_settings.local_model_list.clone();

                        match profile_config.apply_discovered_models(&models) {
                            Ok(_) => {
                                ::ai::api_keys::ApiKeyManager::handle(ctx).update(
                                    ctx,
                                    |api_keys, ctx| {
                                        api_keys.set_local_model_settings_for_profile(
                                            &profile_key,
                                            profile_config.local_model_aliases.clone(),
                                            profile_config.local_model_list.clone(),
                                            ctx,
                                        );
                                    },
                                );
                            }
                            Err(error) => {
                                manager.test_status = LocalMultiAgentTestStatus::Failed {
                                    message: error.to_string(),
                                };
                                ctx.emit(LocalMultiAgentManagerEvent::TestStatusChanged);
                                return;
                            }
                        }
                        manager.test_status = LocalMultiAgentTestStatus::Passed { model_count };
                        manager.refresh_llm_preferences(ctx);
                    }
                    Err(error) => {
                        manager.test_status = LocalMultiAgentTestStatus::Failed {
                            message: format!("{error:#}"),
                        };
                    }
                }
                ctx.emit(LocalMultiAgentManagerEvent::TestStatusChanged);
            },
        );
    }

    pub fn record_config_error(&mut self, message: String, ctx: &mut ModelContext<Self>) {
        self.status = LocalMultiAgentStatus::Failed { message };
        ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
    }

    fn schedule_restart(&mut self, ctx: &mut ModelContext<Self>) {
        if !no_cloud_mode_enabled() {
            self.stop_child();
            self.status = LocalMultiAgentStatus::Disabled;
            set_global_root_url(None);
            ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
            return;
        }

        if let Some(handle) = self.restart_debounce.take() {
            handle.abort();
        }

        self.status = LocalMultiAgentStatus::Restarting;
        ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
        self.restart_debounce = Some(
            ctx.spawn(
                async {
                    Timer::after(RESTART_DEBOUNCE).await;
                },
                |manager, _, ctx| {
                    manager.restart_debounce = None;
                    manager.start_now(ctx);
                },
            )
            .abort_handle(),
        );
    }

    fn start_now(&mut self, ctx: &mut ModelContext<Self>) {
        self.stop_child();
        let config = self.config.clone();
        let openai_api_key = self.openai_api_key.clone();
        self.status = LocalMultiAgentStatus::Starting;
        ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);

        let _ = ctx.spawn(
            async move { start_service(config, openai_api_key).await },
            |manager, result, ctx| {
                match result {
                    Ok(start_result) => {
                        manager.apply_start_result(start_result, ctx);
                    }
                    Err(error) => {
                        log::warn!("Failed to start local multi-agent service: {error:#}");
                        manager.set_failed(error.to_string(), ctx)
                    }
                }
                ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
            },
        );
    }

    fn apply_start_result(&mut self, result: StartServiceResult, ctx: &mut ModelContext<Self>) {
        let StartServiceResult {
            child,
            root_url,
            pid,
            config_hash,
        } = result;
        if child.is_some() {
            self.stop_child();
            self.child = child;
        }
        set_global_root_url(Some(root_url.clone()));
        let _ = ChannelState::override_server_root_url(root_url.to_string());
        self.status = LocalMultiAgentStatus::Running {
            root_url,
            pid,
            config_hash,
        };
        self.refresh_llm_preferences(ctx);
    }

    fn refresh_llm_preferences(&self, ctx: &mut ModelContext<Self>) {
        let config = self.config.clone();
        let discovered_models = self.discovered_models.clone();
        LLMPreferences::handle(ctx).update(ctx, |prefs, ctx| {
            prefs.update_local_multi_agent_models(&config, &discovered_models, ctx);
        });
    }

    fn stop_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
        }
    }

    fn set_failed(&mut self, message: String, ctx: &mut ModelContext<Self>) {
        self.stop_child();
        self.status = LocalMultiAgentStatus::Failed { message };
        ctx.emit(LocalMultiAgentManagerEvent::StatusChanged);
    }
}

impl Drop for LocalMultiAgentManager {
    fn drop(&mut self) {
        self.stop_child();
    }
}

impl Entity for LocalMultiAgentManager {
    type Event = LocalMultiAgentManagerEvent;
}

impl SingletonEntity for LocalMultiAgentManager {}

struct StartServiceResult {
    child: Option<Child>,
    root_url: Url,
    pid: Option<u32>,
    config_hash: String,
}

async fn start_service(
    config: LocalMultiAgentConfig,
    openai_api_key: Option<String>,
) -> Result<StartServiceResult> {
    config.validate()?;
    let root_url = config.root_url()?;
    let config_hash = config.config_hash();

    match health_check(root_url.clone()).await {
        Ok(()) => {
            log::info!("Reusing existing healthy local multi-agent service at {root_url}");
            return Ok(StartServiceResult {
                child: None,
                root_url,
                pid: None,
                config_hash,
            });
        }
        Err(_) => ensure_port_available(&config)?,
    }

    let service_binary =
        service_binary_path().context("Local multi-agent service binary is not bundled")?;
    if !service_binary.is_file() {
        bail!(
            "Local multi-agent service binary not found at {}. Build or bundle crates/local_multi_agent_service first.",
            service_binary.display()
        );
    }

    let service_env = config.env(openai_api_key.as_deref());
    prepare_service_paths(&service_env)?;
    let service_dir = service_binary.parent().map(Path::to_path_buf);

    let mut command = Command::new(&service_binary);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(service_dir) = service_dir.as_ref() {
        command.current_dir(service_dir);
    }
    for (key, value) in service_env {
        command.env(key, value);
    }
    command.env("LOCAL_CONFIG_HASH", &config_hash);

    let child = command
        .spawn()
        .with_context(|| format!("Failed to spawn {}", service_binary.display()))?;
    let pid = Some(child.id());

    wait_until_healthy(root_url.clone()).await?;

    Ok(StartServiceResult {
        child: Some(child),
        root_url,
        pid,
        config_hash,
    })
}

async fn wait_until_healthy(root_url: Url) -> Result<()> {
    let started = instant::Instant::now();
    let mut last_error = None;
    while started.elapsed() < Duration::from_secs(10) {
        match health_check(root_url.clone()).await {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        Timer::after(Duration::from_millis(150)).await;
    }
    Err(last_error.unwrap_or_else(|| anyhow!("Timed out waiting for local service health check")))
}

async fn health_check(root_url: Url) -> Result<()> {
    let url = root_url
        .join("/health")
        .context("Failed to build local health URL")?;
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .timeout(HEALTH_TIMEOUT)
        .send()
        .await
        .context("request failed")?;
    if !response.status().is_success() {
        bail!("HTTP {}", response.status())
    }
    let health = response
        .json::<LocalMultiAgentHealthResponse>()
        .await
        .context("health response was not Warp local service JSON")?;
    if health.ok
        && health
            .version
            .as_deref()
            .is_some_and(|version| !version.is_empty())
    {
        Ok(())
    } else {
        bail!("health response did not identify a Warp local service")
    }
}

async fn fetch_provider_models(base_url: &str, api_key: Option<&str>) -> Result<Vec<String>> {
    validate_absolute_http_url(base_url)?;
    let models_url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut request = client
        .get(models_url)
        .timeout(HEALTH_TIMEOUT)
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(api_key) = api_key.and_then(non_empty_str) {
        request = request.bearer_auth(api_key);
    }

    let response = request
        .send()
        .await
        .context("provider models request failed")?;
    if !response.status().is_success() {
        bail!(
            "provider models request failed with HTTP {}",
            response.status()
        );
    }

    let payload = response
        .json::<ProviderModelsResponse>()
        .await
        .context("provider models response was not OpenAI-compatible JSON")?;
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    for item in payload.data {
        let id = match item {
            serde_json::Value::String(id) => id,
            serde_json::Value::Object(model) => model
                .get("id")
                .and_then(|id| id.as_str())
                .unwrap_or_default()
                .to_string(),
            _ => String::new(),
        };
        let id = id.trim().to_string();
        if !id.is_empty() && seen.insert(id.clone()) {
            models.push(id);
        }
    }

    if models.is_empty() {
        bail!("provider models response had no usable model IDs");
    }

    Ok(models)
}

fn prepare_service_paths(env: &BTreeMap<String, String>) -> Result<()> {
    if let Some(path) = env
        .get("LOCAL_GRAPHQL_DB_PATH")
        .and_then(|path| non_empty_str(path))
    {
        create_parent_dir(path)
            .with_context(|| format!("Failed to prepare local GraphQL DB path at {path}"))?;
    }

    if let Some(path) = env
        .get("LOCAL_SERVICE_LOG_PATH")
        .and_then(|path| non_empty_str(path))
        .filter(|path| !matches!(path.to_ascii_lowercase().as_str(), "false" | "off" | "0"))
    {
        create_parent_dir(path)
            .with_context(|| format!("Failed to prepare local service log path at {path}"))?;
    }

    Ok(())
}

fn create_parent_dir(path: &str) -> Result<()> {
    if let Some(parent) = Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn ensure_port_available(config: &LocalMultiAgentConfig) -> Result<()> {
    match TcpListener::bind((config.host.trim(), config.port)) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(error) => bail!(
            "Port {} on {} is already in use and does not look like a healthy Warp local service: {}",
            config.port,
            config.host,
            error
        ),
    }
}

fn service_binary_path() -> Option<PathBuf> {
    if let Some(resources) = warp_core::paths::bundled_resources_dir() {
        let bundled = resources.join(BUNDLED_SERVICE_DIR);
        let binary = bundled.join(SERVICE_BINARY_NAME);
        if binary.is_file() {
            return Some(binary);
        }
    }

    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(current_dir) = current_exe.parent() {
            let binary = current_dir.join(SERVICE_BINARY_NAME);
            if binary.is_file() {
                return Some(binary);
            }
        }
    }

    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .to_path_buf();
    [
        workspace_root
            .join("target")
            .join("debug")
            .join(SERVICE_BINARY_NAME),
        workspace_root
            .join("target")
            .join("release-lto")
            .join(SERVICE_BINARY_NAME),
        workspace_root
            .join("target")
            .join("release")
            .join(SERVICE_BINARY_NAME),
    ]
    .into_iter()
    .find(|binary| binary.is_file())
}

fn default_graphql_db_path() -> String {
    warp_core::paths::data_dir()
        .join("local-multi-agent")
        .join("local-graphql.sqlite")
        .to_string_lossy()
        .into_owned()
}

fn default_log_path() -> String {
    let base = warp_logging::log_directory().unwrap_or_else(|_| warp_core::paths::data_dir());
    base.join("local-multi-agent.log")
        .to_string_lossy()
        .into_owned()
}

fn load_config(ctx: &AppContext) -> LocalMultiAgentConfig {
    match ctx.private_user_preferences().read_value(PREF_KEY) {
        Ok(Some(json)) => match serde_json::from_str(&json) {
            Ok(config) => config,
            Err(error) => {
                log::warn!("Failed to parse local multi-agent config: {error}");
                LocalMultiAgentConfig::default()
            }
        },
        Ok(None) => LocalMultiAgentConfig::default(),
        Err(error) => {
            log::warn!("Failed to read local multi-agent config: {error}");
            LocalMultiAgentConfig::default()
        }
    }
}

fn load_config_with_legacy_migration(ctx: &mut AppContext) -> LocalMultiAgentConfig {
    let mut config = load_config(ctx);
    if config.openai_base_url.is_none() {
        let legacy_base_url = ::ai::api_keys::ApiKeyManager::as_ref(ctx)
            .keys()
            .openai_base_url
            .clone();
        if let Some(legacy_base_url) = legacy_base_url {
            config.openai_base_url = Some(legacy_base_url);
            save_config(&config, ctx);
            ::ai::api_keys::ApiKeyManager::handle(ctx).update(ctx, |api_keys, ctx| {
                api_keys.set_openai_base_url(None, ctx);
            });
        }
    }
    config
}

fn save_config(config: &LocalMultiAgentConfig, ctx: &AppContext) {
    let Ok(json) = serde_json::to_string(config) else {
        return;
    };
    if let Err(error) = ctx.private_user_preferences().write_value(PREF_KEY, json) {
        log::warn!("Failed to persist local multi-agent config: {error}");
    }
}

fn set_global_root_url(root_url: Option<Url>) {
    *LOCAL_ROOT_URL.write() = root_url;
}

fn default_local_max_completion_tokens() -> u32 {
    DEFAULT_LOCAL_MAX_COMPLETION_TOKENS
}

fn non_empty(value: String) -> Option<String> {
    non_empty_str(&value).map(str::to_string)
}

fn non_empty_str(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn validate_absolute_http_url(value: &str) -> Result<(), LocalMultiAgentConfigError> {
    let Ok(parsed) = Url::parse(value.trim().trim_end_matches('/')) else {
        return Err(LocalMultiAgentConfigError::InvalidOpenAiBaseUrl);
    };
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(LocalMultiAgentConfigError::InvalidOpenAiBaseUrl);
    }
    Ok(())
}

fn parse_model_aliases(
    value: &str,
) -> Result<BTreeMap<String, String>, LocalMultiAgentConfigError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(BTreeMap::new());
    }
    let parsed: serde_json::Value = serde_json::from_str(value)
        .map_err(|source| LocalMultiAgentConfigError::InvalidModelAliases { source })?;
    let Some(object) = parsed.as_object() else {
        return Err(LocalMultiAgentConfigError::ModelAliasesMustBeObject);
    };
    Ok(object
        .iter()
        .filter_map(|(key, value)| {
            value
                .as_str()
                .and_then(non_empty_str)
                .map(|value| (key.clone(), value.to_string()))
        })
        .collect())
}

fn parse_model_list_ids(value: &str) -> Vec<String> {
    let value = value.trim();
    if value.is_empty() {
        return Vec::new();
    }
    if value.starts_with('[') {
        return serde_json::from_str::<serde_json::Value>(value)
            .ok()
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| match value {
                serde_json::Value::String(id) => Some(id),
                serde_json::Value::Object(model) => model
                    .get("id")
                    .and_then(|id| id.as_str())
                    .map(str::to_string),
                _ => None,
            })
            .filter_map(|id| non_empty_str(&id).map(str::to_string))
            .collect();
    }
    value
        .split(',')
        .filter_map(|id| non_empty_str(id).map(str::to_string))
        .collect()
}

fn validate_context_tokens(value: &str) -> Result<(), LocalMultiAgentConfigError> {
    let value = value.trim();
    if value.is_empty() || value.parse::<u32>().is_ok_and(|v| v > 0) {
        return Ok(());
    }
    let parsed: serde_json::Value = serde_json::from_str(value)
        .map_err(|source| LocalMultiAgentConfigError::InvalidContextTokensJson { source })?;
    if !parsed.is_object() {
        return Err(LocalMultiAgentConfigError::InvalidContextTokens);
    }
    Ok(())
}

pub fn local_no_cloud_root_url() -> Option<Url> {
    LocalMultiAgentManager::global_root_url()
        .or_else(|| Url::parse(LOCAL_NO_CLOUD_SERVER_ROOT_URL).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_schema() {
        let schema = config_schema_defaults();
        let config = LocalMultiAgentConfig::default();
        assert_eq!(config.host, schema.host);
        assert_eq!(config.port, schema.port);
        assert_eq!(
            config.openai_base_url.as_deref(),
            non_empty_str(&schema.openai_base_url)
        );
        assert_eq!(config.local_model_aliases, schema.local_model_aliases);
        assert_eq!(config.local_model_list, schema.local_model_list);
        assert_eq!(config.local_enable_tools, schema.local_enable_tools);
        assert_eq!(
            config.local_max_history_messages,
            schema.local_max_history_messages
        );
        assert_eq!(
            config.local_max_completion_tokens,
            schema.local_max_completion_tokens
        );
        assert_eq!(config.log_level, schema.log_level);
    }

    #[test]
    fn config_to_env_uses_defaults_for_paths_and_provider_settings() {
        let config = LocalMultiAgentConfig {
            openai_base_url: Some("http://127.0.0.1:11434/v1".to_string()),
            ..Default::default()
        };
        let env = config.env(Some("sk-test"));
        assert_eq!(env.get("HOST").map(String::as_str), Some("127.0.0.1"));
        assert_eq!(env.get("PORT").map(String::as_str), Some("8787"));
        assert_eq!(
            env.get("WARP_URL_SCHEME").map(String::as_str),
            Some(ChannelState::url_scheme())
        );
        assert_eq!(
            env.get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-test")
        );
        assert_eq!(
            env.get("OPENAI_BASE_URL").map(String::as_str),
            Some("http://127.0.0.1:11434/v1")
        );
        assert_eq!(
            env.get("LOCAL_MAX_COMPLETION_TOKENS").map(String::as_str),
            Some("2048")
        );
        assert!(env
            .get("LOCAL_GRAPHQL_DB_PATH")
            .is_some_and(|v| v.ends_with(".sqlite")));
        assert!(env
            .get("LOCAL_SERVICE_LOG_PATH")
            .is_some_and(|v| v.ends_with(".log")));
    }

    #[test]
    fn validation_rejects_bad_json_settings() {
        let config = LocalMultiAgentConfig {
            local_model_aliases: "[]".to_string(),
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(LocalMultiAgentConfigError::ModelAliasesMustBeObject)
        ));

        let config = LocalMultiAgentConfig {
            local_model_context_tokens: Some("not-json".to_string()),
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(LocalMultiAgentConfigError::InvalidContextTokensJson { .. })
        ));

        let config = LocalMultiAgentConfig {
            local_max_completion_tokens: 0,
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(LocalMultiAgentConfigError::InvalidMaxCompletionTokens)
        ));
    }

    #[test]
    fn persisted_config_without_max_completion_tokens_uses_default() {
        let config: LocalMultiAgentConfig = serde_json::from_str(
            r#"{
                "host": "127.0.0.1",
                "port": 8787,
                "openai_base_url": null,
                "openai_model": null,
                "local_model_aliases": "{}",
                "local_model_list": "model",
                "local_enable_tools": true,
                "local_max_history_messages": 80,
                "local_model_context_tokens": null,
                "local_graphql_db_path": null,
                "log_level": "info",
                "local_service_log_path": null,
                "local_multi_agent_system_prompt": ""
            }"#,
        )
        .unwrap();

        assert_eq!(
            config.local_max_completion_tokens,
            DEFAULT_LOCAL_MAX_COMPLETION_TOKENS
        );
    }

    #[test]
    fn validation_rejects_bad_openai_base_url() {
        let config = LocalMultiAgentConfig {
            openai_base_url: Some("localhost:11434/v1".to_string()),
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(LocalMultiAgentConfigError::InvalidOpenAiBaseUrl)
        ));
    }

    #[test]
    fn discovered_models_update_default_aliases_and_model_list() {
        let mut config = LocalMultiAgentConfig {
            openai_model: Some("missing".to_string()),
            local_model_aliases: r#"{"custom":"kept"}"#.to_string(),
            local_model_list: "old".to_string(),
            ..Default::default()
        };
        let models = vec!["model-a".to_string(), "model-b".to_string()];

        assert!(config.apply_discovered_models(&models).unwrap());
        assert_eq!(config.openai_model.as_deref(), Some("model-a"));
        assert_eq!(config.local_model_list, "model-a,model-b");

        let aliases = config.model_aliases().unwrap();
        assert_eq!(aliases.get("custom").map(String::as_str), Some("kept"));
        for alias in LOCAL_MODEL_ALIAS_IDS {
            assert_eq!(aliases.get(alias).map(String::as_str), Some("model-a"));
        }
    }

    #[test]
    fn prepare_service_paths_creates_parent_directories() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("data/local-graphql.sqlite");
        let log_path = temp_dir.path().join("logs/local-service.log");
        let mut env = BTreeMap::new();
        env.insert(
            "LOCAL_GRAPHQL_DB_PATH".to_string(),
            db_path.to_string_lossy().into_owned(),
        );
        env.insert(
            "LOCAL_SERVICE_LOG_PATH".to_string(),
            log_path.to_string_lossy().into_owned(),
        );

        prepare_service_paths(&env).unwrap();

        assert!(db_path.parent().unwrap().is_dir());
        assert!(log_path.parent().unwrap().is_dir());
    }
}
