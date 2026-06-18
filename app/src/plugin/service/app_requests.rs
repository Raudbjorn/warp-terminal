//! IPC service carrying host→app requests that need an `AppContext` (`warp.ui.toast`,
//! `warp.keymap.bind`). Hosted by the app; the handler enqueues onto the
//! [`crate::plugin::app_requests`] channel drained by the `PluginHost` model.

use serde::{Deserialize, Serialize};

use crate::plugin::app_requests::PluginAppRequest;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PluginAppRequestEnvelope {
    pub request: PluginAppRequest,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PluginAppRequestResponse {
    pub success: bool,
}

/// IPC service that relays a [`PluginAppRequest`] from the plugin host to the app.
pub struct PluginAppRequestService {}

impl ipc::Service for PluginAppRequestService {
    type Request = PluginAppRequestEnvelope;
    type Response = PluginAppRequestResponse;
}
