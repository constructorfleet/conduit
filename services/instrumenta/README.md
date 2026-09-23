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

- **HTTP** upstreams are connected via the SDK's streamable-HTTP client.
- **stdio** upstreams are spawned via the SDK's stdio client transport.

Both are then driven by the same supervisor, one task per upstream, with
capped exponential-backoff autorestart. Only the transport differs; the
lifecycle does not. Forwarding tools are registered the first time a
connection *succeeds* — including a retry after a lost boot race — so a
transient failure never permanently hides them. Per-upstream reachability is
reported on `/upstreams`, never on `/health`.

An upstream that goes away has its tools removed from `tools/list` until it
reconnects, so a model never picks a tool whose upstream is gone. The same
poll keeps the set current: an upstream that adds, removes or renames a tool
while staying reachable has its forwarders replaced, rather than advertising
the set it had when it was first attached.

The poll is every 5s for stdio and every 30s for HTTP — a stdio child is a
local process this service owns, while an HTTP upstream is someone else's
server and every poll is a request to it.

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
