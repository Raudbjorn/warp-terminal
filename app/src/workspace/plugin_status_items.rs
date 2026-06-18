//! oh-my-warp: plugin-contributed status pills rendered in the tab bar.
//!
//! A plugin pushes status items via `warp.ui.setStatusItem(id, item)` — a tiny, themed pill that
//! lives next to the leader indicator and can be clicked to run a registered plugin command. Use
//! this for live status the user should glance at (a build state, a queue depth, an active session
//! count) rather than for transient notifications (those are `warp.ui.toast`).
//!
//! Like the prompt-segment model, this lives outside the feature-gated `plugin` module so the
//! render path never depends on the plugin host being built in: with no host the store is just
//! always empty. The actual `setStatusItem` plumbing (plugin app-request relay, JS API) is
//! `plugin_host`-only.

use std::collections::BTreeMap;

use pathfinder_color::ColorU;
use serde::{Deserialize, Serialize};
use warpui::elements::{
    Container, CornerRadius, CrossAxisAlignment, Element, Flex, Hoverable, MainAxisSize,
    MouseStateHandle, ParentElement, Radius, Text,
};
use warpui::fonts::Weight;
use warpui::platform::Cursor;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use crate::appearance::Appearance;
use crate::ui_components::blended_colors;
use crate::workspace::WorkspaceAction;

/// Severity-style flavor of a status pill. Maps to a theme color so the pill tracks the active
/// terminal theme without each plugin hard-coding hex.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusItemKind {
    #[default]
    Info,
    Success,
    Warn,
    Error,
    Accent,
}

/// One plugin-contributed pill in the tab bar.
///
/// `command_id`, when set, names a command registered via `warp.commands.register`; clicking the
/// pill dispatches `WorkspaceAction::RunPluginCommand(command_id)`, mirroring how `showPalette`
/// items work — the pill *triggers* a command, it doesn't re-enter the plugin host inline.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StatusItem {
    pub text: String,
    #[serde(default)]
    pub kind: StatusItemKind,
    #[serde(default)]
    pub tooltip: Option<String>,
    #[serde(default)]
    pub command_id: Option<String>,
}

/// Composite key for a status pill: `(plugin_id, item_id)`. We use a tuple rather than allocating
/// a String at every render so the hot path doesn't churn the allocator.
type PillKey = (String, String);

/// Singleton model storing each plugin's pills, keyed by `(plugin_id, item_id)`. BTreeMaps preserve
/// stable ordering across frames (sorted by plugin id, then by item id), so the pill positions
/// don't dance when a plugin updates one of them.
///
/// `mouse_states` holds a `MouseStateHandle` per clickable pill. The render path *must* reuse the
/// same handle across frames — `MouseStateHandle::default()` per render loses the GPUI hover/
/// click identity between frames, so the pill never registers hover or click. The map only carries
/// handles for pills with a `command_id`; passive (read-only) pills don't need one.
#[derive(Default)]
pub struct PluginStatusItemsModel {
    items: BTreeMap<String, BTreeMap<String, StatusItem>>,
    mouse_states: BTreeMap<PillKey, MouseStateHandle>,
}

impl PluginStatusItemsModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces (or removes, if `item` is `None`) the pill identified by `(plugin_id, item_id)`,
    /// then emits a change event so any view observing this model re-renders.
    ///
    /// For items with a `command_id` we also pre-create a `MouseStateHandle` so the render path
    /// can reuse the same handle across frames. Creating a fresh handle on every render breaks
    /// GPUI's hover/click identity, so the pill never registers interaction. We allocate the handle
    /// here (not lazily at render time) because the singleton model is borrowed immutably from
    /// `as_ref(app)` during render, and `BTreeMap::entry` needs `&mut self`.
    pub fn set(
        &mut self,
        plugin_id: String,
        item_id: String,
        item: Option<StatusItem>,
        ctx: &mut ModelContext<Self>,
    ) {
        let key = (plugin_id.clone(), item_id.clone());
        match item {
            Some(item) => {
                let command_id = item.command_id.clone();
                self.items
                    .entry(plugin_id)
                    .or_default()
                    .insert(item_id, item);
                if command_id.is_some() {
                    // Insert (or keep) a handle. The default handle is a no-op wrapper around an
                    // internal `Rc<RefCell<...>>` so the entry-or-default clone is cheap.
                    self.mouse_states.entry(key).or_default();
                } else {
                    // Passive pill — no hover/click state needed. Drop any stale handle from a
                    // prior clickable incarnation so the map doesn't leak.
                    self.mouse_states.remove(&key);
                }
            }
            None => {
                if let Some(plugin_items) = self.items.get_mut(&plugin_id) {
                    plugin_items.remove(&item_id);
                    if plugin_items.is_empty() {
                        self.items.remove(&plugin_id);
                    }
                }
                self.mouse_states.remove(&key);
            }
        }
        ctx.emit(PluginStatusItemsEvent::Changed);
        ctx.notify();
    }

    /// Returns the persistent `MouseStateHandle` for `(plugin_id, item_id)`, or `None` if the pill
    /// is passive (no `command_id`). The handle is `Clone` so the render path can hand a copy to
    /// the per-frame `Hoverable` element; the original stays in the model for the next frame.
    pub fn mouse_state_for(&self, plugin_id: &str, item_id: &str) -> Option<MouseStateHandle> {
        self.mouse_states
            .get(&(plugin_id.to_owned(), item_id.to_owned()))
            .cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.items.values().all(|m| m.is_empty())
    }
}

