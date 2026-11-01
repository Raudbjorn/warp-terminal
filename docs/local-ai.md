# Local AI

This fork's OSS build never talks to Warp's servers. AI features work only
against endpoints you configure yourself, with one default to know about: the
*OpenAI Base URL* of an Agent Profile falls back to `https://api.openai.com/v1`
when it is empty (see [Bundled local service](#bundled-local-service)). There
are **three separate AI code paths** with separate settings, and the network
rules differ between them, so read the first two sections before configuring
anything.

Everything below was read from the source on `master`. Where behavior is
known to be incomplete, the text says so and links the tracking issue.

## Which setting controls which feature

| | Single-shot features | Warp Agent conversations | Bundled local service |
|---|---|---|---|
| What it powers | Natural-language to command, workflow metadata, command prediction | Multi-step agent conversations with tool use, and passive suggestions | Local command autocomplete (off by default), model discovery and the *Test* button in Agent Profiles, and a stand-in for the Warp server URL |
| Where the code runs | In the app: `LocalOpenAIClient` (`app/src/ai/local_openai.rs`) | In the app: the `local_agent` crate (`crates/local_agent`), chosen by `app/src/ai/agent/api/local_route.rs` | A bundled child process, `warp-local-multi-agent` (`crates/local_multi_agent_service`), started by `app/src/local_multi_agent` when the app starts. It listens on `127.0.0.1:8787` by default |
| Where you configure it | **Settings → Local AI**, or `[agents.local_openai]` in `settings.toml` | **Settings → Agents → Warp Agent → Custom Inference → Add custom endpoint** (base URL, API key, model names), when the workspace policy allows it | **Settings → Agents → Agent Profiles**, section *LOCAL INFERENCE* (API key, *OpenAI Base URL*, model aliases, *Autocomplete* switch) |
| Providers | OpenAI-compatible Chat Completions, or OpenCode | Custom endpoints that use the OpenAI Chat Completions schema | OpenAI-compatible Chat Completions |
| Network policy | Enforced: loopback only (see [Network behavior](#network-behavior-in-the-oss-build)) | Not enforced: the engine uses a plain `reqwest` client | Not enforced: the service and the app's health and `/models` calls use plain `reqwest` clients ([#32](https://github.com/Raudbjorn/warp-terminal/issues/32)) |

Changing one of these settings does not change the other two.

## Single-shot features: Local AI settings

Open **Settings → Local AI**, turn on *Use Local AI provider*, pick a
*Provider*, and fill in the fields. The page is only shown in the OSS
(local-only) build. The same values live in `settings.toml` under
`[agents.local_openai]`:

| Key | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `false` | Master switch for the single-shot features. |
| `base_url` | string | `http://127.0.0.1:1234/v1` | OpenAI-compatible base URL. Must be loopback (see below). Not used by the OpenCode provider. |
| `api_key` | string | empty | Optional. Local servers such as Ollama and LM Studio usually do not need one. |
| `command_model` | string | `google/gemma-4-e4b` | Model for command generation and workflow metadata. |
| `prediction_model` | string | `google/gemma-4-e4b` | Model for command prediction. |
| `timeout_ms` | integer | `8000` | Per-request timeout. Values are clamped to 1 to 3 600 000. |
| `provider_kind` | string | `open_ai_compatible` | `open_ai_compatible` or `open_code`. `opencode` and `open-code` are accepted as aliases. An empty or unrecognized value falls back to `open_ai_compatible`. |
| `opencode_command` | string | `opencode` | Binary to spawn when `provider_kind = "open_code"`. A bare name is resolved from `PATH`; use an absolute path if the app does not inherit your shell's `PATH`. |
| `opencode_args` | list of strings | `["serve", "--port", "0"]` | Arguments passed to the OpenCode binary. In the settings page they are edited as one shell-style string, so quote any argument that contains spaces. |
| `use_responses_api` | bool | `false` | **No effect today.** The Responses API code path is not called by any feature. Tracked in [#29](https://github.com/Raudbjorn/warp-terminal/issues/29). |

### Example: OpenAI-compatible server (Ollama)

```toml
[agents.local_openai]
enabled = true
provider_kind = "open_ai_compatible"
base_url = "http://localhost:11434/v1"
command_model = "qwen2.5-coder:7b"
prediction_model = "qwen2.5-coder:7b"
timeout_ms = 15000
```

### Example: OpenCode

```toml
[agents.local_openai]
enabled = true
provider_kind = "open_code"
opencode_command = "/usr/local/bin/opencode"
# Models are written providerID/modelID. The provider must already be
# configured inside OpenCode.
command_model = "ollama/qwen2.5-coder:7b"
prediction_model = "ollama/qwen2.5-coder:7b"
```

## OpenCode provider

[OpenCode](https://opencode.ai) is a coding-agent runtime whose `serve` command
exposes an HTTP API. It does not speak Chat Completions, so Warp drives its
session API instead.

- **Requirements.** An `opencode` binary that serves the `/api/session` API.
  The settings page notes it was checked against OpenCode 2.2.
- **How the sidecar starts.** Warp runs `opencode_command` with `opencode_args`
  (default `serve --port 0`), reads the listening port OpenCode prints on
  stdout, and waits up to 60 seconds for it. The server binds loopback and
  requires HTTP Basic auth with a random per-process password that Warp passes
  through `OPENCODE_SERVER_PASSWORD` and `OPENCODE_PASSWORD`.
- **What a request does.** Each request creates a short-lived session with every
  tool permission denied, asks for a completion, and deletes the session, so a
  command-generation prompt cannot make OpenCode run tools.
- **Models.** Write them `providerID/modelID`. Anything else is rejected with
  *OpenCode model `…` must be written `providerID/modelID`*.
- **Data leaves the machine if your OpenCode provider is hosted.** The loopback
  rule only covers the hop from Warp to the sidecar. What OpenCode does next is
  up to its own provider configuration.
- **Working directory.** Today there is one sidecar per Warp process, running in
  the app's working directory, not one per repository
  ([#39](https://github.com/Raudbjorn/warp-terminal/issues/39)).
- **Troubleshooting.**
  - *OpenCode binary `…` is not available on PATH*: install OpenCode, or set
    `opencode_command` to an absolute path. A desktop launcher often gives the
    app a shorter `PATH` than your shell.
  - *failed to start within 60s* or *exited before becoming ready*: run the same
    command by hand (`opencode serve --port 0`) and check that it prints its
    port. Warp does not currently surface the sidecar's stdout and stderr
    ([#38](https://github.com/Raudbjorn/warp-terminal/issues/38)).
  - *stdout could not be parsed*: `opencode_args` must make OpenCode print the
    bound port, as the default `--port 0` does.

## Warp Agent conversations

Conversations run inside the app. `route_request`
(`app/src/ai/agent/api/local_route.rs`) serves a request with the `local_agent`
crate only when both of these hold:

- the selected model belongs to one of your custom endpoints, and that endpoint
  uses the **OpenAI Chat Completions** schema;
- the input is a user query, a tool-call result or a passive-suggestion result.

Every other request is refused with an error that starts *Local agent loop is
enabled, so this request was not sent to the Warp server*. It is not forwarded
to Warp and not to the bundled service. That covers any model that is not one of
your custom endpoints' models (Warp-hosted and `auto` models included), custom
endpoints that use the OpenAI Responses or Anthropic Messages schema (the
settings page offers them, but this build does not route them), and requests
such as code review, environment creation, skills and orchestration.

To use the Warp Agent, add a custom endpoint under **Settings → Agents → Warp
Agent → Custom Inference** with the Chat Completions schema, for example
`http://localhost:11434/v1` for Ollama, and select one of its models. The
profile's *LOCAL INFERENCE* settings are not used for conversations.

The engine makes one model call per request. The app runs the tool calls the
model asks for and continues the conversation with their results.

## Bundled local service

In the OSS build the app starts `warp-local-multi-agent` at launch and points
its Warp server URL at it (`127.0.0.1:8787` by default). Conversations do not
run there (see above). The app uses the service for:

- **Command autocomplete**, when the *Autocomplete* switch of the profile is on.
  It is off by default. The profile's API key, base URL and model aliases are
  sent along with each request.
- **Model discovery and the *Test* button** in Agent Profiles, which call the
  service's `/health` and then `GET {base URL}/models` with the API key as a
  bearer token.
- The "no-cloud" GraphQL surface (`/graphql/v2`) that the service provides for
  local integrations.

Defaults come from `crates/local_multi_agent_service/config-schema.json`:

| Setting | Default |
|---|---|
| Service address | `127.0.0.1:8787` |
| Provider base URL | empty in the schema, which both the app and the service treat as `https://api.openai.com/v1` |

Per-profile *OpenAI API key*, *OpenAI Base URL* and the model aliases
(`auto`, `auto-efficient`, `auto-coding`, `auto-reasoning`, `auto-autocomplete`)
are edited in **Settings → Agents → Agent Profiles**. Point the base URL at any
OpenAI-compatible server, for example `http://localhost:11434/v1` for Ollama.

> **Default endpoint.** If *OpenAI Base URL* is empty in the profile, the
> *Test* button and the service's provider calls (autocomplete, when it is
> switched on) default to `https://api.openai.com/v1`, and send your API key
> there if you set one (`DEFAULT_OPENAI_BASE_URL` in `app/src/local_multi_agent/mod.rs`,
> `DEFAULT_BASE_URL` in `crates/local_multi_agent_service/src/config.rs`). Set
> the base URL before pressing *Test* or turning on *Autocomplete* if you do not
> want that. Warp Agent conversations are not affected: they use only the custom
> endpoints you add.

## Network behavior in the OSS build

`app/src/bin/oss.rs` starts the app in `ServicesMode::LocalOnly` with telemetry
and auto-update configuration unset. Concretely:

- **Requests made through `http_client`** (this includes every Local AI
  single-shot request) are checked against `network_policy` before they are
  sent. Only loopback hosts pass: `localhost`, `127.0.0.0/8`, `::1` and
  IPv4-mapped loopback, over `http`, `https`, `ws` or `wss`. Private-network,
  Tailscale and public addresses are denied with `NetworkPolicyDenied`, and so
  is a hostname that merely resolves to loopback. Every redirect hop is checked
  again, and system proxy settings are ignored.
- **Warp Agent conversations and the bundled service are not covered by that
  check. This is a known limitation.** The conversation engine
  (`crates/local_agent`) and the service make their own requests to the
  provider, and the app's health check, model-list refresh and autocomplete
  calls to the service use plain `reqwest` clients too. None of them applies
  `network_policy`, re-validates redirects or bypasses system proxies. A
  provider URL you configure can therefore point at a non-loopback host, and the
  API key and prompt content go there, possibly through `HTTP(S)_PROXY` and
  after redirects. Routing the app and service calls through the policy is
  tracked in [#32](https://github.com/Raudbjorn/warp-terminal/issues/32). Whether
  LAN and Tailscale endpoints should become an explicit opt-in is open in
  [#33](https://github.com/Raudbjorn/warp-terminal/issues/33).
- **No account or Warp server.** There is no login step, and Warp server URLs
  are the local-only configuration.
- **Disabled CLI subcommands.** These are rejected and hidden from `--help`:
  `agent`, `environment`, `run`, `task`, `model`, `login`, `logout`, `whoami`,
  `provider`, `integration`, `schedule`, `secret`, `federate`,
  `harness-support`, `artifact`.

### What can still reach the network

| Component | Reachable hosts |
|---|---|
| Single-shot Local AI, OpenAI-compatible | Loopback only |
| Single-shot Local AI, OpenCode | Loopback to the sidecar; the sidecar reaches whatever its configured provider uses |
| Warp Agent conversations | Whatever base URL the custom endpoint sets (not policy-checked) |
| Bundled service (autocomplete, *Test*, model discovery) | Whatever base URL the profile sets (not policy-checked; `https://api.openai.com/v1` when empty) |
| Telemetry, auto-update | Not configured in the OSS channel |

## Keeping this page accurate

The settings keys above are the `toml_path` entries in
`app/src/settings/ai.rs`. When one is added, renamed or removed, update the
table. The behavior notes link their tracking issues; drop the caveat when the
issue closes.
