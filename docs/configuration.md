# Configuration Reference

Conduit is configured with environment variables. Unset provider variables mean
the provider is not registered; partial provider configuration is treated as an
error when it would otherwise fail mid-turn.

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

If neither database nor pipeline directory is set, pipelines are stored as JSON
files in the default local data directory and survive API restarts. The server
uses memory only when `CONDUIT_PIPELINE_DIR=:memory:` is set, and logs a warning
for that disposable mode.

The `conduit-api` crate enables PostgreSQL support by default. A
`--no-default-features` build refuses to start if `CONDUIT_DATABASE_URL` is set.

## OpenAI-Compatible Providers

| Variable | Default | Description |
| --- | --- | --- |
| `CONDUIT_OPENAI_BASE_URL` | `https://api.openai.com/v1` when a provider is configured by key | Base URL for an OpenAI-compatible server. |
| `CONDUIT_OPENAI_API_KEY` | unset | Bearer token for the OpenAI-compatible server. Local servers often do not need one. |
| `CONDUIT_OPENAI_NAME` | `openai` | Provider registry name used by pipeline nodes. |
| `CONDUIT_OPENAI_READ_TIMEOUT_SECS` | `60` | How long the server may go silent while a response body is in progress. `0` disables this provider-level read timeout. |
| `CONDUIT_OPENAI_STT_MODEL` | unset | Registers `OpenAiStt` using this transcription model. |
| `CONDUIT_OPENAI_TTS_MODEL` | unset | Registers `OpenAiTts` using this speech model. |

Setting `CONDUIT_OPENAI_BASE_URL` or `CONDUIT_OPENAI_API_KEY` registers an
OpenAI-compatible language model provider. The language model node in the
pipeline supplies the model name in node config.

The service exposes `GET /v1/pipeline-components` so the Operator Console can
render provider-specific configuration forms from component schemas. Operators
configure provider instances with stable IDs on the Providers page, then select
those IDs from pipeline nodes.

Provider instance definitions are still stored by the Operator Console. When a
supported inline provider is assigned to a pipeline node, the console embeds the
runtime component and settings into that saved graph node so the server can
construct the provider while preparing the pipeline. Wyoming TTS is currently
supported this way: a node with provider id `piper` and config
`component=wyoming.tts`, `url=tcp://host:port`, and optional `voice` resolves as
a runnable TTS provider even when no process environment provider named `piper`
was registered at startup. Unsupported provider components remain UI
definitions until Conduit grows a server-side provider store and runtime plugin
loader.

Setting an STT or TTS model without a base URL or API key is an error. The
server refuses to start rather than registering a speech stage with no server.

Example local configuration:

```sh
CONDUIT_OPENAI_BASE_URL=http://localhost:8000/v1 \
CONDUIT_OPENAI_NAME=local \
CONDUIT_OPENAI_STT_MODEL=Systran/faster-whisper-small \
CONDUIT_OPENAI_TTS_MODEL=piper \
cargo run -p conduit-api
```

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
