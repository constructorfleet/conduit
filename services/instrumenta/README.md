# Conduit Instrumenta

Reference tool service. Standalone-capable FastAPI app that will aggregate
upstream MCP servers and ship a small built-in tool set behind a
configuration UI. Registers with Conduit as `LinkedServiceKind=instrumenta`.

This service supports SQLite and PostgreSQL backends, with Fernet-encrypted
secret plumbing, streamable-HTTP and SSE MCP endpoints, an aggregator,
built-in tools, stdio supervision, UI, and audit log.

## Environment

| Variable | Default | Purpose |
| --- | --- | --- |
| `INSTRUMENTA_DATA_DIR` | `/data` | Where the SQLite database and link records live when using SQLite; PostgreSQL uses the configured server/database |
| `INSTRUMENTA_BACKEND` | `sqlite` | Backend selector (`sqlite` or `postgres`) |
| `INSTRUMENTA_DATABASE_URL` | `postgresql://postgres:postgres@postgres:5432/postgres` when `INSTRUMENTA_BACKEND=postgres`; otherwise unset | PostgreSQL connection URL |
| `INSTRUMENTA_BASE_URL` | `http://localhost:8085` | Advertised in link handshake |
| `INSTRUMENTA_SECRET_KEY` | (unset) | Fernet key for at-rest secret encryption |
| `INSTRUMENTA_API_KEY` | (unset) | Bearer token protecting mutating routes (added by later PRs) |

Instrumenta refuses to start if any encrypted secret exists in the backend
and `INSTRUMENTA_SECRET_KEY` is not set — misconfiguration surfaces at boot.

## MCP transports

The merged MCP surface is available at `/mcp/http/` (streamable HTTP) and
`/mcp/sse/` (SSE); the legacy `/mcp/` streamable-HTTP alias remains available.
Both transports are enabled by default. `GET /transports` reports their
persisted state and `PUT /transports/{http|sse}` with `{"enabled": false}`
disables one immediately; stale flags for transports removed from the build are
removed when the backend starts.

## Upstream transports

Instrumenta aggregates two kinds of upstream MCP server, both re-exposed under
a `<server_name>.<tool_name>` prefix so nothing collides with the built-ins:

- **HTTP** upstreams are connected once at boot via the SDK's streamable-HTTP
  client.
- **stdio** upstreams are spawned and supervised via the SDK's stdio client
  transport. Each child is driven by one supervise task with capped
  exponential-backoff autorestart. Its forwarding tools are registered the
  first time a connection *succeeds* — including a retry after a lost boot
  race — so a transient child failure never permanently hides its tools.
  Per-upstream reachability is reported on `/upstreams`, never on `/health`.

Forwarded tools mirror the upstream tool's own parameter schema, so a model
calling them sees and validates the real arguments rather than an opaque bag.

## Running

```
uvicorn instrumenta.app:create_app --factory --host 0.0.0.0 --port 8085
```

## Tests

```
cd services && PYTHONPATH=. pytest instrumenta/test_app.py
```

## Follow-ups

- `scripts/dev.sh` integration on `:8085` (`--instrumenta-port`, `INSTRUMENTA_BASE_URL`).
- Aggregator (HTTP upstream MCP clients).
- Built-in tools: `http.fetch`, `time.now`, `math.eval`, `text.regex`.
- Layered `instrumenta-node` / `instrumenta-python` images so stdio upstreams
  can run node/python servers without bloating the base image.
- HTMX configuration UI.
- Audit log table + `/audit` viewer.
