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

use serde::{Deserialize, Serialize};
use warpui::{Entity, ModelContext, SingletonEntity};

/// Which side of the prompt a segment is grouped on. Left segments render first (after the built-in
/// chips), then right segments. (True right-edge alignment is classic-mode only; see PLUGIN_SPEC.md.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptSide {
    #[default]
    Left,
    Right,
}

/// One plugin-contributed prompt segment: the text to show, which side to group it on, and an
/// optional tooltip.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptSegment {
    pub text: String,
    #[serde(default)]
    pub side: PromptSide,
    #[serde(default)]
    pub tooltip: Option<String>,
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

    /// All segments in render order: every left segment (across plugins, by plugin id) then every
    /// right segment.
    pub fn ordered_segments(&self) -> Vec<&PromptSegment> {
        let mut out: Vec<&PromptSegment> = Vec::new();
        for seg in self.segments_by_plugin.values().flatten() {
            if matches!(seg.side, PromptSide::Left) {
                out.push(seg);
            }
        }
        for seg in self.segments_by_plugin.values().flatten() {
            if matches!(seg.side, PromptSide::Right) {
                out.push(seg);
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.segments_by_plugin.is_empty()
    }
}

pub enum PluginPromptEvent {
    Changed,
}

impl Entity for PluginPromptModel {
    type Event = PluginPromptEvent;
}

impl SingletonEntity for PluginPromptModel {}
