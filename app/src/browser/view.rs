//! The backing GPUI view for the embedded browser pane (oh-my-warp).
//!
//! Owns a [`BrowserSession`] (a spawned Chrome + CDP screencast/input thread,
//! see [`super::session`]) and renders a toolbar (back / forward / reload + an
//! editable address bar) above the live viewport.
//!
//! Threading: session updates (frames + URL changes) arrive on an
//! `async_channel` drained on the foreground executor (`spawn_stream_local`);
//! input (clicks / scroll / navigation) is sent back over a command channel the
//! view owns, so UI closures can dispatch without holding the session. The UI
//! thread never blocks on the network or JPEG decode.
//!
//! Click mapping: [`FrameImage`] reports the exact (letterbox-corrected) rect it
//! drew into via a shared [`BoundsSink`]; the view maps a pane-local pointer
//! position into CSS viewport pixels (using the frame's `css_size`) for CDP
//! `Input.*`.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use instant::Instant;

use warpui::clipboard::ClipboardContent;
use warpui::elements::{
    Border, BoundsSink, ChildAnchor, ChildView, Clipped, ConstrainedBox, Container, CornerRadius,
    CrossAxisAlignment, DispatchEventResult, Empty, EventHandler, Expanded, Flex, FrameImage,
    MainAxisSize, MouseStateHandle, OffsetPositioning, ParentElement, PositionedElementAnchor,
    PositionedElementOffsetBounds, Radius, SavePosition, SizeSink, Stack,
};
use warpui::geometry::vector::{vec2f, Vector2F};
use warpui::image_cache::StaticImage;
use warpui::keymap::Keystroke;
use warpui::text_layout::ClipConfig;
use warpui::ui_components::components::UiComponent;
use warpui::{
    AppContext, BlurContext, Element, Entity, FocusContext, ModelHandle, SingletonEntity,
    TypedActionView, View, ViewContext, ViewHandle,
};

use super::session::{load_config, BrowserCommand, BrowserSession, SessionUpdate, DEFAULT_URL};
use crate::appearance::Appearance;
use crate::editor::{
    BaselinePositionComputationMethod, EditorView, Event as EditorEvent, SingleLineEditorOptions,
};
use crate::menu::{Event as MenuEvent, Menu, MenuItem, MenuItemFields};
use crate::pane_group::focus_state::PaneFocusHandle;
use crate::pane_group::pane::view::{self, HeaderContent, StandardHeader, StandardHeaderOptions};
use crate::pane_group::{BackingView, PaneConfiguration, PaneEvent};
use crate::ui_components::buttons::icon_button;
use crate::ui_components::icons;

/// Header text for the browser pane.
pub const BROWSER_HEADER_TEXT: &str = "Browser";

/// Multiplier applied to scroll-wheel deltas before forwarding to CDP. Tunable.
const SCROLL_SCALE: f64 = 1.0;

/// `SavePosition` id for the viewport cell. The right-click context menu anchors
/// to it (rather than to the pane/window) so it lands under the cursor no matter
/// where the pane sits in the window (e.g. a right-hand split).
const BROWSER_VIEWPORT_POSITION_ID: &str = "oh_my_warp_browser_viewport";

thread_local! {
    /// One-shot initial-URL override for the next [`BrowserView::new`], set by
    /// `warp.ui.openWebTab(url)` immediately before the pane is constructed (same
    /// UI-thread call stack, so it is consumed before any other pane is built).
    /// `None` falls back to the configured home page.
    static NEXT_BROWSER_URL: Cell<Option<String>> = const { Cell::new(None) };
}

/// Sets the initial URL for the *next* browser pane created (consumed once by
/// [`BrowserView::new`]). Backs the `warp.ui.openWebTab` plugin bridge; the input
/// is normalized like an address-bar entry (bare hosts gain `https://`, non-URLs
/// become a web search) via [`normalize_url`]. Also used by the persistence
/// restore path (`pane_group::mod`) to navigate restored panes to the URL
/// they were last viewing.
pub fn set_next_browser_url(url: String) {
    NEXT_BROWSER_URL.with(|cell| cell.set(Some(normalize_url(&url))));
}

impl BrowserView {
    /// The current page URL — read by `BrowserPane::snapshot` when persisting
    /// the pane so it can be restored to the same page across app restarts.
    pub fn current_url(&self) -> &str {
        &self.url
    }
}

