use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;
use warp_multi_agent_api as api;
use warpui_core::{Entity, ModelContext, SingletonEntity};
use warpui_extras::secure_storage::{self, AppContextExt};

pub use crate::aws_credentials::{AwsCredentials, AwsCredentialsState};

const SECURE_STORAGE_KEY: &str = "AiApiKeys";
pub const DEFAULT_PROFILE_INFERENCE_KEY: &str = "default";

/// Emitted when user-provided API keys are updated in-memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiKeyManagerEvent {
    KeysUpdated,
}

/// User-provided API keys for AI providers.
///
/// These are used for "Bring Your Own API Key" functionality, allowing
/// users to use their own API keys instead of Warp's.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiKeys {
    /// Legacy/global fields. New callers should prefer `profile_inference_settings`.
    /// These stay serialized so older clients can still read/write the secure-storage payload.
    pub google: Option<String>,
    pub anthropic: Option<String>,
    pub openai: Option<String>,
    pub openai_base_url: Option<String>,
    pub local_multi_agent_server_root_url: Option<String>,
    pub open_router: Option<String>,
    pub custom_endpoints: Vec<CustomEndpoint>,
    pub profile_inference_settings: BTreeMap<String, ProfileInferenceSettings>,
    pub profile_settings_migrated: bool,
    pub local_profile_settings_migrated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileInferenceSettings {
    pub google: Option<String>,
    pub anthropic: Option<String>,
    pub openai: Option<String>,
    pub openai_base_url: Option<String>,
    pub local_multi_agent_server_root_url: Option<String>,
    pub open_router: Option<String>,
    pub custom_endpoints: Vec<CustomEndpoint>,
    pub local_model_aliases: String,
    pub local_model_list: String,
    pub local_ai_autocomplete_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomEndpoint {
    pub name: String,
    pub url: String,
    pub api_key: String,
    pub models: Vec<CustomEndpointModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomEndpointModel {
    pub name: String,
    pub alias: Option<String>,
    /// Stable identifier used as `ModelConfig.{base,coding,cli_agent,computer_use_agent}` and
    /// as the `CustomModelProviders.providers[*].models[*].config_key` on the request wire.
    /// Generated as a UUIDv4 at model creation.
    pub config_key: String,
}

impl CustomEndpointModel {
    /// Picker label: prefer the user-provided alias; fall back to the raw model name
    /// so a row is never blank.
    pub fn display_label(&self) -> &str {
        match self.alias.as_deref() {
            Some(alias) if !alias.trim().is_empty() => alias,
            _ => &self.name,
        }
    }
}

impl ApiKeys {
    pub fn has_any_key(&self) -> bool {
        self.default_profile_settings().has_any_key()
    }

    /// Returns `true` when the user has at least one custom endpoint configured.
    pub fn has_custom_endpoints(&self) -> bool {
        !self.default_profile_settings().custom_endpoints.is_empty()
    }

    pub fn default_profile_settings(&self) -> ProfileInferenceSettings {
        self.profile_settings(DEFAULT_PROFILE_INFERENCE_KEY)
    }

    pub fn profile_settings(&self, profile_key: &str) -> ProfileInferenceSettings {
        if let Some(settings) = self.profile_inference_settings.get(profile_key) {
            return settings.clone();
        }
        if profile_key == DEFAULT_PROFILE_INFERENCE_KEY {
            return self.legacy_default_profile_settings();
        }
        ProfileInferenceSettings::default()
    }

    fn profile_settings_mut(&mut self, profile_key: &str) -> &mut ProfileInferenceSettings {
        if profile_key == DEFAULT_PROFILE_INFERENCE_KEY
            && !self.profile_inference_settings.contains_key(profile_key)
        {
            let legacy_settings = self.legacy_default_profile_settings();
            self.profile_inference_settings
                .insert(profile_key.to_string(), legacy_settings);
        }
        self.profile_inference_settings
            .entry(profile_key.to_string())
            .or_default()
    }

    fn legacy_default_profile_settings(&self) -> ProfileInferenceSettings {
        ProfileInferenceSettings {
            google: self.google.clone(),
            anthropic: self.anthropic.clone(),
            openai: self.openai.clone(),
            openai_base_url: self.openai_base_url.clone(),
            local_multi_agent_server_root_url: self.local_multi_agent_server_root_url.clone(),
            open_router: self.open_router.clone(),
            custom_endpoints: self.custom_endpoints.clone(),
            local_model_aliases: String::new(),
            local_model_list: String::new(),
            local_ai_autocomplete_enabled: false,
        }
    }

    fn migrate_default_profile_if_needed(&mut self) -> bool {
        if self.profile_settings_migrated {
            return false;
        }

        let mut default_profile = self
            .profile_inference_settings
            .remove(DEFAULT_PROFILE_INFERENCE_KEY)
            .unwrap_or_default();
        default_profile.google = default_profile.google.or_else(|| self.google.clone());
        default_profile.anthropic = default_profile.anthropic.or_else(|| self.anthropic.clone());
        default_profile.openai = default_profile.openai.or_else(|| self.openai.clone());
        default_profile.openai_base_url = default_profile
            .openai_base_url
            .or_else(|| self.openai_base_url.clone());
        default_profile.local_multi_agent_server_root_url = default_profile
            .local_multi_agent_server_root_url
            .or_else(|| self.local_multi_agent_server_root_url.clone());
        default_profile.open_router = default_profile
            .open_router
            .or_else(|| self.open_router.clone());
        if default_profile.custom_endpoints.is_empty() {
            default_profile.custom_endpoints = self.custom_endpoints.clone();
        }

        self.profile_inference_settings
            .insert(DEFAULT_PROFILE_INFERENCE_KEY.to_string(), default_profile);
        self.profile_settings_migrated = true;
        true
    }

    pub fn migrate_default_profile_local_settings_if_needed(
        &mut self,
        openai_base_url: Option<String>,
        local_model_aliases: String,
        local_model_list: String,
        local_ai_autocomplete_enabled: bool,
    ) -> bool {
        if self.local_profile_settings_migrated {
            return false;
        }

        let default_profile = self.profile_settings_mut(DEFAULT_PROFILE_INFERENCE_KEY);
        if default_profile.openai_base_url.is_none() {
            default_profile.openai_base_url = normalize_absolute_http_url(openai_base_url);
        }
        if default_profile.local_model_aliases.trim().is_empty()
            && !local_model_aliases.trim().is_empty()
        {
            default_profile.local_model_aliases = local_model_aliases;
        }
        if default_profile.local_model_list.trim().is_empty() && !local_model_list.trim().is_empty()
        {
            default_profile.local_model_list = local_model_list;
        }
        default_profile.local_ai_autocomplete_enabled = local_ai_autocomplete_enabled;
        self.local_profile_settings_migrated = true;
        true
    }

    pub fn clear_profile_settings(&mut self, profile_key: &str) {
        if profile_key == DEFAULT_PROFILE_INFERENCE_KEY {
            return;
        }
        self.profile_inference_settings.remove(profile_key);
    }
}

impl ProfileInferenceSettings {
    pub fn has_any_key(&self) -> bool {
        self.openai.is_some()
            || self.anthropic.is_some()
            || self.google.is_some()
            || self.open_router.is_some()
            || self
                .custom_endpoints
                .iter()
                .any(|endpoint| !endpoint.api_key.trim().is_empty())
    }
}

fn normalize_absolute_http_url(url: Option<String>) -> Option<String> {
    let url = url?;
    let url = url.trim().trim_end_matches('/').to_string();
    if url.is_empty() {
        return None;
    }

    let Ok(parsed) = Url::parse(&url) else {
        return None;
    };
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return None;
    }

    Some(url)
}

/// Controls how AWS credentials are refreshed by [`ApiKeyManager`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AwsCredentialsRefreshStrategy {
    /// Load credentials from the local AWS credential chain (~/.aws). This is the default.
    #[default]
    LocalChain,
    /// Credentials are managed externally via OIDC/STS.
    /// The task ID is used to scope the STS AssumeRoleWithWebIdentity session.
    /// The role ARN + region are the info used to assume the IAM role via STS.
    OidcManaged {
        task_id: Option<String>,
        role_arn: String,
        region: String,
    },
}

