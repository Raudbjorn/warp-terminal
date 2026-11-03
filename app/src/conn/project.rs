use std::path::Path;

use warp_util::git::run_git_command;

/// Names the project a session runs in.
///
/// Every worktree of a repository shares its common git directory, so naming the project after
/// that directory groups worktrees together. Outside git, the working directory names it.
pub(crate) async fn resolve_project(cwd: &Path) -> Option<String> {
    // The working directory is what the terminal said it was, and a remote host controls that.
    // Naming a network share as the directory git runs in is enough to make Windows authenticate
    // to it, so such a directory is named after itself and never asked about.
    let common_dir = if names_network_host(cwd) {
        None
    } else {
        run_git_command(
            cwd,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .await
        .ok()
    };
    common_dir
        .as_deref()
        .map(str::trim)
        .and_then(|dir| repository_key(Path::new(dir)))
        .or_else(|| project_key(cwd))
}

/// Whether `path` names a host on the network, which asking the filesystem about can send
/// credentials to. A WSL distribution is spelt like one but is local, and is how a project in WSL
/// is reached, so it is not counted.
///
/// Judged by spelling alone. Only a Windows path can carry such a prefix, so elsewhere this is
/// always false.
pub(crate) fn names_network_host(path: &Path) -> bool {
    use std::path::{Component, Prefix};

    let network_prefix = path.components().any(|component| {
        matches!(
            component,
            Component::Prefix(prefix)
                if matches!(
                    prefix.kind(),
                    Prefix::UNC(..) | Prefix::VerbatimUNC(..) | Prefix::DeviceNS(..)
                )
        )
    });
    network_prefix && warp_util::path::parse_wsl_unc_path(path).is_none()
}

/// Returns a stable grouping key while leaving the display label to the overview.
fn repository_key(common_dir: &Path) -> Option<String> {
    if common_dir.file_name()? == ".git" {
        return Some(common_dir.parent()?.to_string_lossy().into_owned());
    }
    Some(common_dir.to_string_lossy().into_owned())
}

fn project_key(path: &Path) -> Option<String> {
    path.file_name()?;
    Some(path.to_string_lossy().into_owned())
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
