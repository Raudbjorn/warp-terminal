use std::path::Path;

use warp_util::git::run_git_command;

/// Names the project a session runs in.
///
/// Every worktree of a repository shares its common git directory, so naming the project after
/// that directory groups worktrees together. Outside git, the working directory names it.
pub(crate) async fn resolve_project(cwd: &Path) -> Option<String> {
    let common_dir = run_git_command(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await
    .ok();
    common_dir
        .as_deref()
        .map(str::trim)
        .and_then(|dir| repository_key(Path::new(dir)))
        .or_else(|| project_key(cwd))
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
