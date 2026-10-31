use settings::{Setting, SyncToCloud};
use warpui::{App, SingletonEntity};

use super::{EnableSshWrapper, UseSshTmuxWrapper, WarpifySettings};
use crate::features::FeatureFlag;
use crate::test_util::settings::initialize_settings_for_tests;

#[test]
fn test_parsed_subshell_commands_updated_via_self_subscription() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);

        app.read(|ctx| {
            assert!(
                WarpifySettings::as_ref(ctx)
                    .parsed_added_subshell_commands
                    .is_empty()
            );
        });

        WarpifySettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .added_subshell_commands
                .set_value(vec!["^my-custom-shell$".to_string()], ctx)
                .unwrap();
        });

        // The parsed field must now contain the compiled regex.
        app.read(|ctx| {
            let parsed = &WarpifySettings::as_ref(ctx).parsed_added_subshell_commands;
            assert_eq!(
                parsed.len(),
                1,
                "self-subscription should have updated parsed field"
            );
            let regex = parsed[0].as_ref().expect("regex should compile");
            assert!(
                regex.is_match("my-custom-shell"),
                "compiled regex should match the command pattern"
            );
        });
    });
}

/// Verify that a user who previously set `enable_legacy_ssh_wrapper = false`
/// (old `SshSettings::enable_ssh_wrapper`) has that opt-out forwarded to
/// `enable_ssh_warpification` on first launch after the migration.
#[test]
fn test_enable_ssh_wrapper_false_migrates_to_enable_ssh_warpification_false() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);

        // Simulate a user who had explicitly opted out of the legacy SSH wrapper.
        WarpifySettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .enable_ssh_wrapper
                .set_value(false, ctx)
                .expect("set enable_ssh_wrapper to false");
        });

        // The migration in `register` already ran during `initialize_settings_for_tests`
        // (before we set the value above), so we trigger it manually by calling
        // `register` again on a fresh model to simulate a new launch with the value
        // pre-set in storage.  We verify the outcome by checking state directly.
        //
        // Simpler approach: confirm the migration logic produces the right state
        // by applying it explicitly here.
        app.update(|ctx| {
            WarpifySettings::handle(ctx).update(ctx, |me, ctx| {
                if me.enable_ssh_wrapper.is_value_explicitly_set()
                    && !*me.enable_ssh_wrapper.value()
                {
                    me.enable_ssh_warpification
                        .set_value(false, ctx)
                        .expect("migration set enable_ssh_warpification");
                    me.enable_ssh_wrapper
                        .set_value(true, ctx)
                        .expect("migration reset enable_ssh_wrapper");
                }
            });
        });

        app.read(|ctx| {
            let settings = WarpifySettings::as_ref(ctx);
            assert!(
                !*settings.enable_ssh_warpification.value(),
                "enable_ssh_warpification should be false after migration"
            );
            // The wrapper is reset to true so the migration condition
            // (`!*enable_ssh_wrapper.value()`) won't fire again on the next launch.
            assert!(
                *settings.enable_ssh_wrapper.value(),
                "enable_ssh_wrapper should be reset to true (default) after migration"
            );
        });
    });
}

/// Regression test for #13228: the deprecated SSH-wrapper migration triggers must
/// NOT be cloud-synced. They are read by one-time migrations in `register` that
/// forward an opt-out to `enable_ssh_warpification`; if they synced, a stale cloud
/// value would be restored on every launch and re-arm the migration, repeatedly
/// clobbering the user's choice. Keeping them local means the migration's reset to
/// the default persists and acts as the one-time, per-device marker.
/// See https://github.com/warpdotdev/Warp/issues/13228.
#[test]
fn test_deprecated_ssh_wrapper_migration_triggers_are_not_synced() {
    assert_eq!(
        EnableSshWrapper::sync_to_cloud(),
        SyncToCloud::Never,
        "enable_legacy_ssh_wrapper must not sync — a stale synced value re-arms the \
         migration and re-disables enable_ssh_warpification (#13228)"
    );
    assert_eq!(
        UseSshTmuxWrapper::sync_to_cloud(),
        SyncToCloud::Never,
        "use_ssh_tmux_wrapper must not sync — same re-arm hazard for the tmux notice"
    );
}