/// The CSS viewport size the current frame represents (shared with input
/// closures so they can map pointer positions without borrowing the view).
type CssSize = Rc<Cell<Option<(f32, f32)>>>;

/// Which sub-target keyboard focus belongs to. Pane activation focuses whichever
/// is current (set synchronously on click), so the page doesn't steal focus back
/// from the address bar (and vice versa).
#[derive(Clone, Copy, PartialEq)]
enum FocusTarget {
    Page,
    AddressBar,
}

/// Event emitted by the [`BrowserView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserViewEvent {
    Pane(PaneEvent),
}

/// Actions handled by the [`BrowserView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserViewAction {
    /// Move keyboard focus to the page (so typing forwards to the browser, not
    /// the address bar). Dispatched when the viewport is clicked.
    FocusViewport,
    /// Open the page context menu at the given window-space position (x, y as
    /// integer logical px so the action stays `Eq`).
    OpenContextMenu {
        x: i32,
        y: i32,
    },
    /// Context-menu / navigation actions.
    Back,
    Forward,
    Reload,
    /// Copy the current page URL to the system clipboard.
    CopyUrl,
    /// Open the current page URL in the system default browser.
    OpenInSystemBrowser,
}

/// A pane view that renders a live, interactive browser viewport over CDP.
pub struct BrowserView {
    pane_configuration: ModelHandle<PaneConfiguration>,
    focus_handle: Option<PaneFocusHandle>,
    /// The most recent decoded frame, drawn by [`Self::render`].
    current_frame: Option<Arc<StaticImage>>,
    /// CSS viewport size for the current frame (for click mapping).
    css_size: CssSize,
    /// Window-space rect the frame was last drawn into (for click mapping).
    viewport_rect: BoundsSink,
    /// The viewport element's allocated size (logical px), for sizing Chrome.
    viewport_size: SizeSink,
    /// The last viewport size requested of Chrome (to avoid redundant resizes).
    requested_size: Rc<Cell<Option<(u32, u32)>>>,
    /// The current page URL (updated on navigation).
    url: String,
    /// The editable address bar.
    url_editor: ViewHandle<EditorView>,
    /// Outbound command channel to the session thread.
    cmd_tx: async_channel::Sender<BrowserCommand>,
    /// Scroll-wheel direction multiplier (-1.0 if `reverse_scroll` is set).
    scroll_sign: f64,
    /// Tracks recent clicks (last press time + count) to synthesize the CDP
    /// `clickCount` for double/triple-click (text selection) in the page.
    click_state: Rc<Cell<(Option<Instant>, i64)>>,
    /// Where keyboard focus should go when the pane is (re)activated.
    focus_target: Rc<Cell<FocusTarget>>,
    /// Whether the page view (`BrowserView` itself) currently holds focus. Driven
    /// by `on_focus`/`on_blur`; key-forwarding checks this so we stop forwarding
    /// the instant focus leaves (rather than relying on the sticky focus target).
    page_focused: Rc<Cell<bool>>,
    /// The page right-click context menu, shown at `show_right_click_menu`.
    right_click_menu: ViewHandle<Menu<BrowserViewAction>>,
    /// Viewport-relative offset to render the context menu at, when open. Anchored
    /// to the viewport `SavePosition`, so it tracks the pane's window position.
    show_right_click_menu: Option<Vector2F>,
    back_button: MouseStateHandle,
    forward_button: MouseStateHandle,
    reload_button: MouseStateHandle,
    /// Kept alive for the pane's lifetime; dropping it kills Chrome + the thread.
    _session: Option<BrowserSession>,
}

