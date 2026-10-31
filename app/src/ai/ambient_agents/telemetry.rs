use serde::Serialize;
use serde_json::{Value, json};
use strum_macros::{EnumDiscriminants, EnumIter};
use warp_core::features::FeatureFlag;

use crate::server::ids::ServerId;

/// The entry point through which Cloud Mode was entered.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudModeEntryPoint {
    /// User clicked "New Cloud Agent Tab" or similar action to create a dedicated Cloud Mode tab.
    NewTab,
    /// User entered Cloud Mode from an existing local terminal session (e.g., via keyboard shortcut or command).
    LocalSession,
    /// User entered Cloud Mode through the Oz launch modal.
    OzLaunchModal,
    /// User re-entered Cloud Mode by clicking on an ambient agent entry block.
    EntryBlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffSurface {
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    Gui,
    #[cfg_attr(not(feature = "tui"), allow(dead_code))]
    Tui,
}

/// The entry point through which a local-to-cloud handoff was initiated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffEntryPoint {
    /// User typed `&` in the input to enter handoff compose mode.
    #[default]
    Ampersand,
    /// User used the `/handoff` slash command.
    SlashCommand,
    /// User clicked the "Hand off to cloud" chip in the footer toolbar.
    FooterChip,
    /// The client automatically initiated handoff for an eligible local agent.
    Automatic,
}

/// Describes which synthetic-input path drives an empty-prompt handoff.
/// Captured at handoff initiation so telemetry reflects the intended path
/// regardless of whether the snapshot derivation later produces content.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HandoffInjectionPath {
    /// The handoff carried a non-empty user prompt; no client-side injection.
    #[default]
    None,
    /// Empty prompt + in-progress source. The client substituted `"Continue"`
    /// on the wire so the cloud agent picks up where the local agent left off.
    Continue,
    /// Empty prompt + idle source. The client substituted
    /// `"Apply the workspace changes from my previous session."` on the wire
    /// alongside the snapshot token; the cloud agent's first user-role turn
    /// carries an intent for the rehydrated workspace state.
    SnapshotRehydration,
}
