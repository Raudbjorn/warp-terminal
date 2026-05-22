mod service_impl;

use std::sync::Arc;

use crate::plugin::app_requests::{PluginAppRequest, ToastKind};
use crate::view_components::{DismissibleToast, ToastFlavor};
use crate::workspace::{ToastStack, WorkspaceAction};
use anyhow::{Context, Result};
use command::blocking::Command;
use service_impl::{
    LogServiceImpl, PluginAppRequestServiceImpl, PluginHostBootstrapServiceImpl,
    RegisterCommandServiceImpl, RegisterEventHandlerServiceImpl, RegisterToolServiceImpl,
};
use warpui::keymap::EditableBinding;
use warpui::{Entity, ModelContext, SingletonEntity};

use super::{PLUGIN_HOST_ADDRESS_ENV_VAR, PLUGIN_HOST_FLAG};

/// Singleton model responsible for spawning the plugin host child process and initializing IPC
/// server and clients for communication between the app and plugin host processes.
pub struct PluginHost {
    /// A handle on the actual plugin host process.
    ///
    /// This is `None` if we fail to spawn the plugin host process.
    host_process: Option<std::process::Child>,

    /// The IPC server that serves app services ([`ipc::Service`] implementations) to the plugin
    /// host process.
    ///
    /// This is `None` if server initialization fails.
    _server: Option<ipc::Server>,

    /// An IPC client for sending requests to the plugin host process.
    ///
    /// This is `None` if the IPC handshake for relaying the plugin host process's connection
    /// address fails.
    host_client: Option<Arc<ipc::Client>>,
}

impl PluginHost {
    #[cfg_attr(not(feature = "plugin_host"), allow(dead_code))]
    pub fn new(ctx: &mut ModelContext<Self>) -> Result<Self> {
        let plugin_host_bootstrap_service = PluginHostBootstrapServiceImpl::new();
        let connection_address_rx = plugin_host_bootstrap_service.connection_address_rx();

        // Drain host→app requests (warp.ui.toast / warp.keymap.bind) on the foreground executor,
        // where we have the `ModelContext` needed to touch app state. See `super::app_requests`.
        let app_request_rx = crate::plugin::app_requests::init_channel();
        ctx.spawn_stream_local(
            app_request_rx,
            |me, request, ctx| me.handle_app_request(request, ctx),
            |_, _| {},
        );

        // Schedule a task that awaits a request containing the connection address for the plugin
        // host process and uses it to instantiate a Client when it's received.
        let background_executor = ctx.background_executor();
        ctx.spawn(
            async move {
                match connection_address_rx.recv().await {
                    Ok(connection_address) => {
                        match ipc::Client::connect(connection_address, background_executor).await {
                            Ok(client) => Some(client),
                            Err(e) => {
                                log::error!("Failed to instantiate LocalSocketClient: {e:?}.");
                                None
                            }
                        }
                    }
                    Err(e) => {
                        log::error!(
                            "Failed to receive connection address for PluginHost services: {e:?}."
                        );
                        None
                    }
                }
            },
            |me, client, _| {
                me.host_client = client.map(Arc::new);
            },
        );

        let server_builder = ipc::ServerBuilder::default()
            .with_service(plugin_host_bootstrap_service)
            .with_service(LogServiceImpl::new())
            .with_service(RegisterCommandServiceImpl::new())
            .with_service(RegisterEventHandlerServiceImpl::new())
            .with_service(RegisterToolServiceImpl::new())
            .with_service(PluginAppRequestServiceImpl::new());

        #[cfg(feature = "completions_v2")]
        let server_builder =
            server_builder.with_service(service_impl::RegisterCommandSignatureServiceImpl::new(
                warp_completer::signatures::CommandRegistry::global_instance(),
            ));

        let (server, plugin_host_process) =
            match server_builder.build_and_run(ctx.background_executor()) {
                Ok((server, connection_address)) => {
                    log::info!("Successfully initialized plugin app server.");

                    // Spawn the plugin host process if the app server was successfully initialized.
                    let program = std::env::current_exe()
                        .context("Failed to determine path to current executable.")?;
                    let plugin_host_process = Command::new(program)
                        .args(std::env::args().skip(1))
                        .arg(PLUGIN_HOST_FLAG)
                        .env(PLUGIN_HOST_ADDRESS_ENV_VAR, connection_address.to_string())
                        .spawn()
                        .context("Failed to spawn plugin host process.")?;
                    log::info!("Successfully spawned plugin host process.");

                    (Some(server), Some(plugin_host_process))
                }
                Err(e) => {
                    log::error!("Could not initialize server: {e:?}.");
                    (None, None)
                }
            };

        Ok(Self {
            host_process: plugin_host_process,
            _server: server,
            host_client: None,
        })
    }

    /// Returns an `ipc::ServiceCaller` for the service specified as `S`.
    ///
    /// `S` is assumed to be served by the plugin host process; the returned service caller directs
    /// requests over the IPC connection to the plugin host process.
    pub fn plugin_service_caller<S: ipc::Service>(&self) -> Option<Box<dyn ipc::ServiceCaller<S>>> {
        self.host_client.clone().map(ipc::service_caller::<S>)
    }

    /// Handles a host→app request on the foreground executor (see [`super::app_requests`]).
    fn handle_app_request(&mut self, request: PluginAppRequest, ctx: &mut ModelContext<Self>) {
        match request {
            PluginAppRequest::ShowToast { message, kind } => {
                let Some(window_id) = ctx.windows().active_window() else {
                    return;
                };
                let flavor = match kind {
                    ToastKind::Error => ToastFlavor::Error,
                    ToastKind::Info | ToastKind::Warn => ToastFlavor::Default,
                };
                ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
                    toast_stack.add_ephemeral_toast(
                        DismissibleToast::new(message, flavor),
                        window_id,
                        ctx,
                    );
                });
            }
            PluginAppRequest::BindKey { keys, command_id } => {
                let title = crate::plugin::commands::title(&command_id)
                    .unwrap_or_else(|| command_id.clone());
                // `EditableBinding::new` takes a `&'static str` name; plugin command ids are
                // dynamic, so leak the (small, session-lived) binding name.
                let name: &'static str =
                    Box::leak(format!("plugin-keymap:{command_id}").into_boxed_str());
                let binding = EditableBinding::new(
                    name,
                    title,
                    WorkspaceAction::RunPluginCommand(command_id),
                )
                .with_context_predicate(warpui::id!("Workspace"))
                .with_key_binding(keys);
                ctx.register_editable_bindings([binding]);
            }
        }
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        if let Some(mut host_process) = self.host_process.take() {
            if let Ok(Some(exit_status)) = host_process.try_wait() {
                log::error!("Plugin host process had exited early with status: {exit_status:?}");
            } else {
                // Calling `wait()` is necessary for the OS to release process resources on some
                // systems; processes that have exited but not been `wait`-ed upon are "zombie"
                // processes that can exhaust OS resources.
                //
                // See https://doc.rust-lang.org/std/process/struct.Child.html#warning for more
                // context.
                let _ = host_process.kill();
                let _ = host_process.wait();
            }
        }
    }
}

impl Entity for PluginHost {
    type Event = ();
}

impl SingletonEntity for PluginHost {}
