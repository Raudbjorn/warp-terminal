//! The implementation of `RegisterEventHandlerService` served by the app process to the plugin host.

use std::fmt;

use async_trait::async_trait;

use crate::plugin::service::{
    RegisterEventHandlerRequest, RegisterEventHandlerResponse, RegisterEventHandlerService,
};

#[derive(Clone, Default)]
pub struct RegisterEventHandlerServiceImpl {}

impl RegisterEventHandlerServiceImpl {
    pub fn new() -> Self {
        Self {}
    }
}

impl fmt::Debug for RegisterEventHandlerServiceImpl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisterEventHandlerService").finish()
    }
}

#[async_trait]
impl ipc::ServiceImpl for RegisterEventHandlerServiceImpl {
    type Service = RegisterEventHandlerService;

    async fn handle_request(
        &self,
        request: RegisterEventHandlerRequest,
    ) -> RegisterEventHandlerResponse {
        crate::plugin::events::register_handler(request.event, request.function_id);
        RegisterEventHandlerResponse { success: true }
    }
}
