//! The implementation of `RegisterToolService` served by the app process to the plugin host.

use std::fmt;

use async_trait::async_trait;

use crate::plugin::ai_tools::PluginTool;
use crate::plugin::service::{RegisterToolRequest, RegisterToolResponse, RegisterToolService};

#[derive(Clone, Default)]
pub struct RegisterToolServiceImpl {}

impl RegisterToolServiceImpl {
    pub fn new() -> Self {
        Self {}
    }
}

impl fmt::Debug for RegisterToolServiceImpl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisterToolService").finish()
    }
}

#[async_trait]
impl ipc::ServiceImpl for RegisterToolServiceImpl {
    type Service = RegisterToolService;

    async fn handle_request(&self, request: RegisterToolRequest) -> RegisterToolResponse {
        crate::plugin::ai_tools::register(PluginTool {
            name: request.name,
            description: request.description,
            schema_json: request.schema_json,
            function_id: request.function_id,
        });
        RegisterToolResponse { success: true }
    }
}
