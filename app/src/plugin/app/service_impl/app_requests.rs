//! Implementation of `PluginAppRequestService`: enqueues the request onto the foreground-drained
//! [`crate::plugin::app_requests`] channel.

use std::fmt;

use async_trait::async_trait;

use crate::plugin::service::{
    PluginAppRequestEnvelope, PluginAppRequestResponse, PluginAppRequestService,
};

#[derive(Clone, Default)]
pub struct PluginAppRequestServiceImpl {}

impl PluginAppRequestServiceImpl {
    pub fn new() -> Self {
        Self {}
    }
}

impl fmt::Debug for PluginAppRequestServiceImpl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginAppRequestService").finish()
    }
}

#[async_trait]
impl ipc::ServiceImpl for PluginAppRequestServiceImpl {
    type Service = PluginAppRequestService;

    async fn handle_request(&self, request: PluginAppRequestEnvelope) -> PluginAppRequestResponse {
        crate::plugin::app_requests::send(request.request);
        PluginAppRequestResponse { success: true }
    }
}