impl BrowserView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let pane_configuration = ctx.add_model(|_ctx| PaneConfiguration::new(BROWSER_HEADER_TEXT));
        let config = load_config();
        // A pending `warp.ui.openWebTab(url)` override wins over the configured home.
        let url = NEXT_BROWSER_URL
            .with(|cell| cell.take())
            .unwrap_or_else(|| config.home.clone());
        let scroll_sign = if config.reverse_scroll { -1.0 } else { 1.0 };

        // session updates out (frames + URL), commands in (clicks / nav).
        let (update_tx, update_rx) = async_channel::unbounded::<SessionUpdate>();
        let (cmd_tx, cmd_rx) = async_channel::unbounded::<BrowserCommand>();
        let session = match BrowserSession::spawn(&url, update_tx, cmd_rx) {
            Ok(session) => Some(session),
            Err(e) => {
                log::warn!("[omw-browser] failed to start browser session: {e:#}");
                None
            }
        };

        let url_editor = ctx.add_typed_action_view(|ctx| {
            EditorView::single_line(
                SingleLineEditorOptions {
                    select_all_on_focus: true,
                    // Center the text vertically using font metrics.
                    baseline_position_computation_method: BaselinePositionComputationMethod::Grid,
                    ..SingleLineEditorOptions::default()
                },
                ctx,
            )
        });
        url_editor.update(ctx, |editor, ctx| editor.set_buffer_text(&url, ctx));
        ctx.subscribe_to_view(
            &url_editor,
            |me: &mut Self, _editor, event, ctx| match event {
                EditorEvent::Enter => me.submit_url(ctx),
                // Keep the focus target in sync when the address bar gains focus by
                // any means (click, tab, etc.) so pane activation keeps it focused.
                EditorEvent::Focused => me.focus_target.set(FocusTarget::AddressBar),
                _ => {}
            },
        );

        // Page right-click context menu; hide it when it closes.
        let right_click_menu = ctx.add_typed_action_view(|_| Menu::<BrowserViewAction>::new());
        ctx.subscribe_to_view(&right_click_menu, |me: &mut Self, _, event, ctx| {
            if let MenuEvent::Close { .. } = event {
                me.show_right_click_menu = None;
                ctx.notify();
            }
        });

        ctx.spawn_stream_local(
            update_rx,
            |view: &mut Self, update, ctx| {
                match update {
                    SessionUpdate::Frame(frame) => {
                        view.current_frame = Some(frame.image);
                        view.css_size.set(Some((frame.css_width, frame.css_height)));
                    }
                    SessionUpdate::Url(url) => {
                        view.url = url.clone();
                        view.url_editor
                            .update(ctx, |editor, ctx| editor.set_buffer_text(&url, ctx));
                    }
                }
                ctx.notify();
            },
            |_view, _ctx| {},
        );

        Self {
            pane_configuration,
            focus_handle: None,
            current_frame: None,
            css_size: Rc::new(Cell::new(None)),
            viewport_rect: Rc::new(Cell::new(None)),
            viewport_size: Rc::new(Cell::new(None)),
            requested_size: Rc::new(Cell::new(None)),
            url,
            url_editor,
            cmd_tx,
            scroll_sign,
            click_state: Rc::new(Cell::new((None, 0))),
            focus_target: Rc::new(Cell::new(FocusTarget::AddressBar)),
            page_focused: Rc::new(Cell::new(false)),
            right_click_menu,
            show_right_click_menu: None,
            back_button: MouseStateHandle::default(),
            forward_button: MouseStateHandle::default(),
            reload_button: MouseStateHandle::default(),
            _session: session,
        }
    }

    pub fn pane_configuration(&self) -> ModelHandle<PaneConfiguration> {
        self.pane_configuration.clone()
    }

    pub fn focus(&mut self, ctx: &mut ViewContext<Self>) {
        // Focus whichever sub-target is current. The target is set synchronously
        // on click (page vs address bar), so pane activation doesn't steal focus
        // back to the page from the address bar.
        match self.focus_target.get() {
            FocusTarget::Page => ctx.focus_self(),
            FocusTarget::AddressBar => ctx.focus(&self.url_editor),
        }
    }

    /// Reads the address bar and navigates to it (called on Enter), then returns
    /// focus to the page.
    fn submit_url(&mut self, ctx: &mut ViewContext<Self>) {
        let input = self.url_editor.as_ref(ctx).buffer_text(ctx);
        let _ = self
            .cmd_tx
            .try_send(BrowserCommand::Navigate(normalize_url(&input)));
        self.focus_target.set(FocusTarget::Page);
        ctx.focus_self();
    }

    /// Populates and shows the page right-click context menu at `pos`.
    fn open_context_menu(&mut self, pos: Vector2F, ctx: &mut ViewContext<Self>) {
        let items = vec![
            MenuItemFields::new("Back")
                .with_on_select_action(BrowserViewAction::Back)
                .into_item(),
            MenuItemFields::new("Forward")
                .with_on_select_action(BrowserViewAction::Forward)
                .into_item(),
            MenuItemFields::new("Reload")
                .with_on_select_action(BrowserViewAction::Reload)
                .into_item(),
            MenuItem::Separator,
            MenuItemFields::new("Copy Page URL")
                .with_on_select_action(BrowserViewAction::CopyUrl)
                .into_item(),
            MenuItemFields::new("Open in System Browser")
                .with_on_select_action(BrowserViewAction::OpenInSystemBrowser)
                .into_item(),
        ];
        self.right_click_menu.update(ctx, |menu, view_ctx| {
            menu.set_items(items, view_ctx);
        });
        // `pos` is window-space; the menu anchors to the viewport's `SavePosition`,
        // so store it relative to the viewport's drawn origin. Fall back to the raw
        // position if the viewport hasn't reported its rect yet.
        let offset = match self.viewport_rect.get() {
            Some(rect) => pos - rect.origin(),
            None => pos,
        };
        self.show_right_click_menu = Some(offset);
        ctx.focus(&self.right_click_menu);
        ctx.notify();
    }

    /// A toolbar nav button that sends `command` when clicked.
    fn nav_button(
        &self,
        appearance: &Appearance,
        icon: icons::Icon,
        state: MouseStateHandle,
        command: BrowserCommand,
    ) -> Box<dyn Element> {
        let cmd_tx = self.cmd_tx.clone();
        let button = icon_button(appearance, icon, false, state)
            .build()
            .on_click(move |_ctx, _app, _pos| {
                let _ = cmd_tx.try_send(command.clone());
            })
            .finish();
        Container::new(button).with_margin_right(2.0).finish()
    }

    fn render_toolbar(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        // Accent border when the address bar is focused, neutral outline otherwise
        // (matches Warp's input chrome).
        let border_color = if self.url_editor.is_focused(app) {
            theme.accent()
        } else {
            theme.outline()
        };

        // Leading affordance: a lock for https pages, a globe otherwise.
        let lead_icon = if self.url.starts_with("https://") {
            icons::Icon::Lock
        } else {
            icons::Icon::Globe
        };
        let lead = ConstrainedBox::new(
            lead_icon
                .to_warpui_icon(theme.sub_text_color(theme.surface_2()))
                .finish(),
        )
        .with_width(14.0)
        .with_height(14.0)
        .finish();

        // Clicking the address bar must keep focus there: set the target
        // synchronously on mouse-down (before pane activation focuses the pane),
        // then propagate so the editor still focuses itself.
        let focus_ab = self.focus_target.clone();
        let editor_el = EventHandler::new(
            // Clip so a long URL scrolls within the box instead of overflowing.
            Clipped::new(ChildView::new(&self.url_editor).finish()).finish(),
        )
        .on_left_mouse_down(move |_ctx, _app, _pos| {
            focus_ab.set(FocusTarget::AddressBar);
            DispatchEventResult::PropagateToParent
        })
        .finish();
        let field = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(Container::new(lead).with_margin_right(6.0).finish())
            .with_child(Expanded::new(1.0, editor_el).finish());

        // The address bar: an editable single-line editor in a rounded, bordered box.
        let address = Container::new(field.finish())
            .with_background(theme.surface_2())
            .with_border(Border::all(1.0).with_border_fill(border_color))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.0)))
            .with_padding_left(8.0)
            .with_padding_right(8.0)
            .with_padding_top(5.0)
            // Slightly less bottom padding raises the bottom border ~1px so the
            // text reads centered.
            .with_padding_bottom(4.0)
            .with_margin_left(4.0)
            // Keep the right border off the toolbar edge (avoids a clipped edge).
            .with_margin_right(2.0)
            .finish();

        let row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(self.nav_button(
                appearance,
                icons::Icon::ArrowLeft,
                self.back_button.clone(),
                BrowserCommand::Back,
            ))
            .with_child(self.nav_button(
                appearance,
                icons::Icon::ArrowRight,
                self.forward_button.clone(),
                BrowserCommand::Forward,
            ))
            .with_child(self.nav_button(
                appearance,
                icons::Icon::Refresh,
                self.reload_button.clone(),
                BrowserCommand::Reload,
            ))
            .with_child(Expanded::new(1.0, address).finish());

        // Toolbar background distinguishes it from the page viewport below. It
        // self-sizes to the row + padding (no fixed height), so the address box
        // hugs its content and reads as a properly proportioned input.
        Container::new(row.finish())
            .with_background(theme.surface_1())
            .with_uniform_padding(6.0)
            .finish()
    }

    /// If the pane size changed since the last request, ask Chrome to resize its
    /// viewport to match so the page fills the pane (no letterboxing).
    fn sync_viewport_size(&self) {
        let Some(size) = self.viewport_size.get() else {
            return;
        };
        let (w, h) = (size.x().round() as u32, size.y().round() as u32);
        if w == 0 || h == 0 {
            return;
        }
        let changed = match self.requested_size.get() {
            Some((rw, rh)) => {
                (w as i32 - rw as i32).abs() >= 8 || (h as i32 - rh as i32).abs() >= 8
            }
            None => true,
        };
        if changed {
            self.requested_size.set(Some((w, h)));
            let _ = self.cmd_tx.try_send(BrowserCommand::Resize {
                width: w,
                height: h,
            });
        }
    }

    fn render_viewport(&self) -> Box<dyn Element> {
        let Some(frame) = &self.current_frame else {
            return Empty::new().finish();
        };
        let image = FrameImage::new(frame.clone())
            .report_bounds(self.viewport_rect.clone())
            .report_size(self.viewport_size.clone())
            .finish();

        let (vp_move, css_move, cmd_move) = self.input_handles();
        let (vp_down, css_down, cmd_down) = self.input_handles();
        let (vp_up, css_up, cmd_up) = self.input_handles();
        let css_wheel = self.css_size.clone();
        let cmd_wheel = self.cmd_tx.clone();
        let cmd_key = self.cmd_tx.clone();
        let scroll_scale = SCROLL_SCALE * self.scroll_sign;
        let click_down = self.click_state.clone();
        let click_up = self.click_state.clone();
        let focus_vp = self.focus_target.clone();
        let page_focused_key = self.page_focused.clone();

        EventHandler::new(image)
            // Forward cursor movement so the page shows hover feedback (link
            // highlights, pointer cursors). Coalesced session-side to avoid spam.
            .on_mouse_in(
                move |_ctx, _app, pos| {
                    if let Some((x, y)) = map_to_css(&vp_move, &css_move, pos) {
                        let _ = cmd_move.try_send(BrowserCommand::MouseMove { x, y });
                    }
                    DispatchEventResult::PropagateToParent
                },
                None,
            )
            .on_left_mouse_down(move |ctx, _app, pos| {
                // Clicking the page takes keyboard focus, so typing forwards here
                // rather than to the address bar. Set the target synchronously so
                // pane activation (below) focuses the page, not the address bar.
                focus_vp.set(FocusTarget::Page);
                ctx.dispatch_typed_action(BrowserViewAction::FocusViewport);
                if let Some((x, y)) = map_to_css(&vp_down, &css_down, pos) {
                    // Synthesize clickCount for double/triple-click selection.
                    let now = Instant::now();
                    let (last, last_count) = click_down.get();
                    let click_count = match last {
                        Some(t) if now.duration_since(t) < Duration::from_millis(400) => {
                            (last_count + 1).min(3)
                        }
                        _ => 1,
                    };
                    click_down.set((Some(now), click_count));
                    let _ = cmd_down.try_send(BrowserCommand::MouseDown { x, y, click_count });
                }
                DispatchEventResult::StopPropagation
            })
            .on_right_mouse_down(move |ctx, _app, pos| {
                // Open the page context menu at the cursor.
                ctx.dispatch_typed_action(BrowserViewAction::OpenContextMenu {
                    x: pos.x() as i32,
                    y: pos.y() as i32,
                });
                DispatchEventResult::StopPropagation
            })
            .on_left_mouse_up(move |_ctx, _app, pos| {
                if let Some((x, y)) = map_to_css(&vp_up, &css_up, pos) {
                    let click_count = click_up.get().1.max(1);
                    let _ = cmd_up.try_send(BrowserCommand::MouseUp { x, y, click_count });
                }
                DispatchEventResult::StopPropagation
            })
            .on_scroll_wheel(move |_ctx, _app, delta, _modifiers| {
                if let Some((css_w, css_h)) = css_wheel.get() {
                    let _ = cmd_wheel.try_send(BrowserCommand::Wheel {
                        x: (css_w / 2.0) as f64,
                        y: (css_h / 2.0) as f64,
                        delta_x: delta.x() as f64 * scroll_scale,
                        delta_y: delta.y() as f64 * scroll_scale,
                    });
                }
                DispatchEventResult::StopPropagation
            })
            // Forward typing to the page only while the page view actually holds
            // focus. This stops the instant focus leaves (e.g. clicking another
            // pane), and avoids forwarding the address-bar editor's keys (which
            // reach here because BrowserView is its responder-chain ancestor).
            // Shortcuts (cmd/ctrl) propagate to Warp.
            .on_keydown(move |_ctx, _app, keystroke| {
                if !page_focused_key.get() {
                    return DispatchEventResult::PropagateToParent;
                }
                match map_keystroke(keystroke) {
                    Some(cmd) => {
                        let _ = cmd_key.try_send(cmd);
                        DispatchEventResult::StopPropagation
                    }
                    None => DispatchEventResult::PropagateToParent,
                }
            })
            .finish()
    }

    /// Clones the shared handles a mouse closure needs.
    fn input_handles(&self) -> (BoundsSink, CssSize, async_channel::Sender<BrowserCommand>) {
        (
            self.viewport_rect.clone(),
            self.css_size.clone(),
            self.cmd_tx.clone(),
        )
    }
}

