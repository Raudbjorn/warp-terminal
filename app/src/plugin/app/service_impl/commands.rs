//! The implementation of `RegisterCommandService` served by the app process to the plugin host.

use std::fmt;

use async_trait::async_trait;

use crate::plugin::commands::PluginCommand;
use crate::plugin::service::{
    RegisterCommandRequest, RegisterCommandResponse, RegisterCommandService,
};

#[derive(Clone, Default)]
pub struct RegisterCommandServiceImpl {}

impl RegisterCommandServiceImpl {
    pub fn new() -> Self {
        Self {}
    }
}

impl fmt::Debug for RegisterCommandServiceImpl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisterCommandService").finish()
    }
}

#[async_trait]
impl ipc::ServiceImpl for RegisterCommandServiceImpl {
    type Service = RegisterCommandService;

    async fn handle_request(&self, request: RegisterCommandRequest) -> RegisterCommandResponse {
        crate::plugin::commands::register(PluginCommand {
            id: request.command_id,
            title: request.title,
            function_id: request.function_id,
        });
        RegisterCommandResponse { success: true }
    }
}
