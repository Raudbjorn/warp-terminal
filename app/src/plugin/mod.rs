pub(crate) mod ai_tools;
pub(crate) mod app;
pub(crate) mod app_requests;
pub(crate) mod commands;
pub(crate) mod events;
#[cfg(feature = "plugin_host")]
pub(crate) mod installed;
pub(crate) mod service;

#[cfg_attr(not(target_family = "wasm"), path = "host/native/mod.rs")]
#[cfg_attr(target_family = "wasm", path = "host/wasm/mod.rs")]
mod host;

pub(crate) use app::{PluginHost, PluginHostEvent};
pub use host::run as run_plugin_host;

/// Flag to be passed to the warp executable when executing the warp binary as the plugin host
/// process rather than the main app.
///
/// Must match the clap `long_flag` on `warp_cli::WorkerCommand::PluginHost` (`"plugin-host"`).
/// Upstream defines this constant with an underscore (`--plugin_host`), which clap rejects, so the
/// spawned host exits immediately on arg parsing; use the hyphenated form so the host launches.
pub const PLUGIN_HOST_FLAG: &str = "--plugin-host";

/// The name of the environment variable used to pass connection address for the app server to the
/// plugin host process.
const PLUGIN_HOST_ADDRESS_ENV_VAR: &str = "WARP_PLUGIN_HOST_ADDRESS";