/// Maps a window-space pointer position into CSS viewport pixels using the last
/// drawn image rect + the frame's CSS size. Returns `None` if outside the image.
fn map_to_css(
    viewport_rect: &BoundsSink,
    css_size: &CssSize,
    global: Vector2F,
) -> Option<(f64, f64)> {
    let rect = viewport_rect.get()?;
    let (css_w, css_h) = css_size.get()?;
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let local_x = global.x() - rect.origin().x();
    let local_y = global.y() - rect.origin().y();
    if local_x < 0.0 || local_y < 0.0 || local_x > rect.width() || local_y > rect.height() {
        return None;
    }
    Some((
        (local_x / rect.width() * css_w) as f64,
        (local_y / rect.height() * css_h) as f64,
    ))
}

/// Maps a Warp keystroke to a page-input command, or `None` to let Warp handle
/// it (modifier shortcuts and unmapped keys propagate instead of forwarding).
fn map_keystroke(ks: &Keystroke) -> Option<BrowserCommand> {
    if ks.cmd || ks.ctrl || ks.meta {
        return None;
    }
    let press = |key: &str, code: &str, vk: i64| {
        Some(BrowserCommand::KeyPress {
            key: key.to_owned(),
            code: code.to_owned(),
            vk,
        })
    };
    match ks.key.as_str() {
        "enter" => press("Enter", "Enter", 13),
        "backspace" => press("Backspace", "Backspace", 8),
        "tab" => press("Tab", "Tab", 9),
        "delete" => press("Delete", "Delete", 46),
        "escape" => press("Escape", "Escape", 27),
        "left" => press("ArrowLeft", "ArrowLeft", 37),
        "up" => press("ArrowUp", "ArrowUp", 38),
        "right" => press("ArrowRight", "ArrowRight", 39),
        "down" => press("ArrowDown", "ArrowDown", 40),
        "space" => Some(BrowserCommand::InsertText(" ".to_owned())),
        key if key.chars().count() == 1 => {
            let text = if ks.shift {
                key.to_uppercase()
            } else {
                key.to_owned()
            };
            Some(BrowserCommand::InsertText(text))
        }
        _ => None,
    }
}

