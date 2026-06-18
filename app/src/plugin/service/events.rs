//! IPC service for registering terminal-event handlers defined by JS plugins.
//!
//! Hosted by the app process and called by the plugin host when a plugin invokes
//! `warp.terminal.on*`. See [`crate::plugin::events`].

use serde::{Deserialize, Serialize};
use warp_js::JsFunctionId;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct RegisterEventHandlerRequest {
    /// Event name (e.g. `"command_finished"`).
    pub event: String,
    /// Id of the JS callback (registered in the host) to invoke when the event fires.
    pub function_id: JsFunctionId,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct RegisterEventHandlerResponse {
    /// `true` if the request succeeded, `false` otherwise.
    pub success: bool,
}

/// IPC service to register plugin terminal-event handlers in the app-side event registry.
pub struct RegisterEventHandlerService {}

impl ipc::Service for RegisterEventHandlerService {
    type Request = RegisterEventHandlerRequest;
    type Response = RegisterEventHandlerResponse;
}
