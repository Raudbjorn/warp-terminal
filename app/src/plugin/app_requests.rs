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

use crate::context_chips::plugin_prompt::PromptSegment;

/// One entry of a plugin-driven picker (`warp.ui.showPalette`): a label plus the id of the plugin
/// command (registered via `warp.commands.register`) to run when the user picks it.
///
/// Items reference command ids — not inline callbacks — on purpose: the command is dispatched as a
/// fresh top-level `WorkspaceAction::RunPluginCommand`. Registering a callback at `showPalette` time
/// would re-enter `plugin.get_mut()` while the calling command callback already holds that borrow,
/// panicking the plugin host (`BorrowMutError`). See PLUGIN_SPEC.md (M4).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PalettePluginItem {
    pub label: String,
    pub command_id: String,
}

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
    /// Show a markdown panel (`warp.ui.showMarkdown`).
    ShowMarkdown { title: String, markdown: String },
    /// Show a picker; selecting an item invokes its callback (`warp.ui.showPalette`).
    ShowPalette {
        title: String,
        items: Vec<PalettePluginItem>,
    },
    /// Open an embedded browser pane navigated to `url` (`warp.ui.openWebTab`).
    OpenWebTab { url: String },
    /// Open a new tab with a terminal rooted at `path` (`warp.ui.openProject`).
    OpenProject { path: String },
    /// Replace a plugin's prompt segments (`warp.prompt.set`; empty `segments` clears them).
    SetPrompt {
        plugin_id: String,
        segments: Vec<PromptSegment>,
    },
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