/// Normalizes address-bar input into a URL: passes through explicit schemes,
/// prefixes `https://` for bare domains, else treats it as a web search.
fn normalize_url(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return DEFAULT_URL.to_owned();
    }
    if s.contains("://") {
        return s.to_owned();
    }
    let host = s.split('/').next().unwrap_or(s);
    if host.contains('.') && !host.contains(' ') {
        format!("https://{s}")
    } else {
        format!("https://www.google.com/search?q={}", s.replace(' ', "+"))
    }
}

/// Opens `url` in the system default browser (macOS `open`).
fn open_in_system_browser(url: &str) {
    if let Err(e) = std::process::Command::new("open").arg(url).spawn() {
        log::warn!("[omw-browser] failed to open {url} in system browser: {e}");
    }
}

impl Entity for BrowserView {
    type Event = BrowserViewEvent;
}

impl View for BrowserView {
    fn ui_name() -> &'static str {
        "BrowserView"
    }

    fn on_focus(&mut self, _focus_ctx: &FocusContext, _ctx: &mut ViewContext<Self>) {
        self.page_focused.set(true);
    }

    fn on_blur(&mut self, _blur_ctx: &BlurContext, _ctx: &mut ViewContext<Self>) {
        // Stop forwarding keys the instant the page view loses focus.
        self.page_focused.set(false);
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        // Keep Chrome's viewport sized to the pane (from the last layout).
        self.sync_viewport_size();
        let viewport =
            SavePosition::new(self.render_viewport(), BROWSER_VIEWPORT_POSITION_ID).finish();
        let content = Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_child(self.render_toolbar(app))
            .with_child(Expanded::new(1.0, viewport).finish())
            .finish();

        let mut stack = Stack::new();
        stack.add_child(content);
        if let Some(offset) = self.show_right_click_menu {
            stack.add_positioned_overlay_child(
                ChildView::new(&self.right_click_menu).finish(),
                OffsetPositioning::offset_from_save_position_element(
                    BROWSER_VIEWPORT_POSITION_ID,
                    offset,
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::TopLeft,
                    ChildAnchor::TopLeft,
                ),
            );
        }
        stack.finish()
    }
}

