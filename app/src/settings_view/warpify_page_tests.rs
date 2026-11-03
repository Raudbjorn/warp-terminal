use settings::Setting;
use warpui::elements::Empty;
use warpui::platform::WindowStyle;
use warpui::{
    AddSingletonModel, App, AppContext, Element, Entity, SingletonEntity, TypedActionView, View,
    ViewHandle, WindowId,
};

use std::sync::Arc;

use super::WarpifyPageView;
use crate::appearance::Appearance;
use crate::auth::AuthStateProvider;
use crate::editor::{EditorView, Event as EditorEvent};
use crate::server::server_api::team::MockTeamClient;
use crate::server::server_api::workspace::MockWorkspaceClient;
use crate::settings_view::keybindings::KeybindingChangedNotifier;
use crate::terminal::warpify::settings::WarpifySettings;
use crate::test_util::settings::initialize_settings_for_tests;
use crate::vim_registers::VimRegisters;
use crate::workspace::ToastStack;
use crate::workspace::sync_inputs::SyncedInputState;
use crate::workspaces::user_workspaces::UserWorkspaces;

struct TestRootView;

impl Entity for TestRootView {
    type Event = ();
}

impl View for TestRootView {
    fn ui_name() -> &'static str {
        "TestRootView"
    }

    fn render(&self, _: &AppContext) -> Box<dyn Element> {
        Empty::new().finish()
    }
}

impl TypedActionView for TestRootView {
    type Action = ();
}

fn create_page(app: &mut App) -> (WindowId, ViewHandle<WarpifyPageView>) {
    // Same singletons the editor's own tests register; the page embeds editors.
    initialize_settings_for_tests(app);
    app.add_singleton_model(|_| Appearance::mock());
    app.add_singleton_model(|_| ToastStack);
    app.add_singleton_model(|_| SyncedInputState::mock());
    app.add_singleton_model(|_| VimRegisters::new());
    app.add_singleton_model(|_| KeybindingChangedNotifier::mock());
    app.add_singleton_model(|_| AuthStateProvider::new_for_test());
    #[cfg(feature = "voice_input")]
    app.add_singleton_model(voice_input::VoiceInput::new);
    let team_client = Arc::new(MockTeamClient::new());
    let workspace_client = Arc::new(MockWorkspaceClient::new());
    app.add_singleton_model(|ctx| UserWorkspaces::mock(team_client, workspace_client, vec![], ctx));
    let (window_id, _root) = app.add_window(WindowStyle::NotStealFocus, |_| TestRootView);
    let page = app.add_typed_action_view(window_id, WarpifyPageView::new);
    (window_id, page)
}

fn url_editor(page: &ViewHandle<WarpifyPageView>, app: &App) -> ViewHandle<EditorView> {
    app.read(|ctx| {
        page.as_ref(ctx)
            .ssh_extension_download_base_url_editor
            .as_ref(ctx)
            .editor()
            .clone()
    })
}

/// Types `text` into the URL input and presses Enter, as a user would.
fn submit(editor: &ViewHandle<EditorView>, text: &str, app: &mut App) {
    app.update(|ctx| {
        editor.update(ctx, |editor, ctx| {
            editor.set_buffer_text(text, ctx);
            ctx.emit(EditorEvent::Enter);
        });
    });
}

fn shown_text(editor: &ViewHandle<EditorView>, app: &App) -> String {
    app.read(|ctx| editor.as_ref(ctx).buffer_text(ctx))
}

fn stored_url(app: &App) -> String {
    app.read(|ctx| {
        WarpifySettings::as_ref(ctx)
            .ssh_extension_download_base_url
            .value()
            .clone()
    })
}

#[test]
fn download_url_input_shows_saved_value_after_submit() {
    App::test((), |mut app| async move {
        let (_window_id, page) = create_page(&mut app);
        let editor = url_editor(&page, &app);

        // The trailing slash is normalized away when the value is saved.
        submit(&editor, "https://downloads.example.com/ext/", &mut app);

        assert_eq!(stored_url(&app), "https://downloads.example.com/ext");
        assert_eq!(
            shown_text(&editor, &app),
            "https://downloads.example.com/ext"
        );
    });
}

#[test]
fn download_url_input_shows_value_after_resubmitting_unchanged_url() {
    App::test((), |mut app| async move {
        let (_window_id, page) = create_page(&mut app);
        let editor = url_editor(&page, &app);

        submit(&editor, "https://downloads.example.com/ext", &mut app);
        // Saving the same value again emits no settings change event.
        submit(&editor, "https://downloads.example.com/ext", &mut app);

        assert_eq!(
            shown_text(&editor, &app),
            "https://downloads.example.com/ext"
        );
    });
}
