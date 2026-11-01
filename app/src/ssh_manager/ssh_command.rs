use std::time::Duration;

use crate::terminal::ssh::util::parse_interactive_ssh_command;

pub const SSH_PROFILE_WARPIFY_DISABLED_COMMAND_TTL: Duration = Duration::from_secs(30);

pub fn normalize_ssh_command(command: &str) -> &str {
    let command = command.trim();
    command
        .strip_prefix("command -p ")
        .or_else(|| command.strip_prefix("command "))
        .unwrap_or(command)
        .trim()
}

/// Matches the actual SSH argv, so another proxy/config cannot inherit a managed command's state.
pub fn same_ssh_command(a: &str, b: &str) -> bool {
    let a = normalize_ssh_command(a);
    let b = normalize_ssh_command(b);
    if a == b && a.starts_with("ssh ") {
        return true;
    }
    if parse_interactive_ssh_command(a)
        .and_then(|command| command.host)
        .is_none()
    {
        return false;
    }
    match (shell_words::split(a), shell_words::split(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
#[path = "ssh_command_tests.rs"]
mod tests;
