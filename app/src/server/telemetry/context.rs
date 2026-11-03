//! Static client context (OS info, browser user agent) attached to shared-session telemetry.

use std::sync::OnceLock;

use serde::Serialize;
use serde_json::{Value, json};
use warp_errors::report_error;
#[cfg(target_family = "wasm")]
use warpui::platform::wasm;

use crate::server::OperatingSystemInfo;

static TELEMETRY_CONTEXT: OnceLock<TelemetryContext> = OnceLock::new();

#[derive(Serialize)]
struct TelemetryContextInfo {
    /// Info about the operating system of the client.
    #[serde(skip_serializing_if = "Option::is_none")]
    os: Option<&'static OperatingSystemInfo>,
    /// The user agent provided by the browser, if running on Web. If not on
    /// Web, this is always `None`.
    #[serde(rename = "userAgent", skip_serializing_if = "Option::is_none")]
    user_agent: Option<String>,
}

/// Newtype representing a [`Value`] with a serialized version of the client context.
pub struct TelemetryContext(Value);

impl TelemetryContext {
    pub fn as_value(&self) -> Value {
        self.0.clone()
    }
}

impl TelemetryContext {
    fn new() -> Self {
        let context = TelemetryContextInfo {
            os: OperatingSystemInfo::get().ok(),
            user_agent: user_agent(),
        };

        match serde_json::to_value(context) {
            Ok(value) => Self(value),
            Err(e) => {
                report_error!(
                    anyhow::Error::new(e)
                        .context("Failed to serialize telemetry context info to JSON value")
                );
                Self(json!({}))
            }
        }
    }
}

/// Returns the user agent provided by the browser, if on Web. If not on Web,
/// or if the user agent was not able to be read, returns None.
fn user_agent() -> Option<String> {
    cfg_if::cfg_if! {
        if #[cfg(target_family = "wasm")] {
            wasm::user_agent()
        } else {
            None
        }
    }
}

/// Returns the telemetry context
/// that should be attached to all telemetry events associated to this client.
pub fn telemetry_context() -> &'static TelemetryContext {
    TELEMETRY_CONTEXT.get_or_init(TelemetryContext::new)
}
