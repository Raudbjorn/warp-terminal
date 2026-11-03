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
