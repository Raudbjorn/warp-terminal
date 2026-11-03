# Warp (local fork)

## About this fork

This is a **fully-local fork of [Warp](https://www.warp.dev)**. It keeps Warp's
terminal and agentic development environment, but removes the dependency on
Warp's backend:

- **No Warp API.** The client never talks to Warp's servers for AI.
- **No authentication.** There is no login/account step — the app starts
  straight into a usable session.
- **The agent loop runs locally.** Instead of delegating the multi-agent loop to
  Warp's backend, this fork drives the loop on your machine and talks directly
  to an OpenAI-compatible API.

### AI features require a custom endpoint

Because nothing is served by Warp, **AI features only work once you point the
app at a model endpoint you control.** Use any OpenAI-compatible server, local
(Ollama, LM Studio, vLLM, llama.cpp's server, ...) or remote (OpenAI,
OpenRouter, or any gateway that speaks the OpenAI API), or
[OpenCode](https://opencode.ai). Until an endpoint is configured, the terminal
works normally but AI actions have nowhere to go.

Turn on **Settings → Local AI → Use Local AI provider** first: the OSS build
treats AI as disabled while it is off, which also disables the Agent Profile
fields below, even if you only want the Warp Agent.

There are three separate AI paths, each with its own settings:

- **Natural-language commands, workflow metadata and command prediction:**
  **Settings → Local AI.** Set the base URL (e.g. `http://localhost:11434/v1`),
  an API key *if your server requires one*, and the model names. In this build
  the endpoint must be a loopback address (`localhost`, `127.0.0.1`, `::1`);
  LAN, Tailscale and public hosts are rejected.
- **Warp Agent conversations:** **Settings → Agents → Warp Agent → Custom
  Inference.** Add a custom endpoint that uses the OpenAI Chat Completions
  schema and select one of its models. A request for any other model is
  refused; it is never sent to Warp.
- **Command autocomplete (off by default), model discovery and the *Test*
  button:** **Settings → Agents → Agent Profiles**, section *LOCAL INFERENCE*.
  These go through a bundled local service. If *OpenAI Base URL* is left empty
  there, the service and the *Test* button use `https://api.openai.com/v1`.

See [docs/local-ai.md](docs/local-ai.md) for every setting, the OpenCode
provider, and exactly what the local-only network policy does and does not
cover.

### Save and stream command output to files

Block and session context menus offer local file capture in place of cloud
sharing, adapted from [thikone/warp-terminal](https://github.com/thikone/warp-terminal).
Choose a destination in the native save dialog:

| Command | Content | Default shortcut |
|---|---|---|
| Save output to file | Selected blocks' output | `Ctrl+Shift+S` |
| Stream output to file | Output, updated while the command runs | `Ctrl+Alt+Shift+S` |
| Save block to file | Working directory, command and output | Unbound |
| Stream block to file | Working directory, command and live output | Unbound |
| Save session to file | Retained commands and output in the current pane | Unbound |
| Stream session to file | Retained history and subsequent commands in the pane | Unbound |

On macOS, Save Output uses `Cmd+Shift+S`. Without a block selection, block captures
use the latest command. Block streams
finish with the command; session streams can start at an empty prompt and continue
across commands until the pane closes. Starting another session stream replaces
that pane's previous session stream. The tab context menu captures its focused
pane without switching tabs. Captures contain rendered text: progress-line
redraws replace the current tail instead of appending every intermediate frame.
Live capture mirrors the terminal's retained output: if the active command exceeds
the configured grid limit, older lines can also disappear from its captured tail.
Use shell redirection or a PTY recording when a complete byte log is required.

File operations run in the background. The app allows up to eight open or pending
file captures at a time; a stalled destination retains its place until its I/O finishes.
Closing a pane or clearing its history drains the last available snapshot. If the
file is still opening, capture stops with an incomplete-snapshot warning. Abrupt
application termination can leave the final write incomplete.

All six actions are listed in the Resource Center's **Terminal** shortcuts and
can be assigned keys in **Settings → Keyboard Shortcuts**. Filename patterns live
under **Settings → Features → Session** and are stored locally:

| TOML key | Default |
|---|---|
| `terminal.file_capture.output_pattern` | `{command}_{timestamp}.log` |
| `terminal.file_capture.block_pattern` | `{command}_{timestamp}.log` |
| `terminal.file_capture.session_pattern` | `{session-or-command}_{timestamp}.log` |

Patterns support `{command}`, `{timestamp}`, `{finished-timestamp}` and
`{session-or-command}`. For session captures, `{session-or-command}` uses the
custom tab name from any capture entry point, falling back to the first command.
For output and block captures, it uses the target command.
Command and tab-name tokens are sanitized and limited to 64 UTF-8 bytes without
splitting characters. Custom pattern text can still make a filename too long.
Clearing a settings field restores its default. The pattern suggests a filename;
the save dialog lets you change it.

**Captured files include secrets in plaintext, even if the terminal masks them.**
New files are created with owner-only permissions on Unix; overwriting an
existing file preserves that file's permissions.

## Building and running this fork

The build is driven by the scripts in [`script/`](script). This fork builds as
the **OSS channel**, which needs no Warp-internal access.

### 1. One-time setup

Install the toolchain and build dependencies (Rust, `protoc`, GUI/runtime libs,
and the bundler):

```bash
./script/bootstrap
```

> The `cargo` toolchain installs under `~/.cargo`; make sure `~/.cargo/bin` is on
> your `PATH` (`export PATH="$HOME/.cargo/bin:$PATH"`).

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow (checks,
test layout, pull requests).

### 2. Debug build — for development & debugging

Build and run straight from source with debug assertions. This is the fastest
edit/run loop and what you want while hacking on the fork:

```bash
./script/run
```

### 3. Release build — a real, installed app for daily use

For everyday use you want an optimized, packaged application you launch from your
desktop — not a binary you start from another terminal. Build a release bundle
of the OSS channel:

```bash
./script/bundle --channel oss
```

This fork builds for Linux only. By default `./script/bundle` produces an
AppImage.

To get a proper desktop entry (launcher icon, system integration) on
Debian/Ubuntu, build a `.deb` instead and install it system-wide:

```bash
./script/bundle --channel oss --packages deb --release-tag v0.1.0
sudo dpkg -i target/release-lto/bundle/linux/warp-terminal-oss_0.1.0_amd64.deb
```

After that, **WarpOss** appears in your application launcher like any other
installed app. (`--packages` also accepts `rpm` and `arch`.)

## Documentation

- [docs/local-ai.md](docs/local-ai.md): configuring local and private AI
  endpoints, the OpenCode provider, and local-only network behavior.
- [UPSTREAM.md](UPSTREAM.md): how this fork relates to
  [warpdotdev/warp](https://github.com/warpdotdev/warp) and how it is kept in
  sync.
- The terminal itself is inherited from Warp, and the upstream docs at
  <https://docs.warp.dev/> describe most terminal features. Anything there
  that depends on a Warp account, Warp's cloud or credits needs Warp's
  servers, which this fork does not use.

## Contributing and support

- Bugs, feature requests and questions: [open an issue](https://github.com/Raudbjorn/warp-terminal/issues)
  on this repository. Do not file them against upstream Warp unless they
  reproduce there too.
- Development workflow: [CONTRIBUTING.md](CONTRIBUTING.md).
- Security reports: [SECURITY.md](SECURITY.md).

## Licensing

Warp's UI framework (the `warpui_core` and `warpui` crates) is licensed under
the [MIT license](LICENSE-MIT).

The rest of the code in this repository is licensed under the
[AGPL v3](LICENSE-AGPL).

This fork is a derivative of Warp and keeps its upstream copyright notices.
Third-party license texts for bundled dependencies are generated by
`script/prepare_bundled_resources` from [`about.toml`](about.toml); the accepted
license list is shared with [`deny.toml`](deny.toml).
