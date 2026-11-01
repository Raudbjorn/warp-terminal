use super::*;

#[test]
fn aliases_cannot_bypass_scan_guards() {
    let directory = tempfile::tempdir().unwrap();
    if let Some(home) = dirs::home_dir() {
        let alias = directory.path().join("home-alias");
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        assert!(contains_home_directory(&alias));
    }
    let repo = directory.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(repo.join("nested")).unwrap();
    let alias = directory.path().join("repo-alias");
    std::os::unix::fs::symlink(repo.join("nested"), &alias).unwrap();
    assert!(is_inside_git_working_tree(&alias));
    assert!(!is_inside_git_working_tree(directory.path()));
}
