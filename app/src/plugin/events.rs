//! oh-my-warp: terminal command-lifecycle events for JS plugins (Milestone 2).
//!
//! Plugins subscribe via `warp.terminal.onCommandStart` / `onCommandFinished`. The host registers
//! the JS callback and records its id here, keyed by event name. The terminal view fires these
//! events (see `app/src/terminal/view.rs`) by invoking each registered callback via
//! `CallJsFunctionService` with the event payload; a callback that returns a string shows it as a
//! toast. See PLUGIN_SPEC.md (M2).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use rquickjs::{Ctx, FromJs, Object, Value};
use serde::{Deserialize, Serialize};
use warp_js::{FromWarpJs, IntoWarpJs, JsFunctionId, JsFunctionRegistry};

/// Event name for command-start handlers (`warp.terminal.onCommandStart`).
pub const EVENT_COMMAND_STARTED: &str = "command_started";
/// Event name for command-finished handlers (`warp.terminal.onCommandFinished`).
pub const EVENT_COMMAND_FINISHED: &str = "command_finished";

/// Payload passed to `onCommandFinished` callbacks.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandFinishedEvent {
    pub command: String,
    pub exit_code: i32,
    pub cwd: String,
    pub duration_ms: f64,
}

/// Payload passed to `onCommandStart` callbacks.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandStartedEvent {
    pub command: String,
    pub cwd: String,
}

impl<'js> IntoWarpJs<'js> for CommandFinishedEvent {
    fn into_warp_js(self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
        let obj = Object::new(ctx)?;
        obj.set("command", self.command)?;
        obj.set("exitCode", self.exit_code)?;
        obj.set("cwd", self.cwd)?;
        obj.set("durationMs", self.duration_ms)?;
        Ok(obj.into_value())
    }
}

impl<'js> IntoWarpJs<'js> for CommandStartedEvent {
    fn into_warp_js(self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
        let obj = Object::new(ctx)?;
        obj.set("command", self.command)?;
        obj.set("cwd", self.cwd)?;
        Ok(obj.into_value())
    }
}

/// Return value of an event callback: an optional toast message. A callback that returns a string
/// shows it as a toast; any other return (including `undefined`) shows nothing.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OptionalToast(pub Option<String>);

impl<'js> FromWarpJs<'js> for OptionalToast {
    fn from_warp_js(
        ctx: Ctx<'js>,
        value: Value<'js>,
        _registry: &mut JsFunctionRegistry,
    ) -> rquickjs::Result<Self> {
        if value.is_string() {
            Ok(OptionalToast(String::from_js(ctx, value).ok()))
        } else {
            Ok(OptionalToast(None))
        }
    }
}

static HANDLERS: LazyLock<Mutex<HashMap<String, Vec<JsFunctionId>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Registers a JS callback id for the given event name. Called on the app process when a plugin
/// invokes `warp.terminal.on*`.
pub fn register_handler(event: String, function_id: JsFunctionId) {
    log::info!("Registered plugin handler for event {event:?}");
    HANDLERS
        .lock()
        .unwrap()
        .entry(event)
        .or_default()
        .push(function_id);
}

/// Returns the callback ids registered for the given event name.
pub fn handlers(event: &str) -> Vec<JsFunctionId> {
    HANDLERS
        .lock()
        .unwrap()
        .get(event)
        .cloned()
        .unwrap_or_default()
}
