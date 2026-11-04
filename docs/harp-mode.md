# Harp mode

Harp mode turns the vertical tabs into an agent sidebar. It is a port of
[dominikmartn/harp](https://github.com/dominikmartn/harp) 0.1.8, an unofficial
Warp fork for running many Claude Code and Codex agents side by side. The code
is under the same licenses as upstream Warp: AGPL-3.0, except the `warpui` and
`warpui_core` crates, which are MIT. The bundled Claude Code plugin is based on
Warp's plugin and is MIT (`resources/bundled/claude-plugin/harp/LICENSE-warp`).

Harp mode is off by default. With it off, the app looks and behaves like
`master`.

## What it adds

- Sidebar rows that show each agent's status: working, needs you, done, and a
  dot while background work is still running.
- Tab groups, pins, and read and active flags in the sidebar.
- Limit rings in the sidebar footer: the share of the 5 hour window and of the
  weekly limit used, per provider. See [Limit rings](limit-rings.md).
- An optional tmux sync: tab names, groups, order and flags are stored on the
  tmux sessions.
- Rename a tab by double-clicking its row or from the row's right-click menu.

## Turning it on and off

Use one of these:

- Command palette: **Toggle Harp Mode** (binding id
  `workspace:toggle_harp_mode`).
- **Settings > Appearance > Tabs > Harp mode (agent sidebar)**.
- `settings.toml`:

  ```toml
  [modes.harp]
  enabled = true
  ```

The settings file is `~/.config/warp-oss/settings.toml` for the oss channel
(`$XDG_CONFIG_HOME/warp-oss/settings.toml` if `XDG_CONFIG_HOME` is set). In
debug builds started with `WARP_DATA_PROFILE=<name>` it is in
`warp-oss-<name>`; release builds ignore that variable. The app reloads the
file when it changes.

Turning Harp mode on also turns on vertical tabs and opens the tabs panel.
Turning it off leaves vertical tabs on, with the upstream look. Turn vertical
tabs off separately if you want the horizontal tab bar back.

In Harp mode the CLI agent toolbar under agent sessions is hidden. The
`agents.third_party.should_render_cli_agent_toolbar` setting is not changed;
the toolbar comes back as configured when Harp mode is off.

## Settings

| Setting | Default | Effect |
|---|---|---|
| `modes.harp.enabled` | `false` | Turns Harp mode on. |
| `appearance.vertical_tabs.full_tab_menu` | `true` | `false` shrinks the tab and group menus to rename, move to group, read/unread, active/inactive and close. Also in Settings > Appearance > Tabs. |
| `integrations.limit_rings.enabled` | `true` | Shows the limit rings in the sidebar footer. Has an effect only in Harp mode. |
| `integrations.limit_rings.file` | `""` | Optional JSON file with more providers; `~` is your home folder. Format: [limit-rings.md](limit-rings.md). |
| `integrations.tmux.enabled` | `false` | Tmux sync (names, groups, order, flags, adopting sessions) and agent detection inside tmux. Polls every 10 s. |
| `integrations.tmux.session_script` | `""` | Optional script that renames and regroups instead of the built-in tmux commands: `<script> rename <shell_pid> <group> <title>`, `<script> group <session_id> <old_group> <new_group>`. |

With tmux sync on, a tmux session started elsewhere (for example over ssh) can
ask for a tab with `tmux set-option -t <session> @warp_adopt 1`. The app adopts
only sessions with no client attached, so detach first (or run the command
from outside the session); the tab opens on the next poll.

## Requirements

- `jq`. The bundled Claude Code plugin's scripts use it. Without it the plugin
  reports nothing.
- `tmux`, only if `integrations.tmux.enabled` is on.
- Inside tmux, the Claude Code plugin's reports reach the app only with
  `set -g allow-passthrough on` in your tmux configuration: the plugin writes
  them to the pane's tty wrapped in tmux's passthrough sequence, and tmux drops
  that unless passthrough is allowed.

## Transcript text

By default the Claude Code plugin's stop notifications carry no text from the
conversation: the row keeps the prompt of the turn it began with, and the
desktop notification says "Task completed." That text is what leaves the
transcript and reaches the notification, the sidebar and the app's logs, so it
is opt-in. Set `HARP_STOP_TRANSCRIPT_TEXT=1` in the environment Claude Code
starts in (your shell's startup file, or `env` in Claude Code's settings) and
the stop hooks add the last prompt and the start of the last answer, each cut to
200 characters. The text is passed on as it is, with no filtering for secrets: if
your prompts or Claude's answers can hold credentials, leave the variable unset.
The prompt shown as the row's title when you submit it is not affected.

The app logs a warning at startup and when Harp mode or tmux sync is turned
on if the bundled plugin is not found, `jq` is missing, or tmux sync is on and
`tmux` is missing. It checks the app's own `PATH`, which can differ from the
one your shell's startup files build.

## Shell environment

In Harp mode the app sets two variables in every new shell:

- `CLAUDE_CODE_PLUGIN_DIRS` gets the bundled plugin
  (`<resources>/bundled/claude-plugin/harp`). If the variable is already set,
  the plugin is appended to its colon-separated list. Each Claude Code started
  in that shell loads the plugin for that session. The plugin is not installed
  into `~/.claude`, and Claude Code in other terminals is not affected.
- `HARP_AGENT_STATUS=1`.

With Harp mode off, new shells get neither: `HARP_AGENT_STATUS` is removed, so
a plugin inherited from an outer Harp-mode window stays quiet.

Loading a plugin from `CLAUDE_CODE_PLUGIN_DIRS` needs Claude Code 2.1.280 or
later. An older Claude Code ignores the variable; its rows still show working
and done, but not "needs you" or the background dot.

Only shells started after Harp mode was turned on get these variables. Shells
that were already open, and warpkeep sessions recovered from before, keep the
environment they started with.

## Side effect on `~/.claude/settings.json`

Claude Code hands its plan limits only to its status line. When Harp mode and
`integrations.limit_rings.enabled` are both on and `~/.claude` exists, the app
adds a `statusLine` entry to `~/.claude/settings.json` if that file has none
(it creates the file if it is missing). The entry saves the
status line's input to the fork's state directory
(`~/.local/state/warp-oss/claude-statusline.json` for the oss channel) and
prints nothing.

An existing `statusLine` is left alone. To keep yours and get the rings, save
the input first. Write to a temporary file and rename it, as the app's own
entry does, so several Claude Code sessions updating at once never leave a
half-written file:

```json
"statusLine": {
  "type": "command",
  "command": "f=~/.local/state/warp-oss/claude-statusline.json; mkdir -p \"${f%/*}\"; tee \"$f.$$\" | your-command; mv -f \"$f.$$\" \"$f\""
}
```

A status line set up by Harp itself writes `~/.harp/claude-statusline.json`.
The app reads that file too and uses whichever of the two is newer, so the
rings keep working without changing it.

The app adds the entry only once. It then writes a marker file,
`claude-statusline.installed`, in the same state directory, and does not add
the entry again while the marker exists. To undo the side effect, remove the
`statusLine` entry from `~/.claude/settings.json`; it stays removed. Turning
Harp mode off does not remove the entry.

Claude Code has one status line for all its sessions, while each channel or
data profile of the app has its own state directory. Only the one that added
the entry gets Claude rings.

Codex rings are read from `$CODEX_HOME/sessions` (default `~/.codex/sessions`).
They appear once Codex has answered at least once.

## Development builds

A development build does not find the bundled plugin unless you point it at
the source tree's resources:

```sh
WARP_RESOURCES_DIR=$PWD/resources make warp-run
```

`WARPOSS_STATUS_TRACE=1` (set at launch) logs every change of a row's shown
status as a `[status-check]` line to `~/.local/state/warp-oss/warp-oss.log`.

## Troubleshooting

**A Claude Code row shows no "needs you" status.**

- Check that the plugin is in the shell's environment. In a shell opened in
  Harp mode, `echo "$CLAUDE_CODE_PLUGIN_DIRS"` should end with
  `bundled/claude-plugin/harp`, and that directory should contain `hooks` and
  `scripts`. If it does not, the shell was started before Harp mode was turned
  on, or the app did not find its resources (see Development builds).
- Check that `jq` is on `PATH` and look for the warning in the log.
- An agent started before Harp mode was turned on, or outside a shell of this
  app, has no plugin. Restart it in a new tab. Without the plugin, the row
  still shows working and done, read from the spinner in the agent's terminal
  title.

**The limit rings do not show.**

- Check that `integrations.limit_rings.enabled` is `true`.
- Claude: check that `~/.claude/settings.json` has a `statusLine` that writes
  `claude-statusline.json` to the state directory or to `~/.harp` (see
  above). The rings appear after Claude Code's
  next answer, and only for Claude.ai plans, not API keys.
- Codex: check that `$CODEX_HOME/sessions` or `~/.codex/sessions` exists.
