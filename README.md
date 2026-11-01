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

### Editor and source-control additions from Hako

This fork selectively integrates the following features from
[egehakan/hako](https://github.com/egehakan/hako), inspected at
`fff676a033aeb7423b27cad3bfb3076b1a125edc`:

- **Syntax highlighting:** Svelte (`.svelte`), SCSS (`.scss`), Prisma (`.prisma`),
  and Typst (`.typ`), including their language names in fenced code blocks.
- **Material file and folder icons:** full-color icons in the file tree, file
  pickers, and global search. The bundled artwork retains its MIT license.
- **Project-wide replace:** use **Replace in files** in the command palette, or
  expand the replacement field in Global Search. Preview replacements, restrict
  files with include/exclude patterns, and replace a match, a file, or all matches.
  Regex captures, whole-word matching, and case preservation are supported.
  **Undo last replace in files** restores the most recent replacement while
  refusing to overwrite subsequent edits. Replacement operates on local files;
  remote search remains available.
- **Project formatters:** **Format document** (`Shift-Alt-F`) and format-on-save
  use a formatter selected by the project, falling back to the existing language
  server where applicable. Configure **Format on save** and **Use project
  formatters** in **Settings → Code → Editor & Code Review**. Project formatting
  runs installed tools; it does not download formatters. Before using a project's
  tools, plugins, or executable configuration, invoke **Format document** and
  approve that project for the current application session. Ordinary saves use
  the existing language-server path until approval; the global setting does not
  grant permission. Approved projects also work when opened through a symlink.
  Prettier receives an explicit in-project configuration or disables configuration
  lookup. Its implicit EditorConfig lookup is disabled; put those settings in
  the project's Prettier configuration instead. Discovery runs off the UI thread
  with a bounded wait. A failed or stale result cannot replace newer editor text.
  Typing-triggered autosave skips formatting;
  saves during tab closure flush immediately without waiting for a formatter.
- **Multi-repository Git:** open source control from a terminal in a local folder
  containing repositories or linked worktrees. Each repository has its own
  changes, inline file diffs, commit/push/publish, fetch/sync, and discard actions.
  Sync only fast-forwards or pushes; divergent histories are left for you to
  reconcile. Push respects the branch's configured upstream. Discard requires
  confirmation and removes untracked files as well as tracked changes.

Replacement skips unsafe or unsupported files (including binary files and files
or open buffers over 16 MiB). Selected replacement output and edit allocations
are bounded by the same per-file budget. Its undo journal stores copies under the app's local state directory,
including unsaved buffer contents, and keeps up to five transactions within a
512 MiB total budget. Unix journals use owner-only permissions; on Windows they
inherit the state directory's ACL. Project replacement clears ordinary buffer
undo history; use the replacement journal to undo it. Undo refuses files that
have grown beyond its 256 MiB read limit.

Replacement and undo wait up to 30 seconds for each batch of required buffer saves,
and cancellation stops that wait promptly. Pending writes keep their files and
undo copies protected until their actual completion is known, while unrelated
files can be replaced subject to the journal limits. At most 32 guarded saves can
be retained at once; additional clean buffers in a batch are skipped and can be
retried after the batch finishes. A lost completion signal keeps protection in place until application
restart. Failed saves are reported and retain their journal entries for retry;
intentionally dirty buffers stay unsaved. These protections are local to this
application and do not serialize external filesystem changes. Disk replacement
checks resolved containment and rechecks file
identity and content after preparing the temporary file. A concurrent filesystem
change in the final check-to-rename interval is still possible.

Repository discovery searches up to six levels, skips `node_modules` and symlinked
directories, and avoids scanning the home directory or its ancestors. The overview
is for native local sessions, not SSH or WSL sessions. Its refresh control can
rescan the folder and reload all repository statuses. Git metadata changes outside
the watched folder (for linked worktrees) are picked up within the next 30-second
status refresh, subject to the bounded status-load queue.

The feature ports retain their existing crate boundaries: `languages` and
`syntax_tree` for highlighting, `project_replace` shared with `warp_ripgrep`,
`code_formatters` for project formatting, and the application's `code_review`
modules for multi-repository Git. Source commits include `16e680f6`, `9ae545c0`,
`4294a071`, `5afec9a8`, `93b08676`, `f240fe03`, `1f772d13`, `455fbde5`, and
`abad4863` through `8e2a9b7b` for the Git integration.

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
