use std::fs;

use super::*;

const NO_LIMIT: u64 = u64::MAX;

fn temp_file(content: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("file.txt");
    fs::write(&path, content).unwrap();
    (directory, path)
}

fn names_in(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn a_regular_file_is_read_and_rewritten() {
    let (directory, path) = temp_file(b"before");
    let (bytes, stamp) = read_rewritable(&path, NO_LIMIT).unwrap();
    assert_eq!(bytes, b"before");
    assert!(stamp.still_matches(&path));

    write_atomically(&path, b"after and longer", stamp.metadata()).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"after and longer");
    assert!(!stamp.still_matches(&path));
    assert_eq!(names_in(directory.path()), ["file.txt"]);
}

#[cfg(unix)]
#[test]
fn rewriting_keeps_the_permissions_of_the_file() {
    use std::os::unix::fs::PermissionsExt;

    for mode in [0o644, 0o755, 0o600, 0o664] {
        let (_directory, path) = temp_file(b"#!/bin/sh\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();

        let (_, stamp) = read_rewritable(&path, NO_LIMIT).unwrap();
        write_atomically(&path, b"#!/bin/bash\n", stamp.metadata()).unwrap();

        let rewritten = fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(rewritten, mode, "mode {mode:o}");
        assert_eq!(fs::read(&path).unwrap(), b"#!/bin/bash\n");
    }
}

#[cfg(unix)]
#[test]
fn a_failed_write_leaves_the_file_and_no_temporary_file() {
    use std::os::unix::fs::PermissionsExt;

    let (directory, path) = temp_file(b"before");
    let (_, stamp) = read_rewritable(&path, NO_LIMIT).unwrap();

    // A directory that cannot be written to refuses the temporary file.
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let result = write_atomically(&path, b"after", stamp.metadata());
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();

    assert!(result.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"before");
    assert_eq!(names_in(directory.path()), ["file.txt"]);
}

#[test]
fn a_missing_file_is_reported() {
    let directory = tempfile::tempdir().unwrap();
    let result = read_rewritable(&directory.path().join("gone.txt"), NO_LIMIT);
    assert_eq!(result.err(), Some(SkipReason::Missing));
}

#[test]
fn a_directory_is_not_a_file() {
    let directory = tempfile::tempdir().unwrap();
    let result = read_rewritable(directory.path(), NO_LIMIT);
    assert_eq!(result.err(), Some(SkipReason::NotAFile));
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_is_left_alone() {
    let (directory, path) = temp_file(b"target");
    let link = directory.path().join("link.txt");
    std::os::unix::fs::symlink(&path, &link).unwrap();

    assert_eq!(
        read_rewritable(&link, NO_LIMIT).err(),
        Some(SkipReason::Symlink)
    );
    assert!(read_rewritable(&path, NO_LIMIT).is_ok());
}

#[cfg(any(unix, windows))]
#[test]
fn a_file_with_other_hard_links_is_left_alone() {
    let (directory, path) = temp_file(b"shared");
    fs::hard_link(&path, directory.path().join("other-name.txt")).unwrap();
    assert_eq!(
        read_rewritable(&path, NO_LIMIT).err(),
        Some(SkipReason::HardLinked)
    );
}

#[test]
fn a_file_over_the_size_limit_is_left_alone() {
    let (_directory, path) = temp_file(&[b'x'; 100]);
    assert_eq!(
        read_rewritable(&path, 99).err(),
        Some(SkipReason::TooLarge {
            bytes: 100,
            limit: 99
        })
    );
    assert!(read_rewritable(&path, 100).is_ok());
}

#[test]
fn a_read_only_file_is_left_alone() {
    let (_directory, path) = temp_file(b"locked");
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).unwrap();

    assert_eq!(
        read_rewritable(&path, NO_LIMIT).err(),
        Some(SkipReason::ReadOnly)
    );
    assert_eq!(fs::read(&path).unwrap(), b"locked");
}

#[test]
fn a_change_behind_our_back_is_noticed() {
    let (_directory, path) = temp_file(b"before");
    let (_, stamp) = read_rewritable(&path, NO_LIMIT).unwrap();

    fs::write(&path, b"changed by someone else").unwrap();
    assert!(!stamp.still_matches(&path));

    fs::remove_file(&path).unwrap();
    assert!(!stamp.still_matches(&path));
}

#[cfg(unix)]
#[test]
fn private_files_and_directories_are_closed_to_other_users() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let private_directory = directory.path().join("a/b");
    create_private_dir(&private_directory).unwrap();
    // Creating it again must not fail, and must close a directory that was left open.
    fs::set_permissions(&private_directory, fs::Permissions::from_mode(0o755)).unwrap();
    create_private_dir(&private_directory).unwrap();
    let mode = fs::metadata(&private_directory)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);

    let private_file = private_directory.join("copy");
    create_private(&private_file).unwrap();
    let mode = fs::metadata(&private_file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert!(create_private(&private_file).is_err());
}
