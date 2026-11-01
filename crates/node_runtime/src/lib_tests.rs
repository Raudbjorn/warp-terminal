use std::io::Write;

use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use super::*;

fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, contents) in entries {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(contents).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[test]
fn extract_zip_writes_regular_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dest");
    let data = build_zip(&[("bin/tool", b"tool"), ("README", b"readme")]);

    futures::executor::block_on(extract_zip(&data, &dest, None::<fn(&str) -> bool>)).unwrap();

    assert_eq!(std::fs::read(dest.join("bin/tool")).unwrap(), b"tool");
    assert_eq!(std::fs::read(dest.join("README")).unwrap(), b"readme");
}

#[test]
fn extract_zip_never_writes_outside_dest_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dest");
    let outside = tmp.path().join("outside");
    let absolute_entry = format!("{}/abs.txt", outside.display());
    let data = build_zip(&[
        ("../evil.txt", b"evil"),
        ("nested/../../evil2.txt", b"evil"),
        (absolute_entry.as_str(), b"evil"),
        ("ok.txt", b"ok"),
    ]);

    futures::executor::block_on(extract_zip(&data, &dest, None::<fn(&str) -> bool>)).unwrap();

    assert_eq!(std::fs::read(dest.join("ok.txt")).unwrap(), b"ok");
    assert!(!tmp.path().join("evil.txt").exists());
    assert!(!tmp.path().join("evil2.txt").exists());
    assert!(!outside.exists());
    // Whatever the crate does with the unsafe names, `dest` is the only thing written.
    let top_level: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(top_level, vec![std::ffi::OsString::from("dest")]);
}

#[test]
fn extract_zip_filter_selects_entries_and_errors_when_none_match() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dest");
    let data = build_zip(&[("keep.txt", b"keep"), ("skip.txt", b"skip")]);

    futures::executor::block_on(extract_zip(
        &data,
        &dest,
        Some(|name: &str| name == "keep.txt"),
    ))
    .unwrap();
    assert!(dest.join("keep.txt").exists());
    assert!(!dest.join("skip.txt").exists());

    let result = futures::executor::block_on(extract_zip(
        &data,
        &tmp.path().join("other"),
        Some(|name: &str| name == "missing"),
    ));
    assert!(result.is_err());
}
