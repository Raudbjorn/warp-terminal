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

/// Singleton model storing each plugin's pills, keyed by `(plugin_id, item_id)`. BTreeMaps preserve
/// stable ordering across frames (sorted by plugin id, then by item id), so the pill positions
/// don't dance when a plugin updates one of them.
#[derive(Default)]
pub struct PluginStatusItemsModel {
    items: BTreeMap<String, BTreeMap<String, StatusItem>>,
}

impl PluginStatusItemsModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces (or removes, if `item` is `None`) the pill identified by `(plugin_id, item_id)`,
    /// then emits a change event so any view observing this model re-renders.
    pub fn set(
        &mut self,
        plugin_id: String,
        item_id: String,
        item: Option<StatusItem>,
        ctx: &mut ModelContext<Self>,
    ) {
        match item {
            Some(item) => {
                self.items
                    .entry(plugin_id)
                    .or_default()
                    .insert(item_id, item);
            }
            None => {
                if let Some(plugin_items) = self.items.get_mut(&plugin_id) {
                    plugin_items.remove(&item_id);
                    if plugin_items.is_empty() {
                        self.items.remove(&plugin_id);
                    }
                }
            }
        }
        ctx.emit(PluginStatusItemsEvent::Changed);
        ctx.notify();
    }

    /// All pills, in stable render order. Pairs each item with the owning `(plugin_id, item_id)` so
    /// the render path can build per-pill mouse-state handles keyed by identity.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str, &StatusItem)> {
        self.items.iter().flat_map(|(plugin_id, items)| {
            items
                .iter()
                .map(move |(item_id, item)| (plugin_id.as_str(), item_id.as_str(), item))
        })
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

    for (_plugin_id, _item_id, item) in model.iter() {
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
            Hoverable::new(MouseStateHandle::default(), move |state| {
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
