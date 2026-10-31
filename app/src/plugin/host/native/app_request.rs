//! oh-my-warp: relays host→app requests (`warp.ui.toast`, `warp.keymap.bind`) to the app process.
//!
//! Mirrors the logger (`super::logging`): a plugin callback enqueues onto a channel — a
//! non-blocking operation — and a background task performs the actual IPC call. This keeps the
//! blocking IPC round-trip OFF the plugin runner thread. Doing the IPC inline inside the QuickJS
//! callback (as an earlier version did) blocks/panics on the runner thread and tears the QuickJS
//! runtime down mid-call, aborting in `JS_FreeRuntime`. See PLUGIN_SPEC.md (M3).

use std::sync::{Arc, OnceLock};

use warpui::r#async::executor::Background;
use crate::plugin::app_requests::PluginAppRequest;
use crate::plugin::service::{PluginAppRequestEnvelope, PluginAppRequestService};

static APP_REQUEST_TX: OnceLock<async_channel::Sender<PluginAppRequest>> = OnceLock::new();

/// Spawns a background task that forwards queued requests to the app via the IPC
/// `PluginAppRequestService`. Called once during plugin host startup.
pub(super) fn initialize_app_request_relay(client: &Arc<ipc::Client>, executor: &Arc<Background>) {
    let caller = ipc::service_caller::<PluginAppRequestService>(client.clone());
    let (tx, rx) = async_channel::unbounded::<PluginAppRequest>();
    let _ = APP_REQUEST_TX.set(tx);
    executor
        .spawn(async move {
            while let Ok(request) = rx.recv().await {
                if let Err(e) = caller.call(PluginAppRequestEnvelope { request }).await {
                    eprintln!("Failed to send plugin app request: {e:?}");
                }
            }
        })
        .detach();
}

/// Enqueues a host→app request (non-blocking). The IPC call is performed on the background
/// executor, never on the calling (plugin runner) thread.
pub(super) fn send_app_request(request: PluginAppRequest) {
    if let Some(tx) = APP_REQUEST_TX.get() {
        // The relay channel is unbounded, so `send` can never block — use the
        // synchronous `try_send` directly. Avoids spinning up a runtime via
        // `block_on` (which would panic if we're already on a tokio executor) and
        // sidesteps the cross-runtime deadlock risk entirely.
        let _ = tx.try_send(request);
    }
}
