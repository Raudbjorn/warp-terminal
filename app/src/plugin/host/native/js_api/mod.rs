use rquickjs::{function::Opt, prelude::MutFn, Ctx, Function, Object};

use super::plugin::PluginHandle;
use crate::plugin::events::{
    CommandFinishedEvent, CommandStartedEvent, OptionalToast, EVENT_COMMAND_FINISHED,
    EVENT_COMMAND_STARTED,
};

/// Semantic version of the `warp.*` plugin API surface. Plugins declare the range
/// they target via `engines.warp` in their manifest; the surface evolves additively
/// within a major version. See PLUGIN_SPEC.md.
pub const PLUGIN_API_VERSION: &str = "1.0.0";

cfg_if::cfg_if! {
    if #[cfg(feature = "completions_v2")] {
        use rquickjs::Value;
        use warp_completer::signatures::CommandSignature;
        use warp_js::FromWarpJs;
    }
}

/// Returns a JS object representing the Warp Plugin API exposed to external JavaScript plugins.
///
/// The base API (always present) exposes:
/// * `warp.version: string` — the [`PLUGIN_API_VERSION`] of the `warp.*` surface.
/// * `warp.log(message: string, level?: "info" | "warn" | "error")` — logs through the
///   host logger (which is relayed to the app via the IPC `LogService`).
/// * `warp.commands.register(id, title, callback)` — registers a command-palette command;
///   `callback()` returns a string that is shown as a toast when the command runs.
/// * `warp.terminal.onCommandStart(cb)` / `onCommandFinished(cb)` — register callbacks invoked
///   when a shell command starts/finishes; a callback that returns a string shows it as a toast.
///
/// Additional namespaces are added behind feature flags (e.g. `completions` under
/// `completions_v2`). See PLUGIN_SPEC.md for the planned surface.
pub fn warp(plugin: PluginHandle, ctx: Ctx<'_>) -> rquickjs::Result<Object<'_>> {
    let api = Object::new(ctx)?;

    api.set("version", PLUGIN_API_VERSION)?;
    api.set(
        "log",
        Function::new(ctx, |message: String, level: Opt<String>| {
            match level.0.as_deref() {
                Some("error") => log::error!("{message}"),
                Some("warn") => log::warn!("{message}"),
                _ => log::info!("{message}"),
            }
        }),
    )?;
    api.set("commands", commands(plugin.clone(), ctx)?)?;
    api.set("terminal", terminal(plugin.clone(), ctx)?)?;

    #[cfg(feature = "completions_v2")]
    api.set("completions", completions(plugin, ctx)?)?;
    Ok(api)
}

/// Returns a JS object representing the Terminal namespace for the Warp Plugin API.
///
/// API methods:
///
/// `onCommandStart(callback: (event) => string | void)` — invoked when a command starts; `event`
///     is `{ command, cwd }`.
/// `onCommandFinished(callback: (event) => string | void)` — invoked when a command finishes;
///     `event` is `{ command, exitCode, cwd, durationMs }`. A returned string is shown as a toast.
fn terminal<'js>(plugin: PluginHandle, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let terminal = Object::new(ctx)?;

    let on_finished_plugin = plugin.clone();
    terminal.set(
        "onCommandFinished",
        Function::new(
            ctx,
            MutFn::from(move |callback: Function<'js>| {
                let mut plugin = on_finished_plugin.get_mut();
                let func_ref = plugin
                    .js_function_registry_mut()
                    .register_js_function::<CommandFinishedEvent, OptionalToast>(callback, ctx);
                plugin.register_event_handler(EVENT_COMMAND_FINISHED.to_string(), func_ref.id);
            }),
        ),
    )?;

    terminal.set(
        "onCommandStart",
        Function::new(
            ctx,
            MutFn::from(move |callback: Function<'js>| {
                let mut plugin = plugin.get_mut();
                let func_ref = plugin
                    .js_function_registry_mut()
                    .register_js_function::<CommandStartedEvent, OptionalToast>(callback, ctx);
                plugin.register_event_handler(EVENT_COMMAND_STARTED.to_string(), func_ref.id);
            }),
        ),
    )?;

    Ok(terminal)
}

/// Returns a JS object representing the Commands namespace for the Warp Plugin API.
///
/// API methods:
///
/// `register(id: string, title: string, callback: () => string)`: Registers a command-palette
///     command. When the user runs it, `callback` executes in the plugin host and its returned
///     string (if any) is shown to the user as a toast.
fn commands<'js>(plugin: PluginHandle, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let commands = Object::new(ctx)?;
    commands.set(
        "register",
        Function::new(
            ctx,
            MutFn::from(move |id: String, title: String, callback: Function<'js>| {
                let mut plugin = plugin.get_mut();
                let func_ref = plugin
                    .js_function_registry_mut()
                    .register_js_function::<String, String>(callback, ctx);
                plugin.register_command(id, title, func_ref.id);
            }),
        ),
    )?;
    Ok(commands)
}

/// Returns a JS object to be used as a the `console` global, implementing `console.log()` and
/// `console.err()`.
pub fn console(ctx: Ctx<'_>) -> rquickjs::Result<Object<'_>> {
    let console = Object::new(ctx)?;
    console.set(
        "log",
        Function::new(ctx, |message: String| {
            log::info!("{message}");
        }),
    )?;
    console.set(
        "err",
        Function::new(ctx, |message: String| {
            log::error!("{message}");
        }),
    )?;
    Ok(console)
}

/// Returns a JS object representing the Completions namespace for the Warp Plugin API.
///
/// API methods:
///
/// `registerCommandSignature(signature: CommandSignature[] | CommandSignature)`: Registers
///     the given command signature(s) to be used for completions.
#[cfg(feature = "completions_v2")]
fn completions<'js>(plugin: PluginHandle, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let completions = Object::new(ctx)?;
    completions.set(
        "registerCommandSignature",
        Function::new(
            ctx,
            MutFn::from(move |val: Value<'js>| {
                if val.is_array() {
                    let mut plugin = plugin.get_mut();
                    match Vec::<CommandSignature>::from_warp_js(
                        ctx,
                        val,
                        plugin.js_function_registry_mut(),
                    ) {
                        Ok(signatures) => plugin.register_command_signatures(signatures),
                        Err(e) => {
                            log::warn!("Attempted to register invalid JS CommandSignatures {e:?}")
                        }
                    }
                } else if val.is_object() {
                    let mut plugin = plugin.get_mut();
                    match CommandSignature::from_warp_js(
                        ctx,
                        val,
                        plugin.js_function_registry_mut(),
                    ) {
                        Ok(signature) => {
                            plugin.register_command_signatures(vec![signature]);
                        }
                        Err(e) => {
                            log::warn!("Attempted to register invalid JS CommandSignature {e:?}")
                        }
                    }
                }
            }),
        ),
    )?;
    Ok(completions)
}
