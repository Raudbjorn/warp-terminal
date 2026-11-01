# Warp plugin specification

This describes the JavaScript plugin host as `app/src/plugin/` implements it, for API version
**1.0.0** (`PLUGIN_API_VERSION` in `app/src/plugin/host/native/js_api/mod.rs`). Where the host does
less than a field's name suggests, that is stated. Source comments refer to sections of this file
as `PLUGIN_SPEC.md §N`, and to the milestones in §10 as `M1`–`M4`.

## 1. Overview

A plugin is a directory with a JavaScript entry point and an optional manifest. The host runs each
plugin in the QuickJS runtime of a separate plugin-host process, so a crashing plugin cannot take
the app down. The plugin host is built into the default build through the `omw_plugins` feature.

A plugin gets one object, `warp`, handed to its `activate` function. What `warp` contains is decided
by the permissions in the manifest (§7).

## 2. Discovery and layout

The host scans `~/.warp/plugins/`. A directory there is a plugin if it contains `main.js`.

```
~/.warp/plugins/<dir>/
  main.js        required: the entry point, an ES module
  plugin.json    optional: the manifest (§4)
```

The plugin's id is the manifest's `id`, else the directory name.

## 3. Lifecycle

1. The host reads `plugin.json` without running any plugin code (§4).
2. It checks `engines.warp` against the API version. An incompatible plugin is skipped, with a
   warning in the log (§9).
3. It applies `contributes.keybindings` (§6).
4. It evaluates `main.js` and calls its exported `activate(warp)`.

```js
export function activate(warp) {
  warp.log("hello from " + warp.plugin.id);
}
```

After `activate` returns, the host calls back into the plugin when a registered command is run, a
terminal event fires, or an AI tool is called. Everything runs on the plugin's single thread: the
`fs`, `process` and `network` calls block it until they return.

## 4. Manifest

`plugin.json` is a JSON object. Every field is optional.

| Field | Meaning |
| --- | --- |
| `id`, `name`, `version`, `description` | Identity and display text (Settings lists them). |
| `engines.warp` | A semver range of API versions the plugin targets, e.g. `"^1.0"` (§9). |
| `permissions` | Strings from the table in §7. |
| `contributes.keybindings` | `[{ "command": "<id>", "key": "<keys>" }]` (§6). |
| `contributes.commands` | `[{ "id", "title" }]`. Parsed and not used yet: commands appear through `warp.commands.register`. |
| `activationEvents` | Parsed and not used yet. Plugins activate at start-up. |

Unknown fields are ignored.

## 5. The always-present surface

Every plugin gets these, whatever its permissions:

- `warp.version`: the API version string.
- `warp.plugin`: `{ id, dir }`.
- `warp.log(message, level?)`: `level` is `"info"` (default), `"warn"` or `"error"`.
- `console.log` and `console.err`, which write to the log.

## 6. Contributions

`contributes.keybindings` binds a key sequence to a command id. The binding is applied before
`activate` runs and is resolved when the key is pressed, so it works whether or not `activate` has
registered the command by then. `warp.keymap.bind` does the same at run time. Keybindings in the
user's `keybindings.yaml` override both.

## 7. Permissions

A permission grants a namespace of `warp`. A plugin gets exactly the permissions in its manifest.

| Permission | Grants |
| --- | --- |
| `commands` | `warp.commands`, `warp.keymap` |
| `terminal:events` | `warp.terminal` |
| `ui` | `warp.ui`, `warp.prompt` |
| `ai` | `warp.ai` |
| `fs:read` | `warp.fs.readFile`, `warp.fs.readDir` |
| `fs:write` | `warp.fs.writeFile` |
| `process` | `warp.process` |
| `network` | `warp.network` |

`fs:read` and `fs:write` are independent. `fs`, `process` and `network` calls are logged. Beyond
the network policy in §8.9, the host adds no restriction: `fs` takes any path the user can reach and
`process` runs any program.

### Plugins without a usable manifest

A directory with only `main.js` has no permissions, so it gets the surface in §5 and nothing else.
It still loads. The same goes for a `plugin.json` that is missing, unreadable or does not parse as
the shape in §4: the host logs a warning for a malformed one and loads the plugin with no
permissions. A broken manifest never grants anything.