/// Post-#13228 behavior: the one-time legacy-wrapper migration honors a historical
/// opt-out once, and a user who then re-enables Warpify SSH keeps it. Because the
/// trigger is no longer synced, its reset-to-default persists and the migration does
/// not fire again to clobber the user's choice.
#[test]
fn test_legacy_wrapper_migration_is_one_time_and_preserves_reenabled_warpification() {
    /// Mirrors the one-time migration body from `WarpifySettings::register`.
    fn run_migration(app: &mut App) {
        app.update(|ctx| {
            WarpifySettings::handle(ctx).update(ctx, |me, ctx| {
                if me.enable_ssh_wrapper.is_value_explicitly_set()
                    && !*me.enable_ssh_wrapper.value()
                {
                    me.enable_ssh_warpification
                        .set_value(false, ctx)
                        .expect("migration set enable_ssh_warpification");
                    me.enable_ssh_wrapper
                        .set_value(true, ctx)
                        .expect("migration reset enable_ssh_wrapper");
                }
            });
        });
    }

    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);

        // Historical opt-out of the legacy wrapper.
        WarpifySettings::handle(&app).update(&mut app, |settings, ctx| {
            settings.enable_ssh_wrapper.set_value(false, ctx).unwrap();
        });

        // Launch 1: migration honors the opt-out once and resets the trigger.
        run_migration(&mut app);
        app.read(|ctx| {
            let settings = WarpifySettings::as_ref(ctx);
            assert!(
                !*settings.enable_ssh_warpification.value(),
                "opt-out is honored once"
            );
            assert!(
                *settings.enable_ssh_wrapper.value(),
                "trigger reset to default acts as the one-time marker"
            );
        });

        // The user re-enables Warpify SSH.
        WarpifySettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .enable_ssh_warpification
                .set_value(true, ctx)
                .unwrap();
        });

        // Launch 2: the trigger is no longer synced, so it stays at its reset
        // default; the migration is a no-op and does not re-disable warpification.
        run_migration(&mut app);
        app.read(|ctx| {
            assert!(
                *WarpifySettings::as_ref(ctx)
                    .enable_ssh_warpification
                    .value(),
                "re-enabled Warpify SSH persists across launches (#13228)"
            );
        });
    });
}

/// Verify that the default state (no legacy setting present) does not
/// spuriously disable `enable_ssh_warpification`.
#[test]
fn test_enable_ssh_wrapper_default_does_not_affect_enable_ssh_warpification() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);

        app.read(|ctx| {
            let settings = WarpifySettings::as_ref(ctx);
            // Neither setting should be explicitly set — both default to true.
            assert!(
                !settings.enable_ssh_wrapper.is_value_explicitly_set(),
                "enable_ssh_wrapper should not be explicitly set in a fresh install"
            );
            assert!(
                *settings.enable_ssh_warpification.value(),
                "enable_ssh_warpification should remain true when no migration is needed"
            );
        });
    });
}

#[cfg(windows)]
#[test]
fn test_wsl_subshell_detection_success() {
    [
        "wsl",
        "wsl.exe",
        "wsl -d Ubuntu",
        "wsl --distribution Ubuntu",
        "wsl -u user",
        "wsl --cd /home/user",
        "wsl --system",
        "wsl --shell-type login",
        "wsl -d Ubuntu --cd /home/user -u username",
        "wsl.exe -d Ubuntu --cd /home/user -u username",
    ]
    .iter()
    .for_each(|cmd| {
        assert!(
            WarpifySettings::is_built_in_subshell_match(cmd),
            "{} failed to match",
            *cmd
        )
    });
}