/// A structure that manages API keys for AI providers.
pub struct ApiKeyManager {
    keys: ApiKeys,
    pub(crate) aws_credentials_state: AwsCredentialsState,
    aws_credentials_refresh_strategy: AwsCredentialsRefreshStrategy,
    secure_storage_write_version: u64,
}

impl ApiKeyManager {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let is_local_only = ChannelState::is_local_only();
        let keys = if is_local_only {
            // Local-only mode starts with empty keys for BYOK (Bring Your Own Key).
            // Users configure their own API keys via the UI as needed.
            ApiKeys::default()
        } else {
            Self::load_keys_from_secure_storage(ctx)
        };
        let grok_tokens = if is_local_only {
            None
        } else {
            Self::load_grok_tokens_from_secure_storage(ctx)
        };
        let mut manager = Self {
            keys,
            grok_tokens,
            aws_credentials_state: AwsCredentialsState::Missing,
            aws_credentials_refresh_strategy: AwsCredentialsRefreshStrategy::default(),
            geap_credentials_state: GeapCredentialsState::Missing,
            #[cfg(not(target_family = "wasm"))]
            grok_refresh_allowed: false,
            #[cfg(not(target_family = "wasm"))]
            grok_refresh_in_flight: false,
            secure_storage_write_version: 0,
        };
        if manager.keys.migrate_default_profile_if_needed() {
            manager.write_keys_to_secure_storage(ctx);
        }
        manager
    }

    pub fn keys(&self) -> &ApiKeys {
        &self.keys
    }

    pub fn set_google_key(&mut self, key: Option<String>, ctx: &mut ModelContext<Self>) {
        self.set_google_key_for_profile(DEFAULT_PROFILE_INFERENCE_KEY, key, ctx);
    }

    pub fn set_google_key_for_profile(
        &mut self,
        profile_key: &str,
        key: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.keys.profile_settings_mut(profile_key).google = key.clone();
        if profile_key == DEFAULT_PROFILE_INFERENCE_KEY {
            self.keys.google = key;
        }
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn set_anthropic_key(&mut self, key: Option<String>, ctx: &mut ModelContext<Self>) {
        self.set_anthropic_key_for_profile(DEFAULT_PROFILE_INFERENCE_KEY, key, ctx);
    }

    pub fn set_anthropic_key_for_profile(
        &mut self,
        profile_key: &str,
        key: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.keys.profile_settings_mut(profile_key).anthropic = key.clone();
        if profile_key == DEFAULT_PROFILE_INFERENCE_KEY {
            self.keys.anthropic = key;
        }
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn set_openai_key(&mut self, key: Option<String>, ctx: &mut ModelContext<Self>) {
        self.set_openai_key_for_profile(DEFAULT_PROFILE_INFERENCE_KEY, key, ctx);
    }

    pub fn set_openai_key_for_profile(
        &mut self,
        profile_key: &str,
        key: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.keys.profile_settings_mut(profile_key).openai = key.clone();
        if profile_key == DEFAULT_PROFILE_INFERENCE_KEY {
            self.keys.openai = key;
        }
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn set_openai_base_url(&mut self, base_url: Option<String>, ctx: &mut ModelContext<Self>) {
        self.set_openai_base_url_for_profile(DEFAULT_PROFILE_INFERENCE_KEY, base_url, ctx);
    }

    pub fn set_openai_base_url_for_profile(
        &mut self,
        profile_key: &str,
        base_url: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        let base_url = normalize_absolute_http_url(base_url);
        self.keys.profile_settings_mut(profile_key).openai_base_url = base_url.clone();
        if profile_key == DEFAULT_PROFILE_INFERENCE_KEY {
            self.keys.openai_base_url = base_url;
        }
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn set_local_multi_agent_server_root_url(
        &mut self,
        url: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.set_local_multi_agent_server_root_url_for_profile(
            DEFAULT_PROFILE_INFERENCE_KEY,
            url,
            ctx,
        );
    }

    pub fn set_local_multi_agent_server_root_url_for_profile(
        &mut self,
        profile_key: &str,
        url: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        let url = normalize_absolute_http_url(url);
        self.keys
            .profile_settings_mut(profile_key)
            .local_multi_agent_server_root_url = url.clone();
        if profile_key == DEFAULT_PROFILE_INFERENCE_KEY {
            self.keys.local_multi_agent_server_root_url = url;
        }
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn set_open_router_key(&mut self, key: Option<String>, ctx: &mut ModelContext<Self>) {
        self.set_open_router_key_for_profile(DEFAULT_PROFILE_INFERENCE_KEY, key, ctx);
    }

    pub fn set_open_router_key_for_profile(
        &mut self,
        profile_key: &str,
        key: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.keys.profile_settings_mut(profile_key).open_router = key.clone();
        if profile_key == DEFAULT_PROFILE_INFERENCE_KEY {
            self.keys.open_router = key;
        }
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn set_local_model_aliases_for_profile(
        &mut self,
        profile_key: &str,
        aliases: String,
        ctx: &mut ModelContext<Self>,
    ) {
        self.keys
            .profile_settings_mut(profile_key)
            .local_model_aliases = aliases;
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn set_local_model_settings_for_profile(
        &mut self,
        profile_key: &str,
        aliases: String,
        model_list: String,
        ctx: &mut ModelContext<Self>,
    ) {
        let settings = self.keys.profile_settings_mut(profile_key);
        settings.local_model_aliases = aliases;
        settings.local_model_list = model_list;
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn set_local_ai_autocomplete_enabled_for_profile(
        &mut self,
        profile_key: &str,
        enabled: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        self.keys
            .profile_settings_mut(profile_key)
            .local_ai_autocomplete_enabled = enabled;
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn set_profile_inference_defaults_if_missing(
        &mut self,
        profile_key: &str,
        defaults: ProfileInferenceSettings,
        ctx: &mut ModelContext<Self>,
    ) {
        if self
            .keys
            .profile_inference_settings
            .contains_key(profile_key)
        {
            return;
        }
        self.keys
            .profile_inference_settings
            .insert(profile_key.to_string(), defaults);
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn remove_profile_settings(&mut self, profile_key: &str, ctx: &mut ModelContext<Self>) {
        if profile_key == DEFAULT_PROFILE_INFERENCE_KEY {
            return;
        }
        if self
            .keys
            .profile_inference_settings
            .remove(profile_key)
            .is_some()
        {
            ctx.emit(ApiKeyManagerEvent::KeysUpdated);
            self.write_keys_to_secure_storage(ctx);
        }
    }

    pub fn rename_profile_settings(
        &mut self,
        old_profile_key: &str,
        new_profile_key: &str,
        ctx: &mut ModelContext<Self>,
    ) {
        if old_profile_key == new_profile_key || old_profile_key == DEFAULT_PROFILE_INFERENCE_KEY {
            return;
        }
        let Some(settings) = self.keys.profile_inference_settings.remove(old_profile_key) else {
            return;
        };
        self.keys
            .profile_inference_settings
            .entry(new_profile_key.to_string())
            .or_insert(settings);
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn migrate_default_profile_local_settings_if_needed(
        &mut self,
        openai_base_url: Option<String>,
        local_model_aliases: String,
        local_model_list: String,
        local_ai_autocomplete_enabled: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.keys.migrate_default_profile_local_settings_if_needed(
            openai_base_url,
            local_model_aliases,
            local_model_list,
            local_ai_autocomplete_enabled,
        ) {
            ctx.emit(ApiKeyManagerEvent::KeysUpdated);
            self.write_keys_to_secure_storage(ctx);
        }
    }

    #[allow(dead_code)]
    fn set_legacy_google_key(&mut self, key: Option<String>, ctx: &mut ModelContext<Self>) {
        self.keys.google = key;
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    #[allow(dead_code)]
    fn set_legacy_anthropic_key(&mut self, key: Option<String>, ctx: &mut ModelContext<Self>) {
        self.keys.anthropic = key;
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    #[allow(dead_code)]
    fn set_legacy_openai_key(&mut self, key: Option<String>, ctx: &mut ModelContext<Self>) {
        self.keys.openai = key;
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    #[allow(dead_code)]
    fn set_legacy_openai_base_url(
        &mut self,
        base_url: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.keys.openai_base_url = normalize_absolute_http_url(base_url);
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    #[allow(dead_code)]
    fn set_legacy_local_multi_agent_server_root_url(
        &mut self,
        url: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.keys.local_multi_agent_server_root_url = normalize_absolute_http_url(url);
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    #[allow(dead_code)]
    fn set_legacy_open_router_key(&mut self, key: Option<String>, ctx: &mut ModelContext<Self>) {
        self.keys.open_router = key;
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn add_custom_endpoint(
        &mut self,
        name: String,
        url: String,
        api_key: String,
        models: Vec<(String, Option<String>, Option<String>)>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.add_custom_endpoint_for_profile(
            DEFAULT_PROFILE_INFERENCE_KEY,
            name,
            url,
            api_key,
            models,
            ctx,
        );
    }

    pub fn add_custom_endpoint_for_profile(
        &mut self,
        profile_key: &str,
        name: String,
        url: String,
        api_key: String,
        models: Vec<(String, Option<String>, Option<String>)>,
        ctx: &mut ModelContext<Self>,
    ) {
        let endpoint = CustomEndpoint {
            name,
            url,
            api_key,
            models: models
                .into_iter()
                .map(|(name, alias, config_key)| CustomEndpointModel {
                    name,
                    alias,
                    config_key: config_key
                        .filter(|k| !k.is_empty())
                        .unwrap_or_else(|| Uuid::new_v4().to_string()),
                })
                .collect(),
        };
        self.keys
            .profile_settings_mut(profile_key)
            .custom_endpoints
            .push(endpoint.clone());
        if profile_key == DEFAULT_PROFILE_INFERENCE_KEY {
            self.keys.custom_endpoints.push(endpoint);
        }
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn save_custom_endpoint(
        &mut self,
        index: usize,
        name: String,
        url: String,
        api_key: String,
        models: Vec<(String, Option<String>, Option<String>)>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.save_custom_endpoint_for_profile(
            DEFAULT_PROFILE_INFERENCE_KEY,
            index,
            name,
            url,
            api_key,
            models,
            ctx,
        );
    }

    pub fn save_custom_endpoint_for_profile(
        &mut self,
        profile_key: &str,
        index: usize,
        name: String,
        url: String,
        api_key: String,
        models: Vec<(String, Option<String>, Option<String>)>,
        ctx: &mut ModelContext<Self>,
    ) {
        if index
            >= self
                .keys
                .profile_settings(profile_key)
                .custom_endpoints
                .len()
        {
            return;
        }
        let endpoint = CustomEndpoint {
            name,
            url,
            api_key,
            models: models
                .into_iter()
                .map(|(name, alias, config_key)| CustomEndpointModel {
                    name,
                    alias,
                    config_key: config_key
                        .filter(|k| !k.is_empty())
                        .unwrap_or_else(|| Uuid::new_v4().to_string()),
                })
                .collect(),
        };
        self.keys.profile_settings_mut(profile_key).custom_endpoints[index] = endpoint.clone();
        if profile_key == DEFAULT_PROFILE_INFERENCE_KEY {
            self.keys.custom_endpoints[index] = endpoint;
        }
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn remove_custom_endpoint(&mut self, index: usize, ctx: &mut ModelContext<Self>) {
        self.remove_custom_endpoint_for_profile(DEFAULT_PROFILE_INFERENCE_KEY, index, ctx);
    }

    pub fn remove_custom_endpoint_for_profile(
        &mut self,
        profile_key: &str,
        index: usize,
        ctx: &mut ModelContext<Self>,
    ) {
        if index
            >= self
                .keys
                .profile_settings(profile_key)
                .custom_endpoints
                .len()
        {
            return;
        }
        self.keys
            .profile_settings_mut(profile_key)
            .custom_endpoints
            .remove(index);
        if profile_key == DEFAULT_PROFILE_INFERENCE_KEY {
            self.keys.custom_endpoints.remove(index);
        }
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn clear_custom_endpoints(&mut self, ctx: &mut ModelContext<Self>) {
        self.clear_custom_endpoints_for_profile(DEFAULT_PROFILE_INFERENCE_KEY, ctx);
    }

    pub fn clear_custom_endpoints_for_profile(
        &mut self,
        profile_key: &str,
        ctx: &mut ModelContext<Self>,
    ) {
        if self
            .keys
            .profile_settings(profile_key)
            .custom_endpoints
            .is_empty()
        {
            return;
        }
        self.keys
            .profile_settings_mut(profile_key)
            .custom_endpoints
            .clear();
        if profile_key == DEFAULT_PROFILE_INFERENCE_KEY {
            self.keys.custom_endpoints.clear();
        }
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
        self.write_keys_to_secure_storage(ctx);
    }

    pub fn set_aws_credentials_state(
        &mut self,
        state: AwsCredentialsState,
        ctx: &mut ModelContext<Self>,
    ) {
        self.aws_credentials_state = state;
        ctx.emit(ApiKeyManagerEvent::KeysUpdated);
    }

    pub fn aws_credentials_state(&self) -> &AwsCredentialsState {
        &self.aws_credentials_state
    }

    pub fn aws_credentials_refresh_strategy(&self) -> AwsCredentialsRefreshStrategy {
        self.aws_credentials_refresh_strategy.clone()
    }

    pub fn set_aws_credentials_refresh_strategy(
        &mut self,
        strategy: AwsCredentialsRefreshStrategy,
    ) {
        self.aws_credentials_refresh_strategy = strategy;
    }

    /// Builds the `CustomModelProviders` registry that ships with every agent request.
    ///
    /// Emits one [`CustomModelProvider`] per configured [`CustomEndpoint`], each populated with
    /// all of its [`CustomEndpointModel`]s. The per-model `config_key` is what the server uses
    /// to map a `ModelConfig.{base,coding,cli_agent,computer_use_agent}` selection back to a
    /// user-provided endpoint, so it MUST be the same UUID we store locally.
    ///
    /// Returns `None` when custom models should not be included or no endpoint has both a
    /// non-empty URL and API key.
    pub fn custom_model_providers_for_request(
        &self,
        profile_key: &str,
        include_custom_models: bool,
    ) -> Option<api::request::settings::CustomModelProviders> {
        if !include_custom_models {
            return None;
        }

        let profile_settings = self.keys.profile_settings(profile_key);
        let providers: Vec<_> = profile_settings
            .custom_endpoints
            .iter()
            .filter(|endpoint| !endpoint.url.trim().is_empty() && !endpoint.api_key.is_empty())
            .map(
                |endpoint| api::request::settings::custom_model_providers::CustomModelProvider {
                    base_url: endpoint.url.clone(),
                    api_key: endpoint.api_key.clone(),
                    models: endpoint
                        .models
                        .iter()
                        .filter(|m| !m.name.trim().is_empty() && !m.config_key.is_empty())
                        .map(
                            |m| api::request::settings::custom_model_providers::CustomModel {
                                slug: m.name.clone(),
                                config_key: m.config_key.clone(),
                            },
                        )
                        .collect(),
                },
            )
            .filter(|provider| !provider.models.is_empty())
            .collect();

        if providers.is_empty() {
            None
        } else {
            Some(api::request::settings::CustomModelProviders { providers })
        }
    }

    pub fn api_keys_for_request(
        &self,
        profile_key: &str,
        include_byo_keys: bool,
        include_aws_bedrock_credentials: bool,
    ) -> Option<api::request::settings::ApiKeys> {
        let profile_settings = self.keys.profile_settings(profile_key);
        let anthropic = include_byo_keys
            .then(|| profile_settings.anthropic.clone())
            .flatten()
            .unwrap_or_default();
        let openai = include_byo_keys
            .then(|| profile_settings.openai.clone())
            .flatten()
            .unwrap_or_default();
        let google = include_byo_keys
            .then(|| profile_settings.google.clone())
            .flatten()
            .unwrap_or_default();
        let open_router = include_byo_keys
            .then(|| profile_settings.open_router.clone())
            .flatten()
            .unwrap_or_default();

        // Also include credentials when running with OIDC-managed Bedrock inference, regardless
        // of the per-user setting flag (which only applies to the local credential chain path).
        let include_aws = include_aws_bedrock_credentials
            || matches!(
                self.aws_credentials_refresh_strategy,
                AwsCredentialsRefreshStrategy::OidcManaged { .. }
            );
        let aws_credentials = include_aws
            .then(|| match self.aws_credentials_state {
                AwsCredentialsState::Loaded {
                    ref credentials, ..
                } => Some(credentials.clone().into()),
                _ => None,
            })
            .flatten();

        if anthropic.is_empty()
            && openai.is_empty()
            && google.is_empty()
            && open_router.is_empty()
            && aws_credentials.is_none()
        {
            None
        } else {
            Some(api::request::settings::ApiKeys {
                anthropic,
                openai,
                google,
                open_router,
                allow_use_of_warp_credits: false,
                aws_credentials,
            })
        }
    }

    pub fn openai_key_for_profile(&self, profile_key: &str) -> Option<String> {
        self.keys.profile_settings(profile_key).openai
    }

    pub fn openai_base_url_for_profile(&self, profile_key: &str) -> Option<String> {
        self.keys.profile_settings(profile_key).openai_base_url
    }

    pub fn local_multi_agent_server_root_url_for_profile(
        &self,
        profile_key: &str,
    ) -> Option<String> {
        self.keys
            .profile_settings(profile_key)
            .local_multi_agent_server_root_url
    }

    pub fn local_model_aliases_for_profile(&self, profile_key: &str) -> String {
        self.keys.profile_settings(profile_key).local_model_aliases
    }

    pub fn local_model_list_for_profile(&self, profile_key: &str) -> String {
        self.keys.profile_settings(profile_key).local_model_list
    }

    pub fn local_ai_autocomplete_enabled_for_profile(&self, profile_key: &str) -> bool {
        self.keys
            .profile_settings(profile_key)
            .local_ai_autocomplete_enabled
    }

    fn load_keys_from_secure_storage(ctx: &mut ModelContext<Self>) -> ApiKeys {
        let key_json = match ctx.secure_storage().read_value(SECURE_STORAGE_KEY) {
            Ok(json) => json,
            Err(e) => {
                if !matches!(e, secure_storage::Error::NotFound) {
                    log::error!("Failed to read API keys from secure storage: {e:#}");
                }
                return ApiKeys::default();
            }
        };

        match serde_json::from_str(&key_json) {
            Ok(keys) => keys,
            Err(e) => {
                log::error!("Failed to deserialize API keys: {e:#}");
                ApiKeys::default()
            }
        }
    }

    fn write_keys_to_secure_storage(&mut self, ctx: &mut ModelContext<Self>) {
        let json = match serde_json::to_string(&self.keys) {
            Ok(json) => json,
            Err(e) => {
                log::error!("Failed to serialize API keys: {e:#}");
                return;
            }
        };
        self.secure_storage_write_version += 1;
        let write_version = self.secure_storage_write_version;

        // Defer the keychain write so it doesn't block the current event
        // processing. The in-memory state is already updated and events
        // already emitted, so the UI updates immediately while the
        // potentially slow platform secure-storage call runs in a
        // subsequent main-thread callback. Skip stale callbacks so older
        // writes cannot complete after and overwrite a newer payload.
        ctx.spawn(async move { json }, move |me, json, ctx| {
            if write_version != me.secure_storage_write_version {
                return;
            }
            if let Err(e) = ctx.secure_storage().write_value(SECURE_STORAGE_KEY, &json) {
                log::error!("Failed to write API keys to secure storage: {e:#}");
            }
        });
    }
}

impl Entity for ApiKeyManager {
    type Event = ApiKeyManagerEvent;
}

#[cfg(test)]
mod url_normalization_tests {
    use super::normalize_absolute_http_url;

    #[test]
    fn normalizes_absolute_http_urls() {
        assert_eq!(
            normalize_absolute_http_url(Some(" http://127.0.0.1:8787/// ".to_string())),
            Some("http://127.0.0.1:8787".to_string()),
        );
        assert_eq!(
            normalize_absolute_http_url(Some("https://llm.example.test/v1/".to_string())),
            Some("https://llm.example.test/v1".to_string()),
        );
    }

    #[test]
    fn rejects_empty_relative_and_non_http_urls() {
        assert_eq!(normalize_absolute_http_url(Some("   ".to_string())), None);
        assert_eq!(
            normalize_absolute_http_url(Some("localhost:8787".to_string())),
            None,
        );
        assert_eq!(
            normalize_absolute_http_url(Some("/v1/chat/completions".to_string())),
            None,
        );
        assert_eq!(
            normalize_absolute_http_url(Some("ftp://example.test".to_string())),
            None,
        );
    }
}

impl SingletonEntity for ApiKeyManager {}

#[cfg(test)]
#[path = "api_keys_tests.rs"]
mod tests;
