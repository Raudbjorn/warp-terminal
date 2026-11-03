use std::path::Path;

use settings::Setting as _;
use warp_errors::report_if_error;
use warpui::{App, SingletonEntity};

use super::*;
use crate::test_util::settings::initialize_settings_for_tests;
use crate::workspace::tab_settings::TabSettings;

/// Lays out `<resources>/bundled/claude-plugin/harp`, with `hooks/hooks.json` when asked, and
/// returns the plugin dir.
fn make_plugin_dir(resources: &Path, with_hooks_manifest: bool) -> PathBuf {
    let plugin = resources.join("bundled").join("claude-plugin").join("harp");
    std::fs::create_dir_all(plugin.join("hooks")).expect("create plugin dir");
    if with_hooks_manifest {
        std::fs::write(plugin.join("hooks").join("hooks.json"), "{}").expect("write hooks.json");
    }
    plugin
}

fn set_harp_mode(app: &mut App, on: bool) {
    app.update(|ctx| {
        TabSettings::handle(ctx).update(ctx, |settings, ctx| {
            report_if_error!(settings.harp_mode.set_value(on, ctx));
        });
    });
}

#[test]
fn plugin_dir_in_needs_the_hooks_manifest() {
    let resources = tempfile::tempdir().expect("create temp dir");
    assert_eq!(plugin_dir_in(resources.path()), None);

    let plugin = make_plugin_dir(resources.path(), false);
    assert_eq!(plugin_dir_in(resources.path()), None);

    std::fs::write(plugin.join("hooks").join("hooks.json"), "{}").expect("write hooks.json");
    assert_eq!(plugin_dir_in(resources.path()), Some(plugin));
}

// Mode off (the default) injects nothing even when a complete plugin is bundled, and does not
// even look for the resources dir.
#[test]
fn claude_plugin_dir_is_none_with_harp_mode_off() {
    let resources = tempfile::tempdir().expect("create temp dir");
    make_plugin_dir(resources.path(), true);

    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);

        app.read(|ctx| {
            assert_eq!(
                claude_plugin_dir_from(ctx, || panic!("resources dir looked up with mode off")),
                None
            );
            assert_eq!(claude_plugin_dir(ctx), None);
        });
    });
}

#[test]
fn claude_plugin_dir_finds_the_bundled_plugin_with_harp_mode_on() {
    let resources = tempfile::tempdir().expect("create temp dir");
    let plugin = make_plugin_dir(resources.path(), true);
    let resources_dir = resources.path().to_path_buf();

    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        set_harp_mode(&mut app, true);

        app.read(|ctx| {
            assert_eq!(
                claude_plugin_dir_from(ctx, || Some(resources_dir)),
                Some(plugin)
            )
        });
    });
}

#[test]
fn claude_plugin_dir_is_none_without_hooks_manifest_or_resources_dir() {
    let resources = tempfile::tempdir().expect("create temp dir");
    make_plugin_dir(resources.path(), false);
    let resources_dir = resources.path().to_path_buf();

    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        set_harp_mode(&mut app, true);

        app.read(|ctx| {
            assert_eq!(claude_plugin_dir_from(ctx, || Some(resources_dir)), None);
            assert_eq!(claude_plugin_dir_from(ctx, || None), None);
        });
    });
}

#[cfg(unix)]
fn write_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    /// rwxr-xr-x
    const EXECUTABLE_MODE: u32 = 0o755;
    std::fs::write(path, "#!/bin/sh\n").expect("write file");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(EXECUTABLE_MODE))
        .expect("chmod");
}

#[cfg(unix)]
#[test]
fn missing_tools_checks_tmux_only_with_tmux_sync() {
    let bin = tempfile::tempdir().expect("create temp dir");
    let path_var = std::env::join_paths([bin.path()]).expect("join PATH entries");

    assert_eq!(missing_tools(&path_var, false), vec![JQ]);
    assert_eq!(missing_tools(&path_var, true), vec![JQ, TMUX]);

    // A non-executable file does not count as the tool.
    std::fs::write(bin.path().join(JQ), "").expect("write non-executable jq");
    assert_eq!(missing_tools(&path_var, false), vec![JQ]);

    write_executable(&bin.path().join(JQ));
    write_executable(&bin.path().join(TMUX));
    assert!(missing_tools(&path_var, true).is_empty());
}
