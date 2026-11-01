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
            Workspace::ensure_ssh_panel_in_config(ctx);
            Workspace::ensure_ssh_panel_in_config(ctx);
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
            Workspace::ensure_ssh_panel_in_config(ctx);
            assert_eq!(
                TabSettings::as_ref(ctx)
                    .header_toolbar_chip_selection
                    .value(),
                &selection
            );
        });
    });
}
