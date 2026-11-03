use super::*;
use crate::test_support::{Project, UPPERCASE};

const PRETTIER: &[&str] = &["node_modules/.bin/prettier"];

#[test]
fn finds_the_nearest_project_local_executable_walking_up() {
    let project = Project::new();
    let root_bin = project.script("node_modules/.bin/prettier", UPPERCASE);
    let nested_bin = project.script("packages/app/node_modules/.bin/prettier", UPPERCASE);
    let root = project.root();

    let nested = SearchScope::new(&project.path("packages/app/src/deep/a.ts"), Some(&root));
    assert_eq!(nested.find_executable(PRETTIER), Some(nested_bin));

    let sibling = SearchScope::new(&project.path("packages/lib/src/b.ts"), Some(&root));
    assert_eq!(sibling.find_executable(PRETTIER), Some(root_bin));
}

#[test]
fn scope_runs_from_the_file_directory_to_the_project_root() {
    let project = Project::new();
    let root = project.root();
    let scope = SearchScope::new(&project.path("src/ui/a.ts"), Some(&root));

    let dirs: Vec<_> = scope.dirs().collect();
    assert_eq!(
        dirs,
        [project.path("src/ui"), project.path("src"), root.clone()]
    );
    assert_eq!(scope.root(), Some(root.as_path()));
}

#[test]
fn never_looks_above_the_project_root() {
    let project = Project::new();
    project.script("node_modules/.bin/prettier", UPPERCASE);
    project.write(".prettierrc", "{}");
    let nested_root = project.mkdir("vendor/project");

    let scope = SearchScope::new(&project.path("vendor/project/a.ts"), Some(&nested_root));
    assert_eq!(scope.find_executable(PRETTIER), None);
    assert_eq!(scope.find_file(&[".prettierrc"]), None);
}

#[test]
fn widens_a_nested_project_root_to_its_repository() {
    let project = Project::new();
    project.mkdir(".git");
    let root_bin = project.script("node_modules/.bin/prettier", UPPERCASE);
    let nested_root = project.mkdir("packages/app");

    let scope = SearchScope::new(&project.path("packages/app/src/a.ts"), Some(&nested_root));
    assert_eq!(scope.find_executable(PRETTIER), Some(root_bin));
    assert_eq!(scope.root(), Some(project.root().as_path()));
}

#[test]
fn without_a_project_root_the_scope_is_the_repository_or_the_file_directory() {
    let project = Project::new();
    let bin = project.script("repo/node_modules/.bin/prettier", UPPERCASE);
    project.script("loose/node_modules/.bin/prettier", UPPERCASE);
    project.mkdir("repo/.git");

    let in_repo = SearchScope::new(&project.path("repo/src/a.ts"), None);
    assert_eq!(in_repo.find_executable(PRETTIER), Some(bin));

    let loose = SearchScope::new(&project.path("loose/src/a.ts"), None);
    assert_eq!(loose.find_executable(PRETTIER), None);
    assert_eq!(loose.root(), Some(project.path("loose/src").as_path()));
}

#[test]
fn a_project_root_that_does_not_contain_the_file_is_ignored() {
    let project = Project::new();
    let elsewhere = project.mkdir("elsewhere");
    let scope = SearchScope::new(&project.path("src/a.ts"), Some(&elsewhere));
    assert_eq!(scope.root(), Some(project.path("src").as_path()));
}

#[test]
fn relative_file_paths_have_an_empty_scope() {
    let scope = SearchScope::new(Path::new("a.ts"), None);
    assert_eq!(scope.dirs().count(), 0);
    assert_eq!(scope.root(), None);
}

#[test]
fn files_that_are_not_executable_are_not_tools() {
    let project = Project::new();
    project.write("node_modules/.bin/prettier", UPPERCASE);
    project.mkdir("node_modules/.bin/biome");
    let scope = SearchScope::new(&project.path("a.ts"), Some(&project.root()));

    assert_eq!(scope.find_executable(PRETTIER), None);
    assert_eq!(scope.find_executable(&["node_modules/.bin/biome"]), None);
}

#[test]
fn finds_a_global_tool_through_the_injected_path() {
    let project = Project::new();
    project.mkdir("empty");
    let global = project.script("global/bin/prettier", UPPERCASE);
    let path_env = project.path_env(&["empty", "global/bin"]);

    assert_eq!(find_on_path("prettier", Some(&path_env)), Some(global));
    assert_eq!(find_on_path("biome", Some(&path_env)), None);
}

#[test]
fn the_first_path_entry_wins() {
    let project = Project::new();
    let first = project.script("first/ruff", UPPERCASE);
    project.script("second/ruff", UPPERCASE);
    let path_env = project.path_env(&["first", "second"]);

    assert_eq!(find_on_path("ruff", Some(&path_env)), Some(first));
}

#[test]
fn the_process_path_is_never_consulted() {
    // `sh` is on every machine's own PATH, so finding it would mean the process environment
    // leaked into discovery.
    assert_eq!(find_on_path("sh", None), None);
    assert_eq!(find_on_path("sh", Some("")), None);
    assert_eq!(find_on_path("sh", Some("relative/bin:.")), None);
}
