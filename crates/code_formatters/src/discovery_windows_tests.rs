use super::*;

#[test]
fn resolves_native_executables_and_prefers_windows_node_shims() {
    let dir = tempfile::tempdir().unwrap();
    let prettier = dir.path().join("prettier");
    fs::write(&prettier, "#!/bin/sh").unwrap();
    fs::write(prettier.with_extension("cmd"), "@echo off").unwrap();
    assert_eq!(
        executable_path(&prettier),
        Some(prettier.with_extension("cmd"))
    );
    let ruff = dir.path().join("ruff.exe");
    fs::write(&ruff, "test fixture").unwrap();
    assert_eq!(find_on_path("ruff", dir.path().to_str()), Some(ruff));
    assert_eq!(executable_path(&dir.path().join("absent")), None);
}
