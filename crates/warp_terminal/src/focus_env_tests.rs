use std::collections::HashMap;
use std::ffi::OsString;

use warp_core::channel::ChannelState;

use super::{FOCUS_URL_ENV, TERMINAL_SESSION_UUID_ENV, add_session_focus_env_vars};

#[test]
fn focus_env_vars_point_at_session_deeplink() {
    let uuid = [
        0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44, 0x00,
        0x00,
    ];
    let mut env_vars = HashMap::new();

    add_session_focus_env_vars(&mut env_vars, &uuid);

    let expected_hex = "550e8400e29b41d4a716446655440000";
    assert_eq!(
        env_vars.get(&OsString::from(TERMINAL_SESSION_UUID_ENV)),
        Some(&OsString::from(expected_hex))
    );
    assert_eq!(
        env_vars.get(&OsString::from(FOCUS_URL_ENV)),
        Some(&OsString::from(format!(
            "{}://session/{expected_hex}",
            ChannelState::url_scheme()
        )))
    );
}

/// Launching Warp from inside a Claude Code session used to make every
/// terminal in it a child of that session, which turns transcript saving off.
/// The markers describe whatever started Warp, never the shells Warp starts.
#[test]
fn launcher_agent_markers_are_dropped_but_user_config_is_kept() {
    // SAFETY: this test owns the variables it sets and runs before any thread
    // it spawns; nextest gives each test its own process.
    unsafe {
        std::env::set_var("CLAUDE_CODE_CHILD_SESSION", "1");
        std::env::set_var("CLAUDECODE", "1");
        std::env::set_var("CLAUDE_PID", "123");
        std::env::set_var("CLAUDE_CONFIG_DIR", "/somewhere/.claude");
        super::clear_launcher_agent_env();
    }

    assert!(std::env::var_os("CLAUDE_CODE_CHILD_SESSION").is_none());
    assert!(std::env::var_os("CLAUDECODE").is_none());
    assert!(std::env::var_os("CLAUDE_PID").is_none());
    assert_eq!(
        std::env::var_os("CLAUDE_CONFIG_DIR"),
        Some("/somewhere/.claude".into()),
        "the user's own configuration is not a session marker"
    );
}
