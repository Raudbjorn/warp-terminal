use rquickjs::{function::Opt, prelude::MutFn, Ctx, Function, Object};

use super::manifest::Manifest;
use super::plugin::PluginHandle;
use crate::context_chips::plugin_prompt::{PromptKind, PromptSegment, PromptSide};
use crate::plugin::ai_tools::ToolOutput;
use crate::plugin::app_requests::{PalettePluginItem, PluginAppRequest, ToastKind};
use crate::plugin::events::{
    CommandFinishedEvent, CommandStartedEvent, OptionalToast, EVENT_COMMAND_FINISHED,
    EVENT_COMMAND_STARTED,
};
use crate::workspace::plugin_status_items::{StatusItem, StatusItemKind};

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
pub fn warp<'js>(
    plugin: PluginHandle,
    manifest: &Manifest,
    plugin_id: &str,
    plugin_dir: &str,
    ctx: Ctx<'js>,
) -> rquickjs::Result<Object<'js>> {
    let api = Object::new(ctx)?;

    // Always-on base surface: API version, plugin identity, and logging.
    api.set("version", PLUGIN_API_VERSION)?;
    let plugin_info = Object::new(ctx)?;
    plugin_info.set("id", plugin_id)?;
    plugin_info.set("dir", plugin_dir)?;
    api.set("plugin", plugin_info)?;
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

    // Capability-gated namespaces (PLUGIN_SPEC.md §7). A legacy (no-`plugin.json`) plugin is
    // granted everything for back-compat; a manifest plugin gets only what it declares in
    // `permissions`. `keymap` rides with `commands` (it binds command ids to keys).
    if manifest.permits("commands") {
        api.set("commands", commands(plugin.clone(), ctx)?)?;
        api.set("keymap", keymap(ctx)?)?;
    }
    if manifest.permits("terminal:events") {
        api.set("terminal", terminal(plugin.clone(), ctx)?)?;
    }
    if manifest.permits("ui") {
        let ui_obj = ui(ctx)?;
        // `setStatusItem` needs the plugin id so each pill is identified per-plugin; install it as
        // a per-plugin overlay on the shared `ui` object after the base namespace is built.
        install_set_status_item(&ui_obj, plugin_id.to_string(), ctx)?;
        api.set("ui", ui_obj)?;
        api.set("prompt", prompt(plugin_id.to_string(), ctx)?)?;
    }
    if manifest.permits("ai") {
        api.set("ai", ai(plugin.clone(), ctx)?)?;
    }
    // High-sensitivity capabilities (PLUGIN_SPEC.md §7): only exposed when explicitly granted in
    // the manifest. `fs:read` and `fs:write` are independent grants gating individual methods.
    if manifest.permits("fs:read") || manifest.permits("fs:write") {
        api.set(
            "fs",
            fs_api(
                manifest.permits("fs:read"),
                manifest.permits("fs:write"),
                ctx,
            )?,
        )?;
    }
    if manifest.permits("process") {
        api.set("process", process_api(ctx)?)?;
    }
    if manifest.permits("network") {
        api.set("network", network_api(ctx)?)?;
    }

    #[cfg(feature = "completions_v2")]
    if manifest.permits("completions") {
        api.set("completions", completions(plugin, ctx)?)?;
    }

    Ok(api)
}

/// Returns a JS object representing the AI namespace for the Warp Plugin API.
///
/// `registerTool({ name, description, schema, run })` — registers a tool the Warp AI agent can
/// call. `schema` is a JSON Schema **string** for the argument object; `run(argsJson)` receives the
/// model's arguments as a JSON string and returns a string result. The tool is injected into the
/// agent's MCP context and dispatched in-process back to this callback. Requires the `ai`
/// permission. See PLUGIN_SPEC.md (M4).
fn ai<'js>(plugin: PluginHandle, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let ai = Object::new(ctx)?;
    ai.set(
        "registerTool",
        Function::new(
            ctx,
            MutFn::from(move |tool: Object<'js>| {
                let name: String = tool.get("name").unwrap_or_default();
                let description: String = tool.get("description").unwrap_or_default();
                let schema_json: String = tool.get("schema").unwrap_or_default();
                let Ok(run) = tool.get::<_, Function>("run") else {
                    log::warn!("warp.ai.registerTool: tool {name:?} is missing a 'run' function");
                    return;
                };
                if name.is_empty() {
                    log::warn!("warp.ai.registerTool: ignoring tool with empty 'name'");
                    return;
                }
                let mut plugin = plugin.get_mut();
                let func_ref = plugin
                    .js_function_registry_mut()
                    .register_js_function::<String, ToolOutput>(run, ctx);
                plugin.register_tool(name, description, schema_json, func_ref.id);
            }),
        ),
    )?;
    Ok(ai)
}

