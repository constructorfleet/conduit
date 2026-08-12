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
| `INSTRUMENTA_BASE_URL` | `http://localhost:8085` | Advertised in link handshake |
| `INSTRUMENTA_SECRET_KEY` | (unset) | Fernet key for at-rest secret encryption |
| `INSTRUMENTA_API_KEY` | (unset) | Bearer token protecting mutating routes (added by later PRs) |

Instrumenta refuses to start if any encrypted secret exists in the backend
and `INSTRUMENTA_SECRET_KEY` is not set — misconfiguration surfaces at boot.

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
- Stdio upstream supervisor + layered `instrumenta-node` / `instrumenta-python` images.
- HTMX configuration UI.
- Audit log table + `/audit` viewer.
