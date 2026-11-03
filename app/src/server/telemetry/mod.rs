mod collector;
mod context;
pub mod context_provider;
mod events;
mod macros;
pub mod secret_redaction;

#[cfg(not(target_family = "wasm"))]
use std::fs::OpenOptions;
#[cfg(not(target_family = "wasm"))]
use std::io::Write as _;

use anyhow::Result;
pub use collector::*;
pub use context::telemetry_context;
pub use events::*;
#[cfg(not(target_family = "wasm"))]
use serde_json::{Value, json};
use warpui::telemetry::Event;
#[cfg(not(target_family = "wasm"))]
use warpui::telemetry::EventPayload;

use crate::ChannelState;
use crate::auth::UserUid;
#[cfg(not(target_family = "wasm"))]
use crate::features::FeatureFlag;
use crate::settings::PrivacySettingsSnapshot;

/// Filename upstream used for telemetry events persisted on quit (to be re-sent on next launch).
/// Nothing writes it anymore; the collector only deletes leftovers.
const LEGACY_PERSISTED_EVENTS_FILE_NAME: &str = "rudder_telemetry_events.json";

/// Removes all telemetry events from the app telemetry event queue.
pub fn clear_event_queue() {
    let _ = warpui::telemetry::flush_events();
}

/// Local-only telemetry sink.
///
/// oh-my-warp: there is no remote telemetry transport. Queued events are drained and dropped;
/// the only place they can end up is the local debug log written when
/// [`FeatureFlag::SendTelemetryToFile`] is enabled (and even then, never events containing UGC).
#[derive(Default)]
pub struct TelemetryApi;

impl TelemetryApi {
    pub fn new() -> Self {
        Self
    }

    /// Drains the global telemetry queue. Returns the number of events drained.
    pub async fn flush_events(&self, settings_snapshot: PrivacySettingsSnapshot) -> Result<usize> {
        let events = warpui::telemetry::flush_events();
        let event_count = events.len();
        if ChannelState::is_local_only() || settings_snapshot.should_disable_telemetry() {
            return Ok(event_count);
        }

        self.write_events_to_local_log(events)?;
        Ok(event_count)
    }

    /// Records a single `TelemetryEvent` immediately (bypassing the queue).
    pub async fn send_telemetry_event(
        &self,
        user_id: Option<UserUid>,
        anonymous_id: String,
        event: impl warp_core::telemetry::TelemetryEvent,
        settings_snapshot: PrivacySettingsSnapshot,
    ) -> Result<()> {
        if ChannelState::is_local_only() || settings_snapshot.should_disable_telemetry() {
            return Ok(());
        }

        let event = warpui::telemetry::create_event(
            user_id.map(|uid| uid.as_string()),
            anonymous_id,
            event.name().into(),
            event.payload(),
            event.contains_ugc(),
            warpui::time::get_current_time(),
        );
        self.write_events_to_local_log(vec![event])
    }

    #[cfg(target_family = "wasm")]
    fn write_events_to_local_log(&self, _events: Vec<Event>) -> Result<()> {
        Ok(())
    }

    /// Appends non-UGC events as JSON lines to the channel's telemetry log file when
    /// [`FeatureFlag::SendTelemetryToFile`] is enabled; otherwise a no-op.
    #[cfg(not(target_family = "wasm"))]
    fn write_events_to_local_log(&self, events: Vec<Event>) -> Result<()> {
        if !FeatureFlag::SendTelemetryToFile.is_enabled() {
            return Ok(());
        }

        // `telemetry_config: None` means telemetry is disabled for this build, and
        // `telemetry_file_name()` is then empty — which would make the path below
        // collapse to the log directory itself.
        let file_name = ChannelState::telemetry_file_name();
        if file_name.is_empty() {
            return Ok(());
        }

        let lines: Vec<String> = events
            .into_iter()
            .filter(|event| !event.contains_ugc)
            .map(|event| event_to_json(event).to_string())
            .collect();
        if lines.is_empty() {
            return Ok(());
        }

        let log_directory = warp_logging::log_directory()?;
        let path = log_directory.join(&*file_name);
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        for line in lines {
            writeln!(file, "{line}")?;
        }
        Ok(())
    }
}

#[cfg(not(target_family = "wasm"))]
fn event_to_json(event: Event) -> Value {
    let (kind, name, user_id, anonymous_id, payload) = match event.payload {
        EventPayload::IdentifyUser {
            user_id,
            anonymous_id,
        } => ("identify", None, Some(user_id), anonymous_id, None),
        EventPayload::AppActive {
            user_id,
            anonymous_id,
        } => (
            "track",
            Some("Active App Usage".into()),
            user_id,
            anonymous_id,
            None,
        ),
        EventPayload::NamedEvent {
            user_id,
            anonymous_id,
            name,
            value,
        } => ("track", Some(name), user_id, anonymous_id, value),
    };
    json!({
        "type": kind,
        "event": name,
        "user_id": user_id,
        "anonymous_id": anonymous_id,
        "payload": payload,
        "timestamp": event.timestamp,
        "session_created_at": event.session_created_at,
    })
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