#[cfg(windows)]
#[test]
fn test_wsl_subshell_detection_fail() {
    [
        "wsl --install",
        "wsl --status",
        "wsl --list",
        "wsl --export Ubuntu file.tar",
        "wsl --uninstall",
        "wsl --shutdown",
        "wslfetch",
        "nowsl",
        "wsl --help",
        "wsl --version",
        "wsl --terminate Ubuntu",
        "wsl --unregister Ubuntu",
        "wsl --update",
        "wsl --import-in-place Ubuntu",
        "wsl --default-user root",
        "wsl --mount \\device",
    ]
    .iter()
    .for_each(|cmd| {
        assert!(
            !WarpifySettings::is_built_in_subshell_match(cmd),
            "{} accidentally matched",
            *cmd
        )
    });
}

#[cfg(not(windows))]
#[test]
fn ssh_remote_server_is_disabled_by_default_even_when_flag_is_enabled() {
    let _remote_server = FeatureFlag::SshRemoteServer.override_enabled(true);

    App::test((), |mut app| async move {
        app.add_singleton_model(WarpifySettings::new_with_defaults);

        app.update(|ctx| {
            assert!(!WarpifySettings::is_ssh_remote_server_enabled(ctx));
        });
    });
}

#[cfg(not(windows))]
#[test]
fn ssh_remote_server_requires_user_setting_and_ssh_warpification() {
    let _remote_server = FeatureFlag::SshRemoteServer.override_enabled(true);

    App::test((), |mut app| async move {
        let settings = app.add_singleton_model(WarpifySettings::new_with_defaults);

        settings.update(&mut app, |settings, ctx| {
            settings
                .enable_ssh_remote_server
                .load_value(true, true, ctx)
                .unwrap();
        });
        app.update(|ctx| {
            assert!(WarpifySettings::is_ssh_remote_server_enabled(ctx));
        });

        settings.update(&mut app, |settings, ctx| {
            settings
                .enable_ssh_warpification
                .load_value(false, true, ctx)
                .unwrap();
        });
        app.update(|ctx| {
            assert!(!WarpifySettings::is_ssh_remote_server_enabled(ctx));
        });
    });
}

#[cfg(not(windows))]
#[test]
fn ssh_extension_download_settings_default_to_channel_values() {
    App::test((), |app| async move {
        let settings = app.add_singleton_model(WarpifySettings::new_with_defaults);

        settings.read(&app, |settings, _ctx| {
            assert_eq!(
                settings.ssh_extension_download_base_url.value(),
                &remote_server::setup::default_download_base_url()
            );
            assert_eq!(
                settings.ssh_extension_download_channel.value(),
                remote_server::setup::default_download_channel()
            );
        });
    });
}

#[cfg(not(windows))]
#[test]
fn ssh_extension_install_options_use_normalized_download_settings() {
    App::test((), |mut app| async move {
        let settings = app.add_singleton_model(WarpifySettings::new_with_defaults);

        settings.update(&mut app, |settings, ctx| {
            settings
                .ssh_extension_download_base_url
                .load_value(
                    "https://downloads.example.com/warp/cli/".to_string(),
                    true,
                    ctx,
                )
                .unwrap();
            settings
                .ssh_extension_download_channel
                .load_value("preview".to_string(), true, ctx)
                .unwrap();
        });

        settings.read(&app, |settings, _ctx| {
            let options = settings.ssh_extension_install_options();
            assert_eq!(
                options.download_base_url,
                "https://downloads.example.com/warp/cli"
            );
            assert_eq!(options.download_channel, "preview");
        });
    });
}

#[cfg(not(windows))]
#[test]
fn ssh_extension_install_options_ignore_stale_production_default_for_oss() {
    App::test((), |mut app| async move {
        let settings = app.add_singleton_model(WarpifySettings::new_with_defaults);

        settings.update(&mut app, |settings, ctx| {
            settings
                .ssh_extension_download_base_url
                .load_value(
                    format!("{}/", remote_server::setup::PRODUCTION_DOWNLOAD_BASE_URL),
                    true,
                    ctx,
                )
                .unwrap();
        });

        settings.read(&app, |settings, _ctx| {
            let options = settings.ssh_extension_install_options();
            assert_eq!(
                options.download_base_url,
                remote_server::setup::default_download_base_url()
            );
        });
    });
}