/// Returns the `warp.fs` namespace (capability-gated; PLUGIN_SPEC.md §7).
///
/// `readFile(path) -> string` / `readDir(path) -> string[]` (require `fs:read`);
/// `writeFile(path, content)` (requires `fs:write`). Runs synchronously in the plugin host (a
/// crash-isolated subprocess); errors throw a catchable JS exception. Every access is logged.
fn fs_api<'js>(can_read: bool, can_write: bool, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let fs = Object::new(ctx)?;
    if can_read {
        fs.set(
            "readFile",
            Function::new(ctx, move |path: String| -> rquickjs::Result<String> {
                log::info!("plugin warp.fs.readFile {path:?}");
                std::fs::read_to_string(&path).map_err(|e| {
                    rquickjs::Exception::throw_message(ctx, &format!("readFile {path:?}: {e}"))
                })
            }),
        )?;
        fs.set(
            "readDir",
            Function::new(ctx, move |path: String| -> rquickjs::Result<Vec<String>> {
                log::info!("plugin warp.fs.readDir {path:?}");
                let entries = std::fs::read_dir(&path).map_err(|e| {
                    rquickjs::Exception::throw_message(ctx, &format!("readDir {path:?}: {e}"))
                })?;
                Ok(entries
                    .flatten()
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .collect())
            }),
        )?;
    }
    if can_write {
        fs.set(
            "writeFile",
            Function::new(
                ctx,
                move |path: String, content: String| -> rquickjs::Result<()> {
                    log::info!(
                        "plugin warp.fs.writeFile {path:?} ({} bytes)",
                        content.len()
                    );
                    std::fs::write(&path, content).map_err(|e| {
                        rquickjs::Exception::throw_message(ctx, &format!("writeFile {path:?}: {e}"))
                    })
                },
            ),
        )?;
    }
    Ok(fs)
}

/// Returns the `warp.process` namespace (capability-gated; requires `process`).
///
/// `exec(command, args?) -> { stdout, stderr, code }` — spawns a subprocess and waits for it. Runs
/// synchronously in the plugin host; errors throw a catchable JS exception. Every spawn is logged.
fn process_api<'js>(ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let process = Object::new(ctx)?;
    process.set(
        "exec",
        Function::new(
            ctx,
            move |command: String, args: Opt<Vec<String>>| -> rquickjs::Result<Object<'js>> {
                let args = args.0.unwrap_or_default();
                log::info!("plugin warp.process.exec {command:?} {args:?}");
                let output = std::process::Command::new(&command)
                    .args(&args)
                    .output()
                    .map_err(|e| {
                        rquickjs::Exception::throw_message(ctx, &format!("exec {command:?}: {e}"))
                    })?;
                let result = Object::new(ctx)?;
                result.set(
                    "stdout",
                    String::from_utf8_lossy(&output.stdout).into_owned(),
                )?;
                result.set(
                    "stderr",
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                )?;
                result.set("code", output.status.code().unwrap_or(-1))?;
                Ok(result)
            },
        ),
    )?;
    Ok(process)
}

/// Returns the `warp.network` namespace (capability-gated; requires `network`).
///
/// `fetch(url) -> { status, body }` — performs a blocking HTTP GET. Runs synchronously in the
/// plugin host; errors throw a catchable JS exception. Every request is logged.
fn network_api<'js>(ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let network = Object::new(ctx)?;
    network.set(
        "fetch",
        // `reqwest::blocking::get` has no default timeout, so a hung or unrouteable host would
        // freeze the plugin runner thread (and the IPC relay it services) indefinitely. Build a
        // dedicated client with a 10s request timeout — long enough for slow remote APIs, short
        // enough that one stuck request can't wedge the host. The 10s cap matches our network
        // policy's max request window, so a plugin can't bypass the policy via `warp.network.fetch`.
        Function::new(ctx, move |url: String| -> rquickjs::Result<Object<'js>> {
            log::info!("plugin warp.network.fetch {url:?}");
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .map_err(|e| {
                    rquickjs::Exception::throw_message(ctx, &format!("failed to build client: {e}"))
                })?;
            let response = client.get(&url).send().map_err(|e| {
                rquickjs::Exception::throw_message(ctx, &format!("fetch {url:?}: {e}"))
            })?;
            let status = i32::from(response.status().as_u16());
            let body = response.text().unwrap_or_default();
            let result = Object::new(ctx)?;
            result.set("status", status)?;
            result.set("body", body)?;
            Ok(result)
        }),
    )?;
}

