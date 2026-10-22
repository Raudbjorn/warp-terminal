//! oh-my-warp: configurable, selectable server/agent backends.
//!
//! Warp's agent traffic (and the rest of the client) talks to the host returned
//! by `ChannelState::server_root_url()` / the websocket URL. This module lets you
//! define *alternative* backends in `agent_backends.toml` and pick which one is
//! the active "default backend" — without replacing Warp's built-in backend,
//! which is always available and is used when nothing else is selected.
//!
//! The selection is applied at startup ([`apply_selected_backend`]) by overriding
//! `ChannelState`'s server + websocket URLs, so switching takes effect on the
//! next launch. The Settings → Features "Default backend" dropdown writes the
//! selection via [`set_selected`].
//!
//! ## `agent_backends.toml` (in Warp's config dir, see [`config_path`])
//! ```toml
//! selected = "my-backend"   # omit or "warp" = built-in Warp backend
//!
//! [[backend]]
//! id = "my-backend"
//! name = "My Backend"
//! server_url = "https://warp.my-company.dev"
//! ws_url = "wss://rtc.warp.my-company.dev/graphql/v2"
//! ```

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use warp_core::channel::ChannelState;

/// Reserved id for the built-in Warp backend (no override applied).
pub const DEFAULT_BACKEND_ID: &str = "warp";
/// Label shown for the built-in Warp backend in the selector.
pub const DEFAULT_BACKEND_LABEL: &str = "Warp (Default)";

const FILE_NAME: &str = "agent_backends.toml";

/// A user-defined alternative backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Backend {
    /// Stable id used for selection.
    pub id: String,
    /// Human-friendly name shown in the dropdown (falls back to `id`).
    #[serde(default)]
    pub name: String,
    /// Server root URL, e.g. `https://warp.my-company.dev`. Empty = don't override
    /// the server URL — e.g. a gRPC-only agent backend that keeps Warp's default
    /// server for everything except the bridged agent ops.
    #[serde(default)]
    pub server_url: String,
    /// Websocket / RTC URL, e.g. `wss://rtc.warp.my-company.dev/graphql/v2`.
    /// Optional — if empty, only the server URL is overridden.
    #[serde(default)]
    pub ws_url: String,
    /// gRPC harness-host endpoint, e.g. `http://127.0.0.1:50061`. When set, the
    /// in-process agent bridge ([`crate::server::server_api::ai::bridge`]) routes
    /// agent operations to this host. Optional; empty = a plain URL-override backend.
    #[serde(default)]
    pub grpc_endpoint: String,
    /// Harness to spawn on the gRPC host (e.g. `claude`, `pi-mono`, `demo`). This
    /// is the *selected* harness, set by the Settings → Features dropdown.
    #[serde(default)]
    pub grpc_harness: String,
    /// Options shown in the Settings "Agent harness (gRPC)" dropdown. Should match
    /// the harnesses your host serves (its `harnesses.toml`).
    #[serde(default)]
    pub grpc_harnesses: Vec<String>,
    /// Bearer token sent to the gRPC host's auth interceptor (optional).
    #[serde(default)]
    pub grpc_token: String,
}

impl Backend {
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

/// On-disk config: the selected backend id plus the user's alternative backends.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentBackendsConfig {
    /// Selected backend id. `None` or [`DEFAULT_BACKEND_ID`] = built-in Warp.
    pub selected: Option<String>,
    /// User-defined alternative backends (TOML `[[backend]]` tables).
    #[serde(rename = "backend")]
    pub backends: Vec<Backend>,
}

impl AgentBackendsConfig {
    /// The currently-selected id, defaulting to the built-in Warp backend.
    pub fn selected_id(&self) -> &str {
        self.selected.as_deref().unwrap_or(DEFAULT_BACKEND_ID)
    }

    /// The selected alternative backend, or `None` for the built-in Warp backend
    /// (or if the selected id no longer exists).
    pub fn selected_backend(&self) -> Option<&Backend> {
        let id = self.selected_id();
        if id == DEFAULT_BACKEND_ID {
            None
        } else {
            self.backends.iter().find(|b| b.id == id)
        }
    }
}

