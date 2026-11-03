//! Harp mode's process-level pieces: which Claude Code plugin new shells get, and a check for the
//! external tools the mode relies on.
//!
//! Everything here is a no-op while `modes.harp.enabled` is off, so the app behaves as on master.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use warp_util::path::resolve_executable_in_path;
use warpui::{AppContext, SingletonEntity};

use crate::tab::harp_mode_enabled;
use crate::workspace::tab_settings::{TabSettings, TabSettingsChangedEvent};

/// Path of the Claude Code plugin below `bundled_resources_dir()`, as staged by
/// `script/prepare_bundled_resources` from `resources/bundled`.
const PLUGIN_PATH: [&str; 3] = ["bundled", "claude-plugin", "harp"];
/// The plugin's hook manifest; without it Claude Code would load an empty plugin.
const HOOKS_MANIFEST_PATH: [&str; 2] = ["hooks", "hooks.json"];
/// Every hook script of the plugin parses Claude Code's JSON with `jq`.
const JQ: &str = "jq";
/// The tmux sync (`integrations.tmux.enabled`) shells out to `tmux`.
const TMUX: &str = "tmux";

/// The bundled Claude Code status plugin to inject into new shells, or `None` when Harp mode is off
/// or the plugin is not where the bundle puts it (e.g. a dev build without `WARP_RESOURCES_DIR`).
///
/// Called once per new PTY on the main thread; with the mode on that costs a canonicalize and a
/// stat of local files, the same order as the rest of PTY setup.
#[cfg_attr(not(feature = "local_tty"), allow(dead_code))]
pub(crate) fn claude_plugin_dir(ctx: &AppContext) -> Option<PathBuf> {
    claude_plugin_dir_from(ctx, warp_core::paths::bundled_resources_dir)
}

/// [`claude_plugin_dir`] with the resources dir lookup injected, so tests need not touch the
/// process environment. `resources_dir` is only called when the mode is on.
fn claude_plugin_dir_from(
    ctx: &AppContext,
    resources_dir: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    if !harp_mode_enabled(ctx) {
        return None;
    }
    plugin_dir_in(&resources_dir()?)
}

/// The Harp plugin dir inside the resources dir `resources`, if its hook manifest is a file.
fn plugin_dir_in(resources: &Path) -> Option<PathBuf> {
    let plugin: PathBuf = PLUGIN_PATH
        .iter()
        .fold(resources.to_path_buf(), |dir, part| dir.join(part));
    let manifest = HOOKS_MANIFEST_PATH
        .iter()
        .fold(plugin.clone(), |dir, part| dir.join(part));
    manifest.is_file().then_some(plugin)
}

/// Logs what Harp mode cannot do on this machine: no bundled plugin (no agent status at all), no
/// `jq` (the plugin's hooks emit nothing) and, with the tmux sync on, no `tmux`. Only warns; the
/// rest of the app is unaffected.
///
/// Tools are looked up on Warp's own `PATH`, which can differ from the PATH a login shell builds,
/// so a tool installed only into a shell-rc PATH may be reported missing (and the reverse).
pub(crate) fn warn_missing_tools(ctx: &AppContext) {
    let resources_dir = warp_core::paths::bundled_resources_dir();
    if resources_dir.as_deref().and_then(plugin_dir_in).is_none() {
        log::warn!(
            "Harp mode: Claude Code plugin not found; agent status rows will stay empty. \
             resources_dir={:?} expected={}/{} hint=on Linux, set {} to a checkout's resources/ \
             for dev builds",
            resources_dir,
            PLUGIN_PATH.join("/"),
            HOOKS_MANIFEST_PATH.join("/"),
            warp_core::paths::RESOURCES_DIR_ENV,
        );
    }

    let tmux_sync = *TabSettings::as_ref(ctx).tmux_sync_enabled;
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for tool in missing_tools(&path_var, tmux_sync) {
        let effect = if tool == JQ {
            "the Claude Code status plugin's hooks emit nothing, so agent status rows stay empty"
        } else {
            "the tmux sync cannot read or rename tmux sessions"
        };
        log::warn!(
            "Harp mode: required tool not found on Warp's PATH. tool={tool} effect={effect}"
        );
    }
}

/// The tools Harp mode needs that `path_var` does not provide.
fn missing_tools(path_var: &OsStr, tmux_sync: bool) -> Vec<&'static str> {
    let mut needed = vec![JQ];
    if tmux_sync {
        needed.push(TMUX);
    }
    needed.retain(|tool| resolve_executable_in_path(tool, path_var).is_none());
    needed
}

/// Checks Harp mode's tools once at startup if the mode is on, and again whenever the mode or the
/// tmux sync (which adds `tmux` to the list) is turned on.
pub(crate) fn init(app: &mut AppContext) {
    if harp_mode_enabled(app) {
        warn_missing_tools(app);
    }
    app.subscribe_to_model(&TabSettings::handle(app), |_, event, ctx| {
        let turned_on = match event {
            TabSettingsChangedEvent::HarpMode { .. } => true,
            TabSettingsChangedEvent::TmuxSyncEnabled { .. } => {
                *TabSettings::as_ref(ctx).tmux_sync_enabled
            }
            _ => false,
        };
        if turned_on && harp_mode_enabled(ctx) {
            warn_missing_tools(ctx);
        }
    });
}

#[cfg(test)]
#[path = "harp_mode_tests.rs"]
mod tests;
