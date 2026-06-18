//! oh-my-warp: plugin-contributed prompt segments.
//!
//! A plugin pushes segments via `warp.prompt.set([...])` (see the `prompt` namespace in the plugin
//! host's `js_api`). Because the plugin host can't render synchronously on the UI thread, the model
//! is **push**: the plugin computes segment text in JS and pushes it; this singleton model caches it
//! keyed by plugin id, and [`crate::context_chips::display::PromptDisplay`] reads it each render and
//! appends the segments as native chips. Each `set` replaces that plugin's segments (empty clears).
//!
//! This lives in `context_chips` (always compiled) rather than the feature-gated `plugin` module so
//! the render path never depends on the plugin host being built in: with no plugin host the model is
//! simply always empty.

use std::collections::BTreeMap;

use pathfinder_color::ColorU;
use serde::{Deserialize, Serialize};
use warpui::elements::Element;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use super::display_chip::{render_udi_chip, UdiChipConfig};
use crate::appearance::Appearance;

/// Which side of the prompt a segment is grouped on. Left segments render first (after the built-in
/// chips), then right segments. (True right-edge alignment is classic-mode only; see PLUGIN_SPEC.md.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptSide {
    #[default]
    Left,
    Right,
}

/// Semantic flavor of a plugin-contributed prompt segment. Maps to a theme color in
/// [`render_plugin_chips_for_side`] so plugins can signal status (e.g. CI green / test red) without
/// hard-coding theme tokens — `info` is the conservative default and matches the original chip blue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptKind {
    #[default]
    Info,
    Success,
    Warn,
    Error,
    Accent,
}

/// One plugin-contributed prompt segment: the text to show, which side to group it on, an optional
/// tooltip, an optional `kind` driving the chip color, and an optional `icon` sigil rendered as a
/// short text prefix on the chip (`✓ tests` etc.). Missing `kind` / `icon` keep the original look,
/// so plugins from before this field existed render unchanged.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptSegment {
    pub text: String,
    #[serde(default)]
    pub side: PromptSide,
    #[serde(default)]
    pub tooltip: Option<String>,
    #[serde(default)]
    pub kind: PromptKind,
    #[serde(default)]
    pub icon: Option<String>,
}

/// Singleton model holding the prompt segments each plugin has pushed. Keyed by plugin id (BTreeMap
/// so render order is stable across frames). Updated by the workspace plugin-host handler; observed
/// by every `PromptDisplay` so a push re-renders the prompt.
#[derive(Default)]
pub struct PluginPromptModel {
    segments_by_plugin: BTreeMap<String, Vec<PromptSegment>>,
}

impl PluginPromptModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces `plugin_id`'s segments (an empty list removes them) and notifies observers.
    pub fn set(
        &mut self,
        plugin_id: String,
        segments: Vec<PromptSegment>,
        ctx: &mut ModelContext<Self>,
    ) {
        if segments.is_empty() {
            self.segments_by_plugin.remove(&plugin_id);
        } else {
            self.segments_by_plugin.insert(plugin_id, segments);
        }
        ctx.emit(PluginPromptEvent::Changed);
        ctx.notify();
    }

    /// All segments for one side, across all plugins (plugin-id order, deterministic).
    pub fn segments_for_side(&self, side: PromptSide) -> impl Iterator<Item = &PromptSegment> {
        self.segments_by_plugin
            .values()
            .flatten()
            .filter(move |s| s.side == side)
    }

    pub fn is_empty(&self) -> bool {
        self.segments_by_plugin.is_empty()
    }
}

/// Returns the chip color for a [`PromptKind`]. Picks ANSI theme colors so the chips track the
/// active terminal theme (light/dark/Solarized/…) without each plugin needing to know about it.
fn chip_color_for_kind(kind: PromptKind, theme: &crate::themes::theme::WarpTheme) -> ColorU {
    match kind {
        PromptKind::Info => theme.ansi_fg_blue(),
        PromptKind::Success => theme.ansi_fg_green(),
        PromptKind::Warn => theme.ansi_fg_yellow(),
        PromptKind::Error => theme.ansi_fg_red(),
        PromptKind::Accent => theme.accent_button_color().into_solid(),
    }
}

/// Renders the plugin-pushed segments for one side as native chip elements. Returns an empty `Vec`
/// when no plugin has pushed segments for that side; callers can `for elem in … { row.add_child(elem) }`
/// without any guarding. Used by `PromptDisplay::render` (terminal prompt) and the agent input
/// footer (`agent_view::agent_input_footer`) so plugin chips appear consistently in both surfaces.
///
/// Each segment's `kind` picks the chip color via [`chip_color_for_kind`]; `icon`, when set, is
/// rendered as a short text prefix (the UdiChip's `Icon` field is reserved for the bundled `Icon`
/// enum, so plugins use a sigil/emoji string instead).
pub fn render_plugin_chips_for_side(
    side: PromptSide,
    app: &AppContext,
    appearance: &Appearance,
) -> Vec<Box<dyn Element>> {
    let model = PluginPromptModel::as_ref(app);
    if model.is_empty() {
        return Vec::new();
    }
    let theme = appearance.theme();
    model
        .segments_for_side(side)
        .map(|seg| {
            let color = chip_color_for_kind(seg.kind, theme);
            let text = match seg.icon.as_deref().filter(|s| !s.is_empty()) {
                Some(icon) => format!("{icon} {}", seg.text),
                None => seg.text.clone(),
            };
            let config = UdiChipConfig::new(color, text);
            render_udi_chip(config, appearance)
        })
        .collect()
}

pub enum PluginPromptEvent {
    Changed,
}

impl Entity for PluginPromptModel {
    type Event = PluginPromptEvent;
}

impl SingletonEntity for PluginPromptModel {}