/// Path to `agent_backends.toml` in Warp's config dir.
pub fn config_path() -> PathBuf {
    warp_core::paths::config_local_dir().join(FILE_NAME)
}

/// Loads the config, returning defaults if the file is missing or unparseable.
pub fn load() -> AgentBackendsConfig {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_else(|e| {
            log::error!("oh-my-warp: failed to parse {}: {e:#}", path.display());
            AgentBackendsConfig::default()
        }),
        // Missing file is the normal "use the built-in Warp backend" case.
        Err(_) => AgentBackendsConfig::default(),
    }
}

/// Writes the config to disk (creating the config dir if needed).
pub fn save(config: &AgentBackendsConfig) -> std::io::Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match toml::to_string_pretty(config) {
        Ok(contents) => std::fs::write(path, contents),
        Err(e) => {
            log::error!("oh-my-warp: failed to serialize agent_backends: {e:#}");
            Ok(())
        }
    }
}

/// Persists a new selection (called by the Settings dropdown). Passing
/// [`DEFAULT_BACKEND_ID`] selects the built-in Warp backend.
pub fn set_selected(id: &str) {
    let mut config = load();
    config.selected = if id == DEFAULT_BACKEND_ID {
        None
    } else {
        Some(id.to_string())
    };
    if let Err(e) = save(&config) {
        log::error!(
            "oh-my-warp: failed to write {}: {e:#}",
            config_path().display()
        );
    }
}

/// gRPC routing target for the in-process agent bridge.
#[derive(Debug, Clone)]
pub struct GrpcTarget {
    pub endpoint: String,
    pub harness: String,
    pub token: String,
}

/// The gRPC target of the selected backend, if it defines a `grpc_endpoint`.
/// Used by the in-process agent bridge to decide whether to route agent ops to a
/// custom harness host. `None` for the built-in Warp backend or any backend
/// without a `grpc_endpoint`.
pub fn selected_grpc_target() -> Option<GrpcTarget> {
    let config = load();
    let backend = config.selected_backend()?;
    if backend.grpc_endpoint.is_empty() {
        return None;
    }
    Some(GrpcTarget {
        endpoint: backend.grpc_endpoint.clone(),
        harness: backend.grpc_harness.clone(),
        token: backend.grpc_token.clone(),
    })
}

/// Persists the selected gRPC harness on the currently-selected backend (called by
/// the Settings → Features "Agent harness (gRPC)" dropdown). No-op for the built-in
/// Warp backend. Takes effect on the next launch.
pub fn set_grpc_harness(harness: &str) {
    let mut config = load();
    let selected_id = config.selected_id().to_string();
    if let Some(backend) = config.backends.iter_mut().find(|b| b.id == selected_id) {
        backend.grpc_harness = harness.to_string();
        if let Err(e) = save(&config) {
            log::error!(
                "oh-my-warp: failed to write {}: {e:#}",
                config_path().display()
            );
        }
    }
}

/// Applies the selected backend's URLs by overriding `ChannelState`. No-op for
/// the built-in Warp backend. Call this at startup BEFORE the server client,
/// GraphQL, or auth read the URLs.
pub fn apply_selected_backend() {
    let config = load();
    let Some(backend) = config.selected_backend() else {
        return;
    };
    if !backend.server_url.is_empty() {
        if let Err(e) = ChannelState::override_server_root_url(backend.server_url.clone()) {
            log::error!(
                "oh-my-warp: invalid server_url for backend '{}': {e:#}",
                backend.id
            );
            return;
        }
    }
    if !backend.ws_url.is_empty() {
        if let Err(e) = ChannelState::override_ws_server_url(backend.ws_url.clone()) {
            log::error!(
                "oh-my-warp: invalid ws_url for backend '{}': {e:#}",
                backend.id
            );
        }
    }
    log::info!(
        "oh-my-warp: using backend '{}' ({})",
        backend.id,
        backend.server_url
    );
}
