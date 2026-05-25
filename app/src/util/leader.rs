//! oh-my-warp: tmux-style "leader key" (prefix) bindings.
//!
//! Press the leader (default `ctrl-b`), release, then press a suffix key to run
//! an action — e.g. `ctrl-b ,` renames the current tab, the way tmux's
//! `prefix ,` renames a window.
//!
//! The chord engine itself is upstream: `Matcher::push_keystroke` already
//! matches multi-keystroke sequences (it returns `Pending` after the leader and
//! the matching `Action` after the suffix), and the input pipeline only forwards
//! a key to the shell on `MatchResult::None`. This module registers a curated set
//! of `<leader> <suffix>` chords on the `Workspace` context (an ancestor of the
//! focused terminal).
//!
//! ## Configurability
//! Each chord is a **named [`EditableBinding`]** (e.g. `leader:rename_tab`) in the
//! `Leader` group, so it shows up in Settings → Keyboard Shortcuts and can be
//! overridden in `keybindings.yaml`. The user config format already supports
//! multi-keystroke sequences, so you can rebind a whole chord — even change the
//! prefix — from the config file, for example:
//!
//! ```yaml
//! "leader:rename_tab": "ctrl-a r"
//! ```
//!
//! (Note: the *GUI* shortcut editor captures a single chord; edit `keybindings.yaml`
//! directly to set multi-keystroke sequences — same as other sequence bindings.)
//!
//! ## Adding your own shortcut
//! Add a row to `leader_bindings`: `(name, description, suffix, action)`. The
//! suffix is any key string accepted by `Keystroke::parse` (e.g. `"c"`, `"&"`,
//! `"n"`), then rebuild Warp. Only fieldless `WorkspaceAction`s work in this
//! table; actions that need arguments (pane splits, locator-based pane ops) are a
//! planned follow-up.
//!
//! ## Note on `ctrl-b`
//! `ctrl-b` is the control character `^B`. Like tmux, claiming it as the leader
//! captures it from the shell (you lose readline's backward-char) while a Warp
//! session is focused. The default leader keystroke is allowlisted past Warp's
//! PTY-conflict check via `PTY_NON_COMPLIANT_KEYSTROKES` in `crate::util::bindings`;
//! if you change `LEADER` (or rebind to another `ctrl-<letter>`) add that keystroke
//! to the allowlist there too, or binding validation will fail.

use crate::util::bindings::BindingGroup;
use crate::workspace::WorkspaceAction;
use warpui::id;
use warpui::keymap::EditableBinding;
use warpui::AppContext;

/// The default leader (prefix) keystroke. Used to build each chord's default
/// trigger; users can override individual chords (or the prefix) in
/// `keybindings.yaml`. If you change this to another `ctrl-<letter>`, also add it
/// to `PTY_NON_COMPLIANT_KEYSTROKES` in `crate::util::bindings`.
pub const LEADER: &str = "ctrl-b";

/// The curated leader chords: `(binding_name, description, suffix_key, action)`.
/// Each entry registers a named editable binding whose default trigger is
/// `<LEADER> <suffix>`. Add your own rows here.
fn leader_bindings() -> Vec<(&'static str, &'static str, &'static str, WorkspaceAction)> {
    vec![
        (
            "leader:rename_tab",
            "Leader: Rename Tab",
            ",",
            WorkspaceAction::RenameActiveTab,
        ), // tmux: rename window
        (
            "leader:new_tab",
            "Leader: New Tab",
            "c",
            WorkspaceAction::AddDefaultTab,
        ), // tmux: new window
        (
            "leader:close_tab",
            "Leader: Close Tab",
            "&",
            WorkspaceAction::CloseActiveTab,
        ), // tmux: kill window
        (
            "leader:next_tab",
            "Leader: Next Tab",
            "n",
            WorkspaceAction::ActivateNextTab,
        ), // tmux: next window
        (
            "leader:prev_tab",
            "Leader: Previous Tab",
            "p",
            WorkspaceAction::ActivatePrevTab,
        ), // tmux: previous window
        (
            "leader:open_browser",
            "Leader: Open Browser",
            "w",
            WorkspaceAction::OpenBrowserPane,
        ), // oh-my-warp: embedded browser pane (w = web)
        (
            "leader:toggle_broadcast",
            "Leader: Toggle Broadcast Input",
            "s",
            WorkspaceAction::ToggleSyncTerminalInputsInTab,
        ), // tmux: synchronize-panes (s = sync) — type once, send to every pane in the tab
    ]
}

/// Registers the leader chords as named editable bindings on the `Workspace`
/// context. Called once at startup from `crate::workspace::init`.
pub fn register_leader_bindings(app: &mut AppContext) {
    let bindings: Vec<EditableBinding> = leader_bindings()
        .into_iter()
        .map(|(name, description, suffix, action)| {
            EditableBinding::new(name, description, action)
                .with_context_predicate(id!("Workspace"))
                .with_key_binding(format!("{LEADER} {suffix}"))
                .with_group(BindingGroup::Leader.as_str())
        })
        .collect();
    app.register_editable_bindings(bindings);
}
