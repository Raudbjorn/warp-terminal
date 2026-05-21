//! IPC service for registering command-palette commands defined by JS plugins.
//!
//! Hosted by the app process and called by the plugin host process when a plugin invokes
//! `warp.commands.register`. See [`crate::plugin::commands`].

use serde::{Deserialize, Serialize};
use warp_js::JsFunctionId;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct RegisterCommandRequest {
    /// Stable command id, unique across plugins (e.g. `"greet.hello"`).
    pub command_id: String,
    /// Human-readable title shown in the command palette.
    pub title: String,
    /// Id of the JS callback (registered in the host) to invoke when the command runs.
    pub function_id: JsFunctionId,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct RegisterCommandResponse {
    /// `true` if the request succeeded, `false` otherwise.
    pub success: bool,
}

/// IPC service to register plugin commands in the app-side command registry.
pub struct RegisterCommandService {}

impl ipc::Service for RegisterCommandService {
    type Request = RegisterCommandRequest;
    type Response = RegisterCommandResponse;
}
