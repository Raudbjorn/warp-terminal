use std::fs::remove_file;
use std::sync::Arc;
use std::time::Duration;

use chrono::{LocalResult, TimeZone, Utc};
use warp_core::execution_mode::AppExecutionMode;
use warp_errors::report_error;
use warpui::r#async::{FutureExt as _, Timer};
use warpui::{App, Entity, ModelContext, SingletonEntity};

use super::{LEGACY_PERSISTED_EVENTS_FILE_NAME, clear_event_queue};
use crate::auth::AuthStateProvider;
use crate::channel::ChannelState;
use crate::features::FeatureFlag;
use crate::server::server_api::ServerApi;
use crate::settings::{PrivacySettings, PrivacySettingsChangedEvent};

// How often we send Active Usage signals.
const ACTIVE_USAGE_DURATION: Duration = Duration::from_secs(60);

/// Duration to wait before draining the event queue.
const TELEMETRY_FLUSH_DURATION: Duration = Duration::from_secs(30);

/// Maximum time to wait for the telemetry flush during shutdown.
const TELEMETRY_SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// App singleton responsible for periodically draining the telemetry event queue (into the local
/// debug log when enabled; nothing is sent off-device). Respects the telemetry enablement setting.
pub struct TelemetryCollector {
    server_api: Arc<ServerApi>,
}

impl TelemetryCollector {
    pub fn new(server_api: Arc<ServerApi>) -> Self {
        Self { server_api }
    }

    pub fn initialize_telemetry_collection(&self, ctx: &mut ModelContext<TelemetryCollector>) {
        if ChannelState::is_local_only() {
            log::info!("Local-only services mode; telemetry collection is disabled.");
            return;
        }

        // Upstream persisted unsent events on quit to re-send on the next launch; nothing reads
        // or writes that file anymore, so just delete any leftovers.
        Self::remove_legacy_persisted_events_files();

        // Send Active App Usage signals
        if FeatureFlag::RecordAppActiveEvents.is_enabled()
            && (ChannelState::is_release_bundle() || FeatureFlag::WithSandboxTelemetry.is_enabled())
        {
            self.schedule_send_active_usage_event(ctx);
        }

        // Start a background thread to periodically flush events from the telemetry event queue.
        if ChannelState::is_release_bundle()
            || FeatureFlag::WithSandboxTelemetry.is_enabled()
            || FeatureFlag::SendTelemetryToFile.is_enabled()
        {
            self.schedule_event_queue_flush(ctx);
        }

        // Clear queued telemetry events when telemetry is enabled or disabled, so events recorded
        // while opted out are never written out after opting in (and vice versa).
        ctx.subscribe_to_model(&PrivacySettings::handle(ctx), |_me, _, event, _ctx| {
            if let PrivacySettingsChangedEvent::UpdateIsTelemetryEnabled { .. } = event {
                clear_event_queue();
            }
        });
    }

    /// Flushes telemetry events when the app is shutting down.
    ///
    /// Depending on the app's execution mode, this either drains the queue through the normal
    /// flush path (with a timeout) or simply discards the queued events.
    pub fn flush_telemetry_events_for_shutdown(&self, ctx: &mut ModelContext<TelemetryCollector>) {
        if ChannelState::is_local_only() {
            log::info!("Local-only services mode; not flushing telemetry events.");
            return;
        }

        let execution_mode = AppExecutionMode::as_ref(ctx);

        if execution_mode.send_telemetry_at_shutdown() {
            let privacy_settings_snapshot = PrivacySettings::as_ref(ctx).get_snapshot(ctx);
            let server_api = self.server_api.clone();
            match warpui::r#async::block_on(async move {
                server_api
                    .flush_telemetry_events(privacy_settings_snapshot)
                    .with_timeout(TELEMETRY_SHUTDOWN_FLUSH_TIMEOUT)
                    .await
            }) {
                Ok(Ok(count)) => {
                    if count > 0 {
                        log::info!("Successfully flushed telemetry events before shutdown");
                    }
                }
                Ok(Err(e)) => {
                    report_error!(e.context("Error flushing telemetry events before shutdown"));
                }
                Err(_) => {
                    log::warn!(
                        "Telemetry flush timed out after {}s during shutdown, skipping",
                        TELEMETRY_SHUTDOWN_FLUSH_TIMEOUT.as_secs()
                    );
                }
            }
        } else {
            clear_event_queue();
        }
    }

    fn remove_legacy_persisted_events_files() {
        let paths = [
            warp_core::paths::secure_state_dir()
                .map(|dir| dir.join(LEGACY_PERSISTED_EVENTS_FILE_NAME)),
            Some(warp_core::paths::state_dir().join(LEGACY_PERSISTED_EVENTS_FILE_NAME)),
        ];
        for path in paths.into_iter().flatten() {
            if let Err(e) = remove_file(&path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                report_error!(
                    anyhow::anyhow!(e).context("Failed to remove legacy persisted telemetry file")
                );
            }
        }
    }

    /// Schedules a background task to record an active usage event if telemetry is enabled. The scheduled task once again schedules itself after
    /// `ACTIVE_USAGE_DURATION`.
    fn schedule_send_active_usage_event(&self, ctx: &mut ModelContext<TelemetryCollector>) {
        let auth_state = AuthStateProvider::as_ref(ctx).get().clone();
        let is_telemetry_enabled = PrivacySettings::as_ref(ctx).is_telemetry_enabled;
        let _ = ctx.spawn(
            async move {
                // Record app active if there was any activity now or right after the previous check
                let last_active_timestamp = App::last_active_timestamp();
                if is_telemetry_enabled
                    && last_active_timestamp + ACTIVE_USAGE_DURATION.as_secs() as i64
                        > Utc::now().timestamp()
                    && let LocalResult::Single(timestamp) =
                        Utc.timestamp_opt(last_active_timestamp, 0)
                {
                    warpui::telemetry::record_app_active_event(
                        auth_state.user_id().map(|uid| uid.as_string()),
                        auth_state.anonymous_id(),
                        timestamp,
                    );
                }
                Timer::after(ACTIVE_USAGE_DURATION).await;
            },
            |me, _, ctx| me.schedule_send_active_usage_event(ctx),
        );
    }

    /// Drains the in-memory event queue and reschedules itself. The scheduled task once again schedules
    /// itself after `TELEMETRY_FLUSH_DURATION`.
    fn schedule_event_queue_flush(&self, ctx: &mut ModelContext<TelemetryCollector>) {
        let server_api = self.server_api.clone();
        let privacy_settings_snapshot = PrivacySettings::as_ref(ctx).get_snapshot(ctx);
        let _ = ctx.spawn(
            async move {
                match server_api
                    .flush_telemetry_events(privacy_settings_snapshot)
                    .await
                {
                    Ok(count) => {
                        if count > 0 {
                            log::debug!("Flushed telemetry events.");
                        }
                    }
                    Err(e) => {
                        log::info!("Failed to flush events from Telemetry queue: {e}");
                    }
                }
                Timer::after(TELEMETRY_FLUSH_DURATION).await;
            },
            |me, _, ctx| me.schedule_event_queue_flush(ctx),
        );
    }
}

impl Entity for TelemetryCollector {
    type Event = ();
}

impl SingletonEntity for TelemetryCollector {}
