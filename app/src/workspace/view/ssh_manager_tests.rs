use warpui::App;

use super::*;
use crate::test_util::settings::initialize_settings_for_tests;

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
