use chrono::Utc;

use super::*;
use crate::ssh_manager::usage::SshDestination;

fn profile(name: &str, host: &str) -> SshHostProfile {
    SshHostProfile::new(name.to_string(), host.to_string())
}

fn row_kinds(items: &[Item]) -> Vec<&'static str> {
    rows(items)
        .map(|row| match row {
            Row::QuickConnect { .. } => "quick",
            Row::Host { .. } => "host",
            Row::RecentQuickConnect { .. } => "recent-quick",
            Row::SaveAndConnect { .. } => "save",
            Row::ShowAll => "all",
        })
        .collect()
}

#[test]
fn destination_input_leads_with_quick_connect_and_offers_save() {
    let profiles = vec![profile("staging-api", "10.0.3.45")];
    let items = build_items("deploy@10.0.3.45:2222", &profiles, &SshHostUsage::default());
    assert_eq!(row_kinds(&items), ["quick", "host", "save"]);
    let actual = rows(&items).collect::<Vec<_>>();
    assert!(matches!(actual[0], Row::QuickConnect { args } if args == "deploy@10.0.3.45 -p 2222"));
    assert!(matches!(actual[1], Row::Host { profile, .. } if profile.id == profiles[0].id));
    assert!(
        matches!(actual[2], Row::SaveAndConnect { input, destination }
        if input == "deploy@10.0.3.45:2222" && destination.host == "10.0.3.45"
        && destination.user.as_deref() == Some("deploy") && destination.port == 2222)
    );
}

#[test]
fn plain_search_lists_matches_before_quick_connect() {
    let profiles = vec![
        profile("staging-api", "10.0.3.45"),
        profile("prod", "10.0.0.1"),
    ];
    let items = build_items("staging", &profiles, &SshHostUsage::default());
    assert_eq!(row_kinds(&items), ["host", "quick", "save"]);
    assert!(
        matches!(rows(&items).next(), Some(Row::Host { profile, .. }) if profile.id == profiles[0].id)
    );
}

#[test]
fn saved_destinations_are_not_offered_for_saving() {
    let mut saved = profile("box", "box");
    saved.host_is_explicit = false;
    saved.port_is_explicit = false;
    let profiles = vec![saved];
    let items = build_items("box", &profiles, &SshHostUsage::default());
    assert_eq!(row_kinds(&items), ["host", "quick"]);
}

#[test]
fn empty_input_shows_pinned_recent_quick_connects_and_all_hosts() {
    let pinned = profile("pinned", "a");
    let recent = profile("recent", "b");
    let other = profile("other", "c");
    let mut usage = SshHostUsage::default();
    usage.toggle_pinned(pinned.id);
    let now = Utc::now();
    usage.record_connection(SshDestination::Profile(recent.id), now);
    usage.record_connection(SshDestination::Profile(pinned.id), now);
    usage.record_connection(SshDestination::QuickConnect("root@x".to_string()), now);

    let items = build_items("", &[pinned, recent, other], &usage);
    assert_eq!(
        row_kinds(&items),
        ["host", "host", "recent-quick", "host", "all"]
    );
}

#[test]
fn editor_navigation_moves_launcher_selection_instead_of_editing_input() {
    use warpui::App;
    use warpui::platform::WindowStyle;

    use crate::editor::EditorAction;
    use crate::test_util::terminal::initialize_app_for_terminal_view;

    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let (_, launcher) = app.add_window(WindowStyle::NotStealFocus, SshLauncherView::new);
        launcher.update(&mut app, |view, ctx| {
            view.editor.update(ctx, |editor, ctx| {
                editor.set_buffer_text_ignoring_undo("root@host", ctx)
            });
        });
        launcher.update(&mut app, |view, ctx| {
            view.editor.update(ctx, |editor, ctx| {
                editor.handle_action(&EditorAction::Down, ctx)
            });
        });
        launcher.read(&app, |view, _| assert_eq!(view.selected, 1));
        launcher.update(&mut app, |view, ctx| {
            view.editor.update(ctx, |editor, ctx| {
                editor.handle_action(&EditorAction::Tab, ctx)
            });
        });
        launcher.read(&app, |view, ctx| {
            assert_eq!(view.selected, 0);
            assert_eq!(view.query(ctx), "root@host");
        });
    });
}

struct ActionCapture {
    launcher: ViewHandle<SshLauncherView>,
    actions: Vec<SshManagerAction>,
}

impl Entity for ActionCapture {
    type Event = ();
}
impl TypedActionView for ActionCapture {
    type Action = WorkspaceAction;
    fn handle_action(&mut self, action: &WorkspaceAction, _ctx: &mut ViewContext<Self>) {
        if let WorkspaceAction::SshManager(action) = action {
            self.actions.push(action.clone());
        }
    }
}
impl View for ActionCapture {
    fn ui_name() -> &'static str {
        "SshLauncherActionCapture"
    }
    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        warpui::elements::ChildView::new(&self.launcher).finish()
    }
}

#[test]
fn ctrl_enter_activates_save_and_connect() {
    use std::cell::Cell;
    use std::rc::Rc;

    use warpui::App;
    use warpui::platform::WindowStyle;

    use crate::test_util::terminal::initialize_app_for_terminal_view;

    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let (_, capture) = app.add_window(WindowStyle::NotStealFocus, |ctx| ActionCapture {
            launcher: ctx.add_typed_action_view(SshLauncherView::new),
            actions: Vec::new(),
        });
        let launcher = capture.read(&app, |view, _| view.launcher.clone());
        let closed = Rc::new(Cell::new(false));
        app.update(|ctx| {
            let closed = closed.clone();
            ctx.subscribe_to_view(&launcher, move |_, event, _| {
                if matches!(event, SshLauncherEvent::Close) {
                    closed.set(true);
                }
            });
        });
        launcher.update(&mut app, |view, ctx| {
            view.editor.update(ctx, |editor, ctx| {
                editor.set_buffer_text_ignoring_undo("root@host", ctx)
            });
        });
        launcher.update(&mut app, |view, ctx| {
            view.handle_editor_event(&EditorEvent::CtrlEnter, ctx)
        });
        assert!(closed.get());
        capture.read(&app, |view, _| {
            assert!(matches!(view.actions.as_slice(), [SshManagerAction::SaveHost { input, connect: true }] if input == "root@host"));
        });
    });
}

#[test]
fn explicitly_typed_default_port_is_not_confused_with_an_inherited_port() {
    let mut saved = profile("box", "box");
    saved.port_is_explicit = false;
    let items = build_items("box:22", &[saved], &SshHostUsage::default());
    assert!(matches!(rows(&items).next(), Some(Row::QuickConnect { args }) if args == "box -p 22"));
    assert!(rows(&items).any(
        |row| matches!(row, Row::SaveAndConnect { destination, .. } if destination.port_is_explicit)
    ));
}
