use std::collections::HashMap;
use std::ffi::OsString;

use warp_core::channel::ChannelState;

pub(crate) const FOCUS_URL_ENV: &str = "WARP_FOCUS_URL";
pub(crate) const TERMINAL_SESSION_UUID_ENV: &str = "WARP_TERMINAL_SESSION_UUID";

pub(crate) fn session_focus_url(session_uuid_hex: &str) -> String {
    format!(
        "{}://session/{session_uuid_hex}",
        ChannelState::url_scheme()
    )
}

/// Environment variables that identify the CLI agent session Warp itself was
/// launched from, rather than anything about the shells it starts.
///
/// Prefixes, not exact names, because the set changes with the agent.
/// `CLAUDE_CONFIG_DIR` is deliberately not matched: it is the user's own
/// configuration, not a session marker.
const LAUNCHER_AGENT_ENV_PREFIXES: &[&str] =
    &["CLAUDECODE", "CLAUDE_CODE_", "CLAUDE_PID", "AI_AGENT"];

/// Drops the launching agent session's markers from this process, so the
/// shells Warp starts do not inherit them.
///
/// Launching Warp from inside a Claude Code session otherwise makes every
/// terminal in it a *child* of that session. Claude Code sees the inherited
/// `CLAUDE_CODE_CHILD_SESSION` marker and turns transcript saving off, which
/// silently removes the record Conn reads — and the agent in the pane belongs
/// to the pane, not to whatever started the terminal.
///
/// # Safety
///
/// `remove_var` races with any thread reading the environment, so this must be
/// called before the process starts any.
pub unsafe fn clear_launcher_agent_env() {
    let doomed: Vec<OsString> = std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| {
            let Some(name) = name.to_str() else {
                return false;
            };
            LAUNCHER_AGENT_ENV_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
        .collect();
    for name in doomed {
        // SAFETY: the caller promises this runs before any other thread exists.
        unsafe { std::env::remove_var(name) };
    }
}

pub fn add_session_focus_env_vars(env_vars: &mut HashMap<OsString, OsString>, session_uuid: &[u8]) {
    let session_uuid_hex = hex::encode(session_uuid);
    env_vars.insert(
        OsString::from(TERMINAL_SESSION_UUID_ENV),
        OsString::from(session_uuid_hex.clone()),
    );
    env_vars.insert(
        OsString::from(FOCUS_URL_ENV),
        OsString::from(session_focus_url(&session_uuid_hex)),
    );
}

#[cfg(test)]
#[path = "focus_env_tests.rs"]
mod tests;
