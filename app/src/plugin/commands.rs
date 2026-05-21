//! oh-my-warp: runtime registry of command-palette commands registered by JS plugins.
//!
//! A plugin calls `warp.commands.register(id, title, cb)` in the host process; the host registers
//! the callback in its [`warp_js::JsFunctionRegistry`] and sends a `RegisterCommandService` request
//! to the app process, which records the command here. The command palette's
//! [`crate::search::command_palette::plugin_command_data_source`] reads this registry live, and the
//! palette's accept handler invokes the callback via `CallJsFunctionService`.
//!
//! See PLUGIN_SPEC.md (Milestone 1).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use warp_js::JsFunctionId;

/// A command registered by a plugin via `warp.commands.register`.
#[derive(Clone, Debug)]
pub struct PluginCommand {
    /// Stable id, unique across plugins (e.g. `"greet.hello"`). Used as the palette binding name.
    pub id: String,
    /// Human-readable title shown in the command palette.
    pub title: String,
    /// Id of the registered JS callback in the plugin host, invoked when the command runs.
    pub function_id: JsFunctionId,
}

static REGISTRY: LazyLock<Mutex<HashMap<String, PluginCommand>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Registers (or replaces) a plugin command. Called on the app process when a plugin invokes
/// `warp.commands.register`.
pub fn register(command: PluginCommand) {
    log::info!(
        "Registered plugin command {:?} ({:?})",
        command.id,
        command.title
    );
    REGISTRY.lock().unwrap().insert(command.id.clone(), command);
}

/// Returns a snapshot of all registered plugin commands.
pub fn all() -> Vec<PluginCommand> {
    REGISTRY.lock().unwrap().values().cloned().collect()
}

/// Returns the JS callback id for the command with the given id, if registered.
pub fn function_id(id: &str) -> Option<JsFunctionId> {
    REGISTRY.lock().unwrap().get(id).map(|c| c.function_id)
}