pub enum PluginStatusItemsEvent {
    Changed,
}

impl Entity for PluginStatusItemsModel {
    type Event = PluginStatusItemsEvent;
}

impl SingletonEntity for PluginStatusItemsModel {}

/// Maps a [`StatusItemKind`] to the ANSI theme color used to *tint* the pill's text. The background
/// stays neutral (matching the tab-bar surface) so the pill reads as a status indicator rather than
/// shouting like a toast — exactly the contrast we want for "glance at it" surfaces.
fn pill_color_for_kind(kind: StatusItemKind, theme: &crate::themes::theme::WarpTheme) -> ColorU {
    match kind {
        StatusItemKind::Info => theme.ansi_fg_blue(),
        StatusItemKind::Success => theme.ansi_fg_green(),
        StatusItemKind::Warn => theme.ansi_fg_yellow(),
        StatusItemKind::Error => theme.ansi_fg_red(),
        StatusItemKind::Accent => theme.accent_button_color().into_solid(),
    }
}

/// Renders all plugin status pills as a single row element, ready to be inserted into the tab bar
/// next to the leader indicator. Returns `None` when no plugin has set any pills, so the caller can
/// `if let Some(row) = render_plugin_status_items(...) { tab_bar.add_child(row); }` without
/// guarding against an empty row.
pub fn render_plugin_status_items(
    app: &AppContext,
    appearance: &Appearance,
) -> Option<Box<dyn Element>> {
    let model = PluginStatusItemsModel::as_ref(app);
    if model.is_empty() {
        return None;
    }
    let theme = appearance.theme();
    let mut row = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_size(MainAxisSize::Min);
    // Collect (plugin_id, item_id, item) tuples first so we can both iterate them and look up
    // the persistent mouse-state handle per clickable pill. The handle is created in `set` so
    // reusing the same instance across frames preserves GPUI's hover/click identity.
    let entries: Vec<(String, String, StatusItem)> = model
        .iter()
        .map(|(p, i, item)| (p.to_owned(), i.to_owned(), item.clone()))
        .collect();

    for (plugin_id, item_id, item) in entries {
        let color = pill_color_for_kind(item.kind, theme);
        let text = item.text.clone();
        let ui_font_family = appearance.ui_font_family();
        let pill = if let Some(command_id) = item.command_id.clone() {
            // Clickable pill: Hoverable owns the cursor + click handler. Hover swaps the background
            // to surface_2 so the pill picks up the same "this is interactive" feedback as native
            // tab-bar buttons. The click dispatches `WorkspaceAction::RunPluginCommand`, mirroring
            // how palette items and keybindings route — no special handling needed.
            let bg = blended_colors::neutral_1(theme);
            // `theme.surface_2()` returns a `Fill` (not `ColorU`), so the two paths use the
            // different setter overloads: `with_background_color` for the default neutral, and
            // `with_background` (Fill) for the hover state.
            let hover_bg = theme.surface_2();
            // Reuse the persistent handle. `set` guarantees one exists for any clickable pill, so
            // the `expect` is a sanity check, not a recoverable branch.
            let mouse_state = model
                .mouse_state_for(&plugin_id, &item_id)
                .expect("clickable pill must have a mouse-state handle allocated by set()");
            Hoverable::new(mouse_state, move |state| {
                let label = Text::new_inline(text.clone(), ui_font_family, 11.)
                    .with_color(color.into())
                    .with_style(warpui::fonts::Properties::default().weight(Weight::Semibold))
                    .with_selectable(false)
                    .finish();
                let mut container = Container::new(label)
                    .with_padding_left(8.)
                    .with_padding_right(8.)
                    .with_padding_top(2.)
                    .with_padding_bottom(2.)
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                    .with_margin_right(6.);
                if state.is_hovered() {
                    container = container.with_background(hover_bg);
                } else {
                    container = container.with_background_color(bg);
                }
                container.finish()
            })
            .with_cursor(Cursor::PointingHand)
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(WorkspaceAction::RunPluginCommand(command_id.clone()));
            })
            .finish()
        } else {
            // Non-clickable pill: same chrome, no hover/cursor (it's a read-only badge).
            let label = Text::new_inline(text, ui_font_family, 11.)
                .with_color(color.into())
                .with_style(warpui::fonts::Properties::default().weight(Weight::Semibold))
                .with_selectable(false)
                .finish();
            Container::new(label)
                .with_padding_left(8.)
                .with_padding_right(8.)
                .with_padding_top(2.)
                .with_padding_bottom(2.)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                .with_background_color(blended_colors::neutral_1(theme))
                .with_margin_right(6.)
                .finish()
        };
        row.add_child(pill);
    }
    Some(row.finish())
}
