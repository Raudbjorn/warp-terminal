use warpui::App;

use super::*;
use crate::test_util::settings::initialize_settings_for_tests;

#[test]
fn workspace_actions_toggle_visibility_and_open_focuses_the_panel() {
    App::test((), |mut app| async move {
        super::super::tests::initialize_app(&mut app);
        app.update(|ctx| {
            SshOpenConnections::handle_or_init(ctx);
        });
        let workspace = super::super::tests::mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            assert!(!workspace.ssh_manager.panel_open);
            workspace.handle_ssh_manager_action(&SshManagerAction::TogglePanel, ctx);
            assert!(workspace.ssh_manager.panel_open);
            workspace.handle_ssh_manager_action(&SshManagerAction::TogglePanel, ctx);
            assert!(!workspace.ssh_manager.panel_open);
            workspace.handle_ssh_manager_action(&SshManagerAction::OpenPanel, ctx);
            assert!(workspace.ssh_manager.panel_open);
        });
        workspace.update(&mut app, |workspace, ctx| {
            assert!(workspace.ssh_profiles_panel_is_focused(ctx));
            // Opening an already open panel must retain visibility and focus.
            workspace.handle_ssh_manager_action(&SshManagerAction::OpenPanel, ctx);
            assert!(workspace.ssh_manager.panel_open);
            assert!(workspace.ssh_profiles_panel_is_focused(ctx));
        });
    });
}

#[test]
fn remote_restore_pairs_visible_nested_panes_with_their_remote_directories() {
    App::test((), |mut app| async move {
        super::super::tests::initialize_app(&mut app);
        app.update(|ctx| {
            SshOpenConnections::handle_or_init(ctx);
        });
        let workspace = super::super::tests::mock_workspace(&mut app);
        let sessions = remote_sessions::parse_remote_output(concat!(
            "/db/warp.sqlite\n[",
            "{\"id\":1,\"tab_id\":1,\"is_leaf\":0,\"horizontal\":1},",
            "{\"id\":2,\"tab_id\":1,\"parent\":1,\"is_leaf\":1,\"kind\":\"terminal\",\"cwd\":\"/left\"},",
            "{\"id\":3,\"tab_id\":1,\"parent\":1,\"is_leaf\":0,\"horizontal\":0},",
            "{\"id\":4,\"tab_id\":1,\"parent\":3,\"is_leaf\":1,\"kind\":\"terminal\",\"cwd\":\"/top\"},",
            "{\"id\":5,\"tab_id\":1,\"parent\":3,\"is_leaf\":1,\"kind\":\"terminal\",\"cwd\":\"/bottom\"}]"
        )).unwrap();
        workspace.update(&mut app, |workspace, ctx| {
            let profile = SshHostProfile::new("restore-test".into(), "example.invalid".into());
            workspace.open_remote_ssh_tabs(&profile, sessions, ctx);
            let panes = workspace.active_tab_pane_group().as_ref(ctx);
            let directories = panes
                .visible_pane_ids()
                .into_iter()
                .map(|pane_id| {
                    panes
                        .terminal_view_from_pane_id(pane_id, ctx)
                        .unwrap()
                        .as_ref(ctx)
                        .ssh_startup_directory()
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                directories,
                vec![
                    Some("/left".into()),
                    Some("/top".into()),
                    Some("/bottom".into())
                ]
            );
        });
    });
}

#[test]
fn opening_ssh_panel_adds_missing_toolbar_item_without_reordering_existing_items() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.update(|ctx| {
            let left = vec![HeaderToolbarItemKind::ToolsPanel];
            let right = vec![HeaderToolbarItemKind::CodeReview];
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                settings
                    .header_toolbar_chip_selection
                    .set_value(
                        HeaderToolbarChipSelection::Custom {
                            left: left.clone(),
                            right: right.clone(),
                        },
                        ctx,
                    )
                    .unwrap();
            });
            Workspace::ensure_ssh_panel_in_config(ctx).unwrap();
            Workspace::ensure_ssh_panel_in_config(ctx).unwrap();
            let config = &TabSettings::as_ref(ctx).header_toolbar_chip_selection;
            assert_eq!(
                config.left_items(),
                vec![
                    HeaderToolbarItemKind::ToolsPanel,
                    HeaderToolbarItemKind::SshProfiles
                ]
            );
            assert_eq!(config.right_items(), right);
        });
    });
}

#[test]
fn opening_ssh_panel_preserves_right_side_placement() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.update(|ctx| {
            let selection = HeaderToolbarChipSelection::Custom {
                left: vec![HeaderToolbarItemKind::ToolsPanel],
                right: vec![HeaderToolbarItemKind::SshProfiles],
            };
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                settings
                    .header_toolbar_chip_selection
                    .set_value(selection.clone(), ctx)
                    .unwrap();
            });
            Workspace::ensure_ssh_panel_in_config(ctx).unwrap();
            assert_eq!(
                TabSettings::as_ref(ctx)
                    .header_toolbar_chip_selection
                    .value(),
                &selection
            );
        });
    });
}

#[test]
fn toolbar_persistence_errors_are_returned_without_changing_selection() {
    use warpui_extras::user_preferences::{Error, UserPreferences};
    struct ReadOnlyPreferences;
    impl UserPreferences for ReadOnlyPreferences {
        fn write_value(&self, _: &str, _: String) -> Result<(), Error> {
            Err(
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, "read-only settings")
                    .into(),
            )
        }
        fn read_value(&self, _: &str) -> Result<Option<String>, Error> {
            Ok(None)
        }
        fn remove_value(&self, _: &str) -> Result<(), Error> {
            Ok(())
        }
    }
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.update(|ctx| {
            let selection = HeaderToolbarChipSelection::Custom {
                left: vec![],
                right: vec![],
            };
            TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                settings
                    .header_toolbar_chip_selection
                    .set_value(selection.clone(), ctx)
                    .unwrap();
            });
            settings::PublicPreferences::handle(ctx).update(ctx, |preferences, _| {
                *preferences = settings::PublicPreferences::new(Box::new(ReadOnlyPreferences));
            });
            assert!(Workspace::ensure_ssh_panel_in_config(ctx).is_err());
            assert_eq!(
                TabSettings::as_ref(ctx)
                    .header_toolbar_chip_selection
                    .value(),
                &selection
            );
        });
    });
}