/// Returns a JS object representing the UI namespace for the Warp Plugin API.
///
/// `toast(message, kind?)` — transient toast; `showMarkdown(title, markdown)` — markdown panel;
/// `showPalette(title, items)` — command picker; `openWebTab(url)` — open an embedded browser pane;
/// `openProject(path)` — open a new tab with a terminal rooted at `path`.
///
/// Each request is enqueued (non-blocking) and relayed to the app on a background task; we must not
/// do the blocking IPC inline on the plugin runner thread (see `super::app_request`).
fn ui<'js>(ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let ui = Object::new(ctx)?;
    ui.set(
        "toast",
        Function::new(ctx, |message: String, kind: Opt<String>| {
            let kind = match kind.0.as_deref() {
                Some("error") => ToastKind::Error,
                Some("warn") => ToastKind::Warn,
                _ => ToastKind::Info,
            };
            super::app_request::send_app_request(PluginAppRequest::ShowToast { message, kind });
        }),
    )?;
    ui.set(
        "showMarkdown",
        Function::new(ctx, |title: String, markdown: String| {
            super::app_request::send_app_request(PluginAppRequest::ShowMarkdown {
                title,
                markdown,
            });
        }),
    )?;
    // `showPalette(title, items)` — `items` is `[{ label, command, description?, icon?, kbd? }]`
    // where `command` is a command id registered via `warp.commands.register`. On selection the app
    // dispatches that command as a fresh `RunPluginCommand`. Items reference command ids (not inline
    // callbacks) so this never re-enters `plugin.get_mut()` from inside the calling callback. The
    // optional `description` / `icon` / `kbd` fields drive the row's decoration (see
    // `PalettePluginItem`). See PLUGIN_SPEC.md (M4).
    ui.set(
        "showPalette",
        Function::new(ctx, |title: String, items: Vec<Object<'js>>| {
            let palette_items: Vec<PalettePluginItem> = items
                .into_iter()
                .filter_map(|item| {
                    let label: String = item.get("label").ok()?;
                    let command_id: String = item.get("command").ok()?;
                    let description: Option<String> = item.get("description").ok();
                    let icon: Option<String> = item.get("icon").ok();
                    let kbd: Option<String> = item.get("kbd").ok();
                    Some(PalettePluginItem {
                        label,
                        command_id,
                        description,
                        icon,
                        kbd,
                    })
                })
                .collect();
            super::app_request::send_app_request(PluginAppRequest::ShowPalette {
                title,
                items: palette_items,
            });
        }),
    )?;
    // `openWebTab(url)` — opens an embedded browser pane navigated to `url` (a bare host like
    // `example.com` is upgraded to `https://`). Fire-and-forget, like the other `ui.*` relays.
    ui.set(
        "openWebTab",
        Function::new(ctx, |url: String| {
            super::app_request::send_app_request(PluginAppRequest::OpenWebTab { url });
        }),
    )?;
    // `openProject(path)` — opens a new tab with a terminal rooted at `path` (the tmux-sessionizer
    // workflow). Fire-and-forget.
    ui.set(
        "openProject",
        Function::new(ctx, |path: String| {
            super::app_request::send_app_request(PluginAppRequest::OpenProject { path });
        }),
    )?;
    Ok(ui)
}

/// Returns the `warp.ui.setStatusItem`-aware bindings as a `(plugin_id, ui_object)` overlay. The
/// `ui` namespace is shared across plugins (one function per method), so we install per-plugin
/// `setStatusItem` separately in [`warp`] after building the base `ui` object — that way each
/// plugin's call carries its own id without us threading it through every method.
fn install_set_status_item<'js>(
    ui: &Object<'js>,
    plugin_id: String,
    ctx: Ctx<'js>,
) -> rquickjs::Result<()> {
    // `setStatusItem(id, item|null)` — `item` is `{ text, kind?, tooltip?, command? }`. Passing
    // `null` (or any non-object) removes the pill. The pill is identified by `(plugin_id, id)`, so
    // a plugin can publish several pills and update each independently.
    ui.set(
        "setStatusItem",
        // `Opt<rquickjs::Value>` rather than `Opt<Object>`: rquickjs's `FromJs` for `Object`
        // calls `into_object()` which fails on `null`/`undefined`, throwing a JS `TypeError`
        // before the `Opt` wrapper can see the missing value. We catch the conversion *outside*
        // `Opt` and then ask the `Value` for its inner object — `None` for primitives, `Some(obj)`
        // for objects, and `None` for the documented `null`/non-object "remove" path.
        Function::new(ctx, move |item_id: String, item: Opt<rquickjs::Value<'js>>| {
            let item = item.0.and_then(|val| val.into_object()).and_then(|obj| {
                let text: String = obj.get("text").ok()?;
                if text.is_empty() {
                    // Empty text is treated as a remove, so plugins don't need separate clear
                    // call sites (`setStatusItem("ci", { text: "" })` is fine).
                    return None;
                }
                let kind = match obj.get::<_, String>("kind").ok().as_deref() {
                    Some("success") => StatusItemKind::Success,
                    Some("warn") => StatusItemKind::Warn,
                    Some("error") => StatusItemKind::Error,
                    Some("accent") => StatusItemKind::Accent,
                    _ => StatusItemKind::Info,
                };
                let tooltip: Option<String> = obj.get("tooltip").ok();
                let command_id: Option<String> = obj.get("command").ok();
                Some(StatusItem {
                    text,
                    kind,
                    tooltip,
                    command_id,
                })
            });
            super::app_request::send_app_request(PluginAppRequest::SetStatusItem {
                plugin_id: plugin_id.clone(),
                item_id,
                item,
            });
        }),
    )?;
    Ok(())
}

