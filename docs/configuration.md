# Configuration Reference

Conduit is configured with environment variables for server, authentication, and
storage concerns. Product provider configuration is saved as server-owned
Provider Definitions through the management API.

## Server

| Variable | Default | Description |
| --- | --- | --- |
| `CONDUIT_BIND` | `0.0.0.0:8080` | Service API listen address. |
| `CONDUIT_OPS_BIND` | `0.0.0.0:9090` | Ops API listen address for `/health`, `/ready`, and `/metrics`. |
| `CONDUIT_LOG` | `info` | `tracing_subscriber` filter used for structured JSON logs. |
| `CONDUIT_TURN_IDLE_TIMEOUT_SECS` | `60` | How long a turn may publish no progress events before cancellation as `idle_timeout`. `0` removes the bound. |

## Authentication

| Variable | Default | Description |
| --- | --- | --- |
| `CONDUIT_TOKENS` | unset | Path to a JSON token file. Required unless `CONDUIT_ALLOW_ANONYMOUS` is set. |
| `CONDUIT_ALLOW_ANONYMOUS` | unset | Set to `1`, `true`, or `yes` to serve the service API without tokens. Cannot be combined with `CONDUIT_TOKENS`. |

There is no open default. A server with neither variable set refuses to start.

Token file shape:

```json
{
  "devices": [
    {
      "token": "64-or-more-hex-characters-from-openssl-rand",
      "device": "kitchen",
      "pipelines": ["default"]
    }
  ],
  "management": [
    {
      "token": "another-generated-token",
      "name": "ui"
    }
  ]
}
```

Rules enforced at startup:

- tokens must be at least 32 characters
- the file must declare at least one token
- the same token cannot appear twice
- every device entry needs a `device` name
- every management entry needs a `name`
- on Unix, the token file must not be group- or world-readable

`pipelines` is optional for device tokens. Omit it to allow any pipeline; use an
empty list to allow none.

## Pipeline Storage

| Variable | Default | Description |
| --- | --- | --- |
| `CONDUIT_DATABASE_URL` | unset | PostgreSQL URL for pipeline storage. Takes precedence over `CONDUIT_PIPELINE_DIR` when the `postgres` feature is enabled. |
| `CONDUIT_DATA_DIR` | `$XDG_DATA_HOME/conduit` or `$HOME/.local/share/conduit` | Base directory for Conduit-managed local data. |
| `CONDUIT_PIPELINE_DIR` | `$CONDUIT_DATA_DIR/pipelines` | Directory for JSON pipeline files. Used when no database URL is configured. Set to `:memory:` only for disposable development storage. |
| `CONDUIT_PROVIDER_DIR` | `$CONDUIT_DATA_DIR/providers` | Directory for JSON Provider Definition files. Set to `:memory:` only for disposable development storage. |

If neither database nor pipeline directory is set, pipelines are stored as JSON
files in the default local data directory and survive API restarts. The server
uses memory only when `CONDUIT_PIPELINE_DIR=:memory:` is set, and logs a warning
for that disposable mode.

Provider Definitions use their own store. If `CONDUIT_PROVIDER_DIR` is unset,
definitions are stored as JSON files under the default local data directory and
survive API restarts. A server rebuilds the Runtime Provider Registry Snapshot
from those definitions during startup and after successful provider writes or
deletes.

The `conduit-api` crate enables PostgreSQL support by default. A
`--no-default-features` build refuses to start if `CONDUIT_DATABASE_URL` is set.

## Provider Definitions

The service exposes `GET /v1/catalog/providers` so the Operator Console can
render provider-specific creation forms from backend-owned component metadata.
Operators save Provider Definitions with stable ids, then select those ids from
pipeline graph nodes. Pipeline graphs store only the selected provider id on
each node; runtime component settings do not belong to graph nodes.

Product runtime providers are not configured from `CONDUIT_OPENAI_*`
environment variables. Use saved Provider Definitions for operator-managed
providers, or compile with `dev-providers` for the direct in-memory
development seam.

### Runtime Providers Built From Definitions

| Variant | Runtime provider | Endpoint |
| --- | --- | --- |
| `openai_llm` / `openai_stt` / `openai_tts` | `conduit-openai` | `base_url`, `http` or `https` |
| `wyoming_stt` / `wyoming_tts` | `conduit-wyoming` | `url`, `tcp://host:port` |
| `mcp_tool` | `conduit-mcp` | stdio, streamable HTTP, or SSE transport |

Every variant registers under its definition id, so a graph node naming the id
resolves to the provider that definition describes.

An MCP definition describes a *server*, which may advertise several tools, and
a graph tool node runs one tool. Each advertised tool is therefore registered as
`<definition id>.<tool name>`; a server advertising exactly one tool is also
registered under the definition id itself.

Discovering those tools needs the server to answer, but saving a definition
does not require it: discovery is given five seconds, and a server that does not
answer leaves the definition saved with no tools registered. Running a
reachability test on that definition rediscovers them.

## OpenTelemetry

| Variable | Default | Description |
| --- | --- | --- |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | unset | Enables OTLP/HTTP span export. |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | unset | Trace-specific OTLP/HTTP endpoint. Takes precedence for spans. |

When neither is set, Conduit still writes structured JSON logs and does not try
to connect to an OTLP collector.

## Test-Only Variables

| Variable | Used by | Description |
| --- | --- | --- |
| `CONDUIT_TEST_POSTGRES_URL` | `conduit-store` tests | PostgreSQL database URL for store conformance tests. Tests that need it skip themselves when unset. |
| `CONDUIT_REGENERATE_FIXTURES` | firmware protocol parity tests | Regenerates checked-in firmware protocol fixtures when intentionally updating the wire contract. |