To use more than §5, a plugin needs a `plugin.json` that declares the permissions.

## 8. API reference

Calls that fail throw a JavaScript exception that the plugin can catch.

### 8.1 `warp.commands` (`commands`)

`register(id, title, callback)` adds a command to the command palette. `callback()` may return a
string, which is shown as a toast.

### 8.2 `warp.keymap` (`commands`)

`bind(commandId, keys)` binds a key sequence such as `"ctrl-b g"` to a command registered with
`warp.commands.register`.

### 8.3 `warp.terminal` (`terminal:events`)

`onCommandStart(cb)` calls `cb({ command, cwd })` when a shell command starts.
`onCommandFinished(cb)` calls `cb({ command, exitCode, cwd, durationMs })` when one finishes. If a
callback returns a string, it is shown as a toast.

### 8.4 `warp.ui` (`ui`)

- `toast(message, kind?)`: `kind` is `"error"`, `"warn"` or anything else for info.
- `showMarkdown(title, markdown)`: a markdown panel.
- `showPalette(title, items)`: a picker. Each item is `{ label, command, description?, icon?, kbd? }`,
  where `command` is a command id registered with `warp.commands.register`.
- `openWebTab(url)`: an embedded browser pane. A bare host such as `example.com` becomes `https://`.
- `openProject(path)`: a new tab with a terminal in `path`.
- `setStatusItem(id, item)`: a pill in the status area, identified by the plugin and `id`.
  `item` is `{ text, kind?, tooltip?, command? }` and `kind` is `"success"`, `"warn"`, `"error"`,
  `"accent"` or info. A `null` item or empty `text` removes the pill.

These requests are queued and carried out by the app; they do not return a result.

### 8.5 `warp.prompt` (`ui`)

`set(segments)` replaces the plugin's segments in the prompt; `clear()` removes them. A segment is
`{ text, side?, tooltip?, kind? }`, with `side` `"left"` (default) or `"right"`. Plugin segments
render as chips after the built-in ones, left segments first. They are not aligned to the right
edge of the terminal; that alignment exists only in the classic prompt mode.

### 8.6 `warp.ai` (`ai`)

`registerTool({ name, description, schema, run })` offers a tool to the AI agent. `schema` is a JSON
Schema string for the arguments. `run(argsJson)` gets the model's arguments as a JSON string and
returns a string result. A tool without a `name` or without a `run` function is ignored with a
warning.

### 8.7 `warp.fs` (`fs:read`, `fs:write`)

`readFile(path)` returns a string and `readDir(path)` returns a list of names; both need `fs:read`.
`writeFile(path, content)` needs `fs:write`.

### 8.8 `warp.process` (`process`)

`exec(command, args?)` runs a program, waits for it, and returns `{ stdout, stderr, code }`.

### 8.9 `warp.network` (`network`)

`fetch(url)` makes a blocking `GET` and returns `{ status, body }`. The body is decoded as UTF-8.

It follows the app's network policy. In local-only services mode, which the open-source build is
always in, only loopback URLs can be fetched, every redirect is checked against the same rule, and
proxy settings from the environment are ignored. A request the policy refuses throws without
connecting. A request takes at most 10 seconds, and a response body over 8 MiB throws.

## 9. API versions and `engines.warp`

`warp.version` is a semantic version. Within a major version the API only grows.

The host checks `engines.warp` before it loads any plugin code. A plugin whose range does not
include the host's API version, or whose range is not a valid semver range, is skipped, and the
reason goes to the log. A plugin with no range, including one with no manifest, is compatible with
every version.

## 10. Milestones

Source comments name the milestone that introduced a piece of the host.

- **M1: palette commands.** `warp.commands.register` and its entries in the command palette.
- **M2: terminal events.** `warp.terminal.onCommandStart` and `onCommandFinished`.
- **M3: app requests and key bindings.** The channel through which a plugin asks the app to do
  something, and `warp.keymap.bind`.
- **M4: manifest and surfaces.** `plugin.json`, permissions, `engines.warp`; `warp.ui` panels,
  pickers and status pills; `warp.prompt`; `warp.ai`; the list of installed plugins in Settings.