/// Returns the `warp.prompt` namespace (capability: `ui`) — plugin-contributed prompt segments.
///
/// `set(segments)` — `segments` is `[{ text, side?, tooltip? }]` (`side`: `"left"` | `"right"`,
///     default left). Replaces *this plugin's* segments in the native prompt (empty array clears);
///     `clear()` removes them. They render as native chips after the built-in ones. The plugin
///     chooses when to refresh (e.g. from `terminal.onCommandFinished` or a timer). Fire-and-forget,
///     like the other relays (see `super::app_request`). The calling plugin's id is captured so each
///     plugin owns its own segments.
fn prompt<'js>(plugin_id: String, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let prompt = Object::new(ctx)?;

    let set_plugin_id = plugin_id.clone();
    prompt.set(
        "set",
        Function::new(ctx, move |segments: Vec<Object<'js>>| {
            let segments: Vec<PromptSegment> = segments
                .into_iter()
                .filter_map(|seg| {
                    let text: String = seg.get("text").ok()?;
                    if text.is_empty() {
                        return None;
                    }
                    let side = match seg.get::<_, String>("side").ok().as_deref() {
                        Some("right") => PromptSide::Right,
                        _ => PromptSide::Left,
                    };
                    // `kind` is optional; an unknown value silently falls back to Info so a
                    // plugin shipped before the field existed (or one targeting a newer Warp)
                    // still renders.
                    let kind = match seg.get::<_, String>("kind").ok().as_deref() {
                        Some("success") => PromptKind::Success,
                        Some("warn") => PromptKind::Warn,
                        Some("error") => PromptKind::Error,
                        Some("accent") => PromptKind::Accent,
                        _ => PromptKind::Info,
                    };
                    let tooltip: Option<String> = seg.get("tooltip").ok();
                    let icon: Option<String> = seg.get("icon").ok();
                    Some(PromptSegment {
                        text,
                        side,
                        tooltip,
                        kind,
                        icon,
                    })
                })
                .collect();
            super::app_request::send_app_request(PluginAppRequest::SetPrompt {
                plugin_id: set_plugin_id.clone(),
                segments,
            });
        }),
    )?;

    prompt.set(
        "clear",
        Function::new(ctx, move || {
            super::app_request::send_app_request(PluginAppRequest::SetPrompt {
                plugin_id: plugin_id.clone(),
                segments: Vec::new(),
            });
        }),
    )?;

    Ok(prompt)
}

/// Returns a JS object representing the Keymap namespace for the Warp Plugin API.
///
/// `bind(commandId: string, keys: string)` — binds a key sequence (e.g. `"ctrl-b g"`) to a
///     command registered via `warp.commands.register`. The user's `keybindings.yaml` overrides it.
fn keymap<'js>(ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let keymap = Object::new(ctx)?;
    keymap.set(
        "bind",
        Function::new(ctx, |command_id: String, keys: String| {
            super::app_request::send_app_request(PluginAppRequest::BindKey { keys, command_id });
        }),
    )?;
    Ok(keymap)
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
/// `register(id: string, title: string, callback: () => string | void)`: Registers a
///     command-palette command. When the user runs it, `callback` executes in the plugin host; if
///     it returns a string, that string is shown to the user as a toast (returning nothing is fine).
fn commands<'js>(plugin: PluginHandle, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let commands = Object::new(ctx)?;
    commands.set(
        "register",
        Function::new(
            ctx,
            MutFn::from(move |id: String, title: String, callback: Function<'js>| {
                let mut plugin = plugin.get_mut();
                // Output is `OptionalToast` (lenient): a command may return a string to toast, or
                // nothing. Using `String` would error on a void return and drop the response
                // (surfacing as "oneshot cancelled").
                let func_ref = plugin
                    .js_function_registry_mut()
                    .register_js_function::<String, OptionalToast>(callback, ctx);
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
