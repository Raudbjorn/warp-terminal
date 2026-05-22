//! oh-my-warp: runtime registry of AI agent tools registered by JS plugins via `warp.ai.registerTool`.
//!
//! A plugin (granted the `ai` permission) calls `warp.ai.registerTool({ name, description, schema,
//! run })` in the host process. The host registers the `run` callback in its
//! [`warp_js::JsFunctionRegistry`] and sends a `RegisterToolService` request to the app, which
//! records the tool here. The agent request builder ([`crate::ai::agent::api`]) injects these tools
//! into the model's MCP context, and the tool-call executor
//! ([`crate::ai::blocklist::action_model::execute::call_mcp_tool`]) dispatches a call back to the
//! plugin's `run` callback via `CallJsFunctionService`.
//!
//! Tool I/O is JSON-string based (the proven `warp_js` `String` path): `run(argsJson)` receives the
//! model's arguments as a JSON string and returns a string result. See PLUGIN_SPEC.md (M4).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use warp_js::{FromWarpJs, JsFunctionId, JsFunctionRegistry};

/// A tool registered by a plugin via `warp.ai.registerTool`.
#[derive(Clone, Debug)]
pub struct PluginTool {
    /// Tool name the model calls (should be unique; authors namespace it, e.g. `"greet_lookup"`).
    pub name: String,
    /// Human-readable description shown to the model.
    pub description: String,
    /// JSON Schema (as a JSON string) describing the tool's argument object.
    pub schema_json: String,
    /// Id of the registered JS `run` callback in the plugin host, invoked when the tool is called.
    pub function_id: JsFunctionId,
}

/// The lenient return value of a plugin tool's `run` callback: a string result, or nothing.
///
/// Mirrors `events::OptionalToast` — using a bare `String` output would error on a `void`/`undefined`
/// return and drop the IPC response ("oneshot cancelled").
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolOutput(pub Option<String>);

impl<'js> FromWarpJs<'js> for ToolOutput {
    fn from_warp_js(
        ctx: rquickjs::Ctx<'js>,
        value: rquickjs::Value<'js>,
        registry: &mut JsFunctionRegistry,
    ) -> rquickjs::Result<Self> {
        if value.is_undefined() || value.is_null() {
            return Ok(ToolOutput(None));
        }
        Ok(ToolOutput(Some(String::from_warp_js(
            ctx, value, registry,
        )?)))
    }
}

static REGISTRY: LazyLock<Mutex<HashMap<String, PluginTool>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Registers (or replaces) a plugin tool. Called on the app process when a plugin invokes
/// `warp.ai.registerTool`.
pub fn register(tool: PluginTool) {
    log::info!(
        "Registered plugin AI tool {:?} ({})",
        tool.name,
        tool.description
    );
    REGISTRY.lock().unwrap().insert(tool.name.clone(), tool);
}

/// Returns a snapshot of all registered plugin tools.
pub fn all() -> Vec<PluginTool> {
    REGISTRY.lock().unwrap().values().cloned().collect()
}

/// Returns the JS `run` callback id for the tool with the given name, if registered.
pub fn function_id(name: &str) -> Option<JsFunctionId> {
    REGISTRY.lock().unwrap().get(name).map(|t| t.function_id)
}
