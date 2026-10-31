use std::fs;

use super::*;

/// Every permission `js_api::warp` checks.
const ALL_PERMISSIONS: [&str; 8] = [
    "commands",
    "terminal:events",
    "ui",
    "ai",
    "fs:read",
    "fs:write",
    "process",
    "network",
];

/// A plugin directory with a `main.js` and, if given, a `plugin.json` holding `manifest`.
fn plugin_dir(manifest: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("main.js"), "export function activate() {}").unwrap();
    if let Some(text) = manifest {
        fs::write(dir.path().join(PLUGIN_MANIFEST_FILE_NAME), text).unwrap();
    }
    dir
}

fn assert_no_permissions(manifest: &Manifest) {
    for permission in ALL_PERMISSIONS {
        assert!(!manifest.permits(permission), "`{permission}` was granted");
    }
}

#[test]
fn plugin_without_a_manifest_has_no_permissions() {
    let dir = plugin_dir(None);

    let manifest = Manifest::load(dir.path());

    assert_no_permissions(&manifest);
    // It still loads: an absent engine range accepts any API version.
    assert_eq!(manifest.check_engine_compatibility(), Ok(()));
}

#[test]
fn malformed_manifest_grants_nothing() {
    for text in [
        "{ not json",
        "",
        // Valid JSON of the wrong shape must not fall back to anything broader either.
        r#"{ "permissions": "all" }"#,
        r#"{ "permissions": [1, 2] }"#,
    ] {
        let dir = plugin_dir(Some(text));

        assert_no_permissions(&Manifest::load(dir.path()));
    }
}

#[test]
fn manifest_grants_exactly_the_permissions_it_declares() {
    let dir = plugin_dir(Some(r#"{ "permissions": ["ui", "fs:read"] }"#));

    let manifest = Manifest::load(dir.path());

    assert!(manifest.permits("ui"));
    assert!(manifest.permits("fs:read"));
    // `fs:read` does not imply `fs:write`, and a prefix of a permission is not the permission.
    for permission in ["fs:write", "fs", "commands", "process", "network"] {
        assert!(!manifest.permits(permission), "`{permission}` was granted");
    }
}

#[test]
fn effective_id_prefers_the_manifest_then_the_directory_name() {
    let named = plugin_dir(Some(r#"{ "id": "my-plugin" }"#));
    assert_eq!(
        Manifest::load(named.path()).effective_id(named.path()),
        "my-plugin"
    );

    let bare = plugin_dir(None);
    let dir_name = bare.path().file_name().unwrap().to_str().unwrap();
    assert_eq!(
        Manifest::load(bare.path()).effective_id(bare.path()),
        dir_name
    );
}

#[test]
fn engine_range_is_checked_against_the_api_version() {
    let manifest = |range: &str| Manifest {
        engines: Engines {
            warp: Some(range.to_string()),
        },
        ..Default::default()
    };

    assert_eq!(manifest("^1.0").check_engine_compatibility(), Ok(()));
    assert_eq!(manifest("*").check_engine_compatibility(), Ok(()));
    assert!(manifest("^2.0").check_engine_compatibility().is_err());
    assert!(
        manifest("not a range")
            .check_engine_compatibility()
            .is_err()
    );
}
