# Local Multi-Agent API

Rust implementation of Warp's local `/ai/multi-agent` sidecar. It accepts Warp's protobuf request body, calls an OpenAI-compatible `/chat/completions` endpoint, streams protobuf response events over server-sent events, and serves the no-cloud GraphQL surface used by local integrations.

## Run

```sh
cargo run -p local_multi_agent_service --bin warp-local-multi-agent
```

The service loads `.env` from the current directory at startup. Shell environment variables take precedence over values in `.env`.

Common settings:

- `OPENAI_API_KEY`
- `OPENAI_BASE_URL` unless Warp sends `X-Warp-OpenAI-Base-URL`
- `OPENAI_MODEL`
- `LOCAL_MODEL_ALIASES`
- `LOCAL_MODEL_LIST`
- `LOCAL_ENABLE_TOOLS`
- `LOCAL_MAX_HISTORY_MESSAGES`
- `LOCAL_MAX_COMPLETION_TOKENS`
- `LOCAL_MODEL_CONTEXT_TOKENS`
- `LOCAL_GRAPHQL_DB_PATH`
- `LOCAL_SERVICE_LOG_PATH`
- `LOG_LEVEL`
- `LOCAL_MULTI_AGENT_SYSTEM_PROMPT`
- `HOST`
- `PORT`

Point Warp's local multi-agent server URL at:

```text
http://127.0.0.1:8787
```

## Autocomplete prompt evals

With the local service running, iterate on autocomplete prompt/context behavior with:

```sh
cargo run -p local_multi_agent_service --bin warp-local-autocomplete-eval -- --service-url http://127.0.0.1:8787
```

The eval harness calls `/ai/local-command-autocomplete` and requests model
`auto-autocomplete`, so it uses the model alias configured in Warp settings.
Useful options:

```sh
cargo run -p local_multi_agent_service --bin warp-local-autocomplete-eval -- \
  --prompt-file /tmp/autocomplete-prompt.txt \
  --case filename \
  --repeat 3 \
  --json
```

For no-cloud GraphQL calls, run Warp or the CLI with:

```sh
WARP_NO_CLOUD=1 WARP_SERVER_ROOT_URL=http://127.0.0.1:8787
```

## Docker

Build from the repository root:

```sh
docker build -t warp-local-multi-agent -f crates/local_multi_agent_service/Dockerfile .
```

Run with persistent SQLite state:

```sh
docker volume create warp-local-multi-agent-data

docker run --rm \
  -p 8787:8787 \
  -v warp-local-multi-agent-data:/data \
  -e OPENAI_API_KEY="$OPENAI_API_KEY" \
  -e OPENAI_BASE_URL="${OPENAI_BASE_URL:-http://host.docker.internal:11434/v1}" \
  warp-local-multi-agent
```

## Behavior

The service preserves the local Node sidecar protocol: `/health`, `/ai/multi-agent`, `/ai/passive-suggestions`, `/graphql/v2`, and shared-session redirects. It stores compatible provider chat transcripts in SQLite, reloads them after restart, and rewrites conversation state when `/compact` summarizes prior messages.

Supported local tool-call names are `read_files`, `file_glob`, `grep`, `search_codebase`, `run_shell_command`, `apply_file_diffs`, `suggest_plan`, `read_mcp_resource`, and `call_mcp_tool`. MCP tools are routed through `call_mcp_tool`; direct native MCP tool names are only accepted when the request context contains exactly one matching tool name.
