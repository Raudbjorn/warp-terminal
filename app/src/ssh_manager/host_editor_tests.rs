use warpui::App;
use warpui::platform::WindowStyle;

use super::*;
use crate::editor::EditorAction;
use crate::test_util::terminal::initialize_app_for_terminal_view;

#[test]
fn modal_tab_navigation_moves_focus_without_inserting_spaces() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let (_, modal) = app.add_window(WindowStyle::NotStealFocus, SshProfileModalView::new);
        modal.update(&mut app, |view, ctx| {
            view.open_add(vec![], ctx);
            view.on_open(ctx);
            view.name_editor.update(ctx, |editor, ctx| {
                editor.handle_action(&EditorAction::Tab, ctx)
            });
        });
        modal.read(&app, |view, ctx| {
            assert!(view.host_editor.is_focused(ctx));
            assert_eq!(view.name_editor.as_ref(ctx).buffer_text(ctx), "");
        });
    });
}

#[test]
fn editing_a_profile_preserves_an_inherited_port_until_explicitly_set() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let (_, modal) = app.add_window(WindowStyle::NotStealFocus, SshProfileModalView::new);
        let mut profile = SshHostProfile::new("work".into(), "host".into());
        profile.port_is_explicit = false;
        modal.update(&mut app, |view, ctx| {
            view.open_edit(profile, vec![], ctx);
            assert_eq!(view.port_editor.as_ref(ctx).buffer_text(ctx), "");
            assert!(!view.build_profile(ctx).unwrap().port_is_explicit);
            view.port_editor.update(ctx, |editor, ctx| {
                editor.set_buffer_text_ignoring_undo("22", ctx)
            });
            assert!(view.build_profile(ctx).unwrap().port_is_explicit);
        });
    });
}
