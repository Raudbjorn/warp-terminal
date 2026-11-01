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
        .and_then(|dir| repository_name(Path::new(dir)))
        .or_else(|| file_name(cwd))
}

/// `<repo>/.git` for a checkout, `<repo>.git` for a bare repository, and
/// `.git/modules/<repo>` for a submodule.
fn repository_name(common_dir: &Path) -> Option<String> {
    if common_dir.file_name()? == ".git" {
        return file_name(common_dir.parent()?);
    }
    let name = file_name(common_dir)?;
    Some(name.strip_suffix(".git").map(str::to_owned).unwrap_or(name))
}

fn file_name(path: &Path) -> Option<String> {
    Some(path.file_name()?.to_string_lossy().into_owned())
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
