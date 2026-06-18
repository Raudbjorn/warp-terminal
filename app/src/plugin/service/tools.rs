//! IPC service for registering AI agent tools defined by JS plugins.
//!
//! Hosted by the app process and called by the plugin host process when a plugin invokes
//! `warp.ai.registerTool`. See [`crate::plugin::ai_tools`].

use serde::{Deserialize, Serialize};
use warp_js::JsFunctionId;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct RegisterToolRequest {
    /// Tool name the model calls (unique across plugins).
    pub name: String,
    /// Human-readable description shown to the model.
    pub description: String,
    /// JSON Schema (as a JSON string) describing the tool's argument object.
    pub schema_json: String,
    /// Id of the JS `run` callback (registered in the host) to invoke when the tool is called.
    pub function_id: JsFunctionId,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct RegisterToolResponse {
    /// `true` if the request succeeded, `false` otherwise.
    pub success: bool,
}

/// IPC service to register plugin AI tools in the app-side tool registry.
pub struct RegisterToolService {}

impl ipc::Service for RegisterToolService {
    type Request = RegisterToolRequest;
    type Response = RegisterToolResponse;
}