impl TypedActionView for BrowserView {
    type Action = BrowserViewAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            BrowserViewAction::FocusViewport => {
                // Focus the page for keyboard input, and route through the pane
                // focus system so the workspace's focused-pane state stays in
                // sync (otherwise focus doesn't cleanly transfer to other panes).
                self.focus_target.set(FocusTarget::Page);
                ctx.focus_self();
                ctx.emit(BrowserViewEvent::Pane(PaneEvent::FocusSelf));
            }
            BrowserViewAction::OpenContextMenu { x, y } => {
                self.open_context_menu(vec2f(*x as f32, *y as f32), ctx);
            }
            BrowserViewAction::Back => {
                let _ = self.cmd_tx.try_send(BrowserCommand::Back);
            }
            BrowserViewAction::Forward => {
                let _ = self.cmd_tx.try_send(BrowserCommand::Forward);
            }
            BrowserViewAction::Reload => {
                let _ = self.cmd_tx.try_send(BrowserCommand::Reload);
            }
            BrowserViewAction::CopyUrl => {
                ctx.clipboard()
                    .write(ClipboardContent::plain_text(self.url.clone()));
            }
            BrowserViewAction::OpenInSystemBrowser => {
                open_in_system_browser(&self.url);
            }
        }
    }
}

impl BackingView for BrowserView {
    type PaneHeaderOverflowMenuAction = BrowserViewAction;
    type CustomAction = BrowserViewAction;
    type AssociatedData = ();

    fn handle_pane_header_overflow_menu_action(
        &mut self,
        _action: &Self::PaneHeaderOverflowMenuAction,
        _ctx: &mut ViewContext<Self>,
    ) {
        // No overflow menu items are registered.
    }

    fn close(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.emit(BrowserViewEvent::Pane(PaneEvent::Close));
    }

    fn focus_contents(&mut self, ctx: &mut ViewContext<Self>) {
        self.focus(ctx);
    }

    fn render_header_content(
        &self,
        _ctx: &view::HeaderRenderContext<'_>,
        _app: &AppContext,
    ) -> HeaderContent {
        HeaderContent::Standard(StandardHeader {
            title: BROWSER_HEADER_TEXT.to_owned(),
            title_secondary: None,
            title_style: None,
            title_clip_config: ClipConfig::start(),
            title_max_width: None,
            left_of_title: None,
            right_of_title: None,
            left_of_overflow: None,
            options: StandardHeaderOptions::default(),
        })
    }

    fn set_focus_handle(&mut self, focus_handle: PaneFocusHandle, _ctx: &mut ViewContext<Self>) {
        self.focus_handle = Some(focus_handle);
    }
}
