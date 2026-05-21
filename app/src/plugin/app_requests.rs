//! oh-my-warp: host→app requests that must run with an `AppContext` (Milestone 3).
//!
//! Some plugin APIs (`warp.ui.toast`, `warp.keymap.bind`) need to touch app state that requires a
//! foreground `AppContext` — but the IPC service handler that receives them runs on a background
//! thread without one. So the handler enqueues a [`PluginAppRequest`] onto this channel, and the
//! [`crate::plugin::PluginHost`] model drains it on the foreground executor (via
//! `spawn_stream_local`), where it has the `ModelContext` needed to show a toast or register a
//! keybinding. See PLUGIN_SPEC.md (M3).

use std::sync::OnceLock;

use async_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

/// Severity of a toast requested via `warp.ui.toast`.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub enum ToastKind {
    #[default]
    Info,
    Warn,
    Error,
}

/// A request from the plugin host that must be handled on the app's foreground executor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PluginAppRequest {
    /// Show a transient toast (`warp.ui.toast`).
    ShowToast { message: String, kind: ToastKind },
    /// Bind a key sequence to a registered plugin command (`warp.keymap.bind`).
    BindKey { keys: String, command_id: String },
}

static SENDER: OnceLock<Sender<PluginAppRequest>> = OnceLock::new();

/// Creates the request channel and returns the receiver. Called once by `PluginHost::new`; the
/// sender is stored globally so IPC handlers can enqueue requests without a context.
pub fn init_channel() -> Receiver<PluginAppRequest> {
    let (sender, receiver) = async_channel::unbounded();
    let _ = SENDER.set(sender);
    receiver
}

/// Enqueues a request to be handled on the app's foreground executor. No-op if the channel hasn't
/// been initialized (e.g. plugin host failed to start).
pub fn send(request: PluginAppRequest) {
    if let Some(sender) = SENDER.get() {
        if let Err(e) = sender.try_send(request) {
            log::warn!("Failed to enqueue plugin app request: {e:?}");
        }
    }
}
