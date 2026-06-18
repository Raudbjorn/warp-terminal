#[cfg(windows)]
use super::WarpifySettings;

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
