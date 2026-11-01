use super::*;
use crate::test_support::{Project, literal};

#[test]
fn parent_components_cannot_authorize_a_file_outside_the_root() {
    let project = Project::new();
    project.write("inside.txt", "foo\n");
    let outside = project.root().parent().unwrap().join("outside.txt");
    fs::write(&outside, "foo\n").unwrap();
    let spec = literal("foo");
    let mut targets = project.search_all(&spec, &["inside.txt"]);
    targets[0].path = project.root().join("../outside.txt");
    let result = project.replace(&project.request(&spec, "bar", targets));
    assert!(result.replaced.is_empty());
    assert_eq!(result.skipped[0].reason, SkipReason::OutOfScope);
    assert_eq!(fs::read_to_string(outside).unwrap(), "foo\n");
}

#[cfg(unix)]
#[test]
fn a_searched_directory_redirected_outside_the_root_is_left_alone() {
    let project = Project::new();
    project.write("sub/file.txt", "foo\n");
    let spec = literal("foo");
    let targets = project.search_all(&spec, &["sub/file.txt"]);
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("file.txt"), "foo\n").unwrap();
    fs::rename(project.path("sub"), project.path("original-sub")).unwrap();
    std::os::unix::fs::symlink(outside.path(), project.path("sub")).unwrap();
    let result = project.replace(&project.request(&spec, "bar", targets));
    assert!(result.replaced.is_empty());
    assert_eq!(result.skipped[0].reason, SkipReason::OutOfScope);
    assert_eq!(
        fs::read_to_string(outside.path().join("file.txt")).unwrap(),
        "foo\n"
    );
    assert_eq!(project.read_text("original-sub/file.txt"), "foo\n");
}

#[cfg(unix)]
#[test]
fn an_explicitly_symlinked_root_remains_usable_and_undo_records_its_resolved_file() {
    let project = Project::new();
    let path = project.write("file.txt", "foo\n");
    let spec = literal("foo");
    let alias_directory = tempfile::tempdir().unwrap();
    let alias = alias_directory.path().join("project");
    std::os::unix::fs::symlink(project.root(), &alias).unwrap();
    let mut targets = project.search_all(&spec, &["file.txt"]);
    targets[0].path = alias.join("file.txt");
    let mut request = project.request(&spec, "bar", targets);
    request.roots = vec![alias];
    let result = project.replace(&request);
    assert!(result.skipped.is_empty());
    assert_eq!(project.read_text("file.txt"), "bar\n");
    result.finish(Vec::new(), Vec::new());
    let transaction = project.journal.latest().unwrap().unwrap();
    assert_eq!(transaction.entries[0].path, fs::canonicalize(path).unwrap());
}
