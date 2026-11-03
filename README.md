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

### SSH profiles

Open **Connect to SSH host** with **Ctrl+Shift+H** (macOS: **Cmd+Shift+H**), or
choose **Toggle SSH profiles panel** from the command palette. The SSH Profiles
panel lets you add and edit hosts, identity files, jump hosts, and tags; pin hosts;
and open saved or recent connections. The launcher also accepts
`user@host:port` for a quick connection. Existing toolbar customizations are
preserved when opening the panel.

Profiles are read from `~/.ssh/config`. Saving a profile updates its host stanza,
preserves unrecognized directives, and keeps a backup at `~/.ssh/config.warp.bak`.
Hand-written stanzas are imported for use and editing; only Warp-managed stanzas
can be deleted from the panel. Pattern and multi-alias stanzas stay in the file
but are not listed as editable profiles. Leave the hostname or port blank to
inherit SSH configuration. Additional IdentityFile entries survive unrelated
edits; change multi-key lists directly in the config file. Remove references to a
saved jump host before renaming or deleting it. Profile metadata and recent-host history stay local.
Password authentication uses SSH's normal terminal prompt; identity files and
SSH-agent authentication are supported. Automatic saved-password release is
unavailable because the shell-launched connection cannot securely bind a helper
request to the exact SSH process. Previously stored passwords can be cleared in
the editor but are never read by the manager.

Remote-tab reopening uses the original connection's control socket and refuses
restoration if effective SSH settings have changed since launch. It accepts at
most eight terminal panes per restoration and limits remote stdout to 4 MiB and
stderr to 16 KiB while reading. A missing control socket causes an error rather
than a new connection. SSH config saves recheck the file before replacing it,
though external editors that ignore Warp's lock can still race the final rename.

The manager is adapted from [Changroro/warp](https://github.com/Changroro/warp),
with its implementation in `app/src/ssh_manager/` and small integration modules in
`app/src/terminal/view/ssh_manager.rs` and
`app/src/workspace/view/ssh_manager.rs`. The source snapshot is
[`f25058b7`](https://github.com/Changroro/warp/commit/f25058b7d3bbae94bc5a3b6f051d6edf50b74251);
remote-session reopening and the workspace adapter use
[`92b8e4cf`](https://github.com/Changroro/warp/commit/92b8e4cf)
to keep this integration independent of the sibling's session archive and agent
logo features. The pre-existing `warp_ssh_manager` database crate is unchanged;
this UI uses SSH config profiles and does not migrate that crate's database rows.

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
