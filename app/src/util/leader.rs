//! oh-my-warp: tmux-style "leader key" (prefix) bindings.
//!
//! Press the leader (default `ctrl-b`), release, then press a suffix key to run
//! an action — e.g. `ctrl-b ,` renames the current tab, the way tmux's
//! `prefix ,` renames a window.
//!
//! The chord engine itself is upstream: `Matcher::push_keystroke` already
//! matches multi-keystroke sequences (it returns `Pending` after the leader and
//! the matching `Action` after the suffix), and the input pipeline only forwards
//! a key to the shell on `MatchResult::None`. So all this module does is register
//! a curated, easy-to-extend table of `<leader> <suffix>` bindings on the
//! `Workspace` context, which is an ancestor of the focused terminal.
//!
//! ## Adding your own shortcut
//! Add a row to `leader_bindings`: `("<suffix>", WorkspaceAction::Something)`.
//! The suffix is any key string accepted by `Keystroke::parse` (e.g. `"c"`,
//! `"&"`, `"n"`), then rebuild Warp. Only fieldless `WorkspaceAction`s work in
//! this table; actions that need arguments (pane splits, locator-based pane ops)
//! are a planned follow-up.
//!
//! ## Note on `ctrl-b`
//! `ctrl-b` is the control character `^B`. Like tmux, claiming it as the leader
//! captures it from the shell (you lose readline's backward-char) while a Warp
//! session is focused. The leader keystroke is allowlisted past Warp's
//! PTY-conflict check via `PTY_NON_COMPLIANT_KEYSTROKES` in
//! `crate::util::bindings`; if you change `LEADER` to another `ctrl-<letter>`,
//! add that keystroke to the allowlist there too or binding validation will fail.

use crate::workspace::WorkspaceAction;
use warpui::id;
use warpui::keymap::FixedBinding;
use warpui::AppContext;

/// The leader (prefix) keystroke. Change this to use a different prefix; if you
/// pick another `ctrl-<letter>`, also add it to `PTY_NON_COMPLIANT_KEYSTROKES`
/// in `crate::util::bindings`.
pub const LEADER: &str = "ctrl-b";

/// The curated leader chords: `(suffix_key, action)`. Each entry becomes the
/// sequence `<LEADER> <suffix>`. Add your own rows here.
fn leader_bindings() -> Vec<(&'static str, WorkspaceAction)> {
    vec![
        (",", WorkspaceAction::RenameActiveTab), // tmux: rename window
        ("c", WorkspaceAction::AddDefaultTab),   // tmux: new window
        ("&", WorkspaceAction::CloseActiveTab),  // tmux: kill window
        ("n", WorkspaceAction::ActivateNextTab), // tmux: next window
        ("p", WorkspaceAction::ActivatePrevTab), // tmux: previous window
    ]
}

/// Registers the leader chords on the `Workspace` context. Called once at
/// startup from `crate::workspace::init`.
pub fn register_leader_bindings(app: &mut AppContext) {
    let bindings: Vec<FixedBinding> = leader_bindings()
        .into_iter()
        .map(|(suffix, action)| {
            FixedBinding::new(format!("{LEADER} {suffix}"), action, id!("Workspace"))
        })
        .collect();
    app.register_fixed_bindings(bindings);
}
