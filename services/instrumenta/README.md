# Conduit Instrumenta

Reference tool service. Standalone-capable FastAPI app that will aggregate
upstream MCP servers and ship a small built-in tool set behind a
configuration UI. Registers with Conduit as `LinkedServiceKind=instrumenta`.

This is the **v1 skeleton** — link endpoints, SQLite backend, Fernet-encrypted
secret plumbing, empty `/mcp` streamable-HTTP endpoint. The aggregator,
built-in tools, stdio supervisor, UI, and audit log land in follow-up PRs
under wayfinder map [#199](https://github.com/constructorfleet/conduit/issues/199).

## Environment

| Variable | Default | Purpose |
| --- | --- | --- |
| `INSTRUMENTA_DATA_DIR` | `/data` | Where SQLite + link records live |
| `INSTRUMENTA_BACKEND` | `sqlite` | Backend selector (`postgres` planned) |
| `INSTRUMENTA_BASE_URL` | `http://localhost:8085` | Advertised in link handshake, and the base of `mcp_url` |
| `INSTRUMENTA_SECRET_KEY` | (unset) | Fernet key for at-rest secret encryption |
| `INSTRUMENTA_API_KEY` | (unset) | Bearer token protecting mutating routes (added by later PRs) |

Instrumenta refuses to start if any encrypted secret exists in the backend
and `INSTRUMENTA_SECRET_KEY` is not set — misconfiguration surfaces at boot.

## Linking to Conduit

The handshake advertises one canonical `mcp_url` — `{INSTRUMENTA_BASE_URL}/mcp/`,
the streamable-HTTP endpoint — alongside `peer_base_url` and the panel, so
Conduit connects as a stock MCP client without knowing how the transport is
mounted (User Story 30 of
[#198](https://github.com/constructorfleet/conduit/issues/198)). The path is
defined once as `MCP_PATH` in `app.py` and used both to mount the transport
and to build the advertised URL, so the two cannot drift.

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

  A child that dies has its tools removed from `tools/list` until it
  reconnects, so a model never picks a tool whose upstream is gone. This does
  **not** yet apply to HTTP upstreams: they are attached once at boot and
  never re-probed, so one that goes away later stays advertised and fails at
  call time.

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
