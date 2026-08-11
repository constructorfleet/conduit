# 0010 — Linked-service lifecycle & dev-ergonomics

Draft. Answers the questions in #169 as a proposal; every decision below is a **default** to react to, not a lock. Each carries a `Why:` line so a different call is easy to justify.

Anchors: `scripts/dev.sh` (today's Conduit-only dev loop), spec 0005 §Standalone posture (services boot without Conduit), `services/vox/app.py` and `services/memoria/app.py` (today's independent entrypoints).

---

## Startup order

**No boot ordering. Services stay independent.** No `dev.sh` that starts everything and auto-links. Operator (or dev) links manually.

**Why**: 0005 §Standalone posture is normative — services MUST boot with zero Conduit reachable. A "start everything and auto-link" script would create a dev-only implicit ordering that becomes reality when the shortcut leaks into someone's mental model. Manual linking is one HTTP POST per service, once, and matches how production works.

**How to apply**: `dev.sh` gains **launcher subcommands** (`dev.sh conduit`, `dev.sh vox`, `dev.sh memoria`, …), each starting exactly one thing. `dev.sh all` is deliberately absent. A README block shows the two-terminal flow: bring up Conduit, curl `POST /link` on each service.

## Health / readiness

**`GET /link/health` returns 200 the moment the HTTP server is listening.** Nothing more.

**Why**: 0005 §Reachability requires the probe be real HTTP and defines the probe as a base-protocol signal (side-channel health is separate). Loading models or waiting for a DB migration is a **service-specific** state — surfacing it through `/link/health` would fold service-specific concerns back into the base protocol, and 0005 explicitly separates them.

**How to apply**:

- Services expose their own richer readiness at `GET /ready` (or wherever they want) when they need it — that's not part of this spec.
- A service that wants to signal "not ready to serve requests yet" MAY return `503` on its own routes, but `/link/health` stays `200` because Conduit is still able to reach it. This keeps the reachability signal honest: the *link* is healthy; whether the service is fully warmed up is a separate concern.

## Config discovery

**Env var: `CONDUIT_LINK_URL`.** No config file, no auto-discovery.

**Why**: One convention, obviously named, easy to grep. A config-file layer would need a discovery order (env, file, defaults) and immediately means the shared module reads env — violating the "explicitness over magic" rule this module already enforces by making services pass a `LinkConfig` in.

**How to apply**:

- The **service's bootstrap** reads `CONDUIT_LINK_URL` (if set) and offers it as the default in the operator's link form. The shared module still receives it via `LinkConfig` — env-reading is a service concern, not a module concern.
- If not set, the operator supplies it at link time via the POST `/link` body. Env var is a convenience, not a requirement.
- `POST /link` body wins over env var if both present; the env var is only the *default*.

Not `CONDUIT_URL` (too generic — collides with unrelated integrations that also want a "the URL of Conduit"). Not `<SERVICE>_CONDUIT_URL` per service (duplicates the same value across N env vars for a machine that runs multiple services).

## Local URLs (dev-vs-prod handshake)

**Ports are what dev uses; Docker/DNS is what prod uses; the link protocol doesn't need to know the difference.**

**Why**: The handshake carries `peer_base_url` — whatever the operator types is what Conduit uses to reach the peer. In dev that's `http://localhost:8082`; in prod it's `http://memoria.internal:8080`. The reverse-proxy contract already handles this; nothing else needs to.

**How to apply**:

- `dev.sh` picks stable ports per service — Vox `:8081`, Memoria `:8082`, future Dicta `:8083`, Excita `:8084`, Instrumenta `:8085`, Forma `:8086`. Documented in the README.
- The `POST /link` body's `peer_base_url` is `http://localhost:<port>` in dev. Conduit-side reachability probes that URL over real HTTP — nothing dev-specific in the protocol.
- Reverse-proxy prefix (`/linked-services/{peer_id}/…`) already handles the "same origin from the browser's POV" concern; the initial handshake doesn't share that concern because it's peer→Conduit, not browser→Conduit.

## Hot reload / restart

**Both sides tolerate the other restarting. No coordination needed.**

**Why**: `link.json` persists on the peer (0005 §Handshake) and the DB row persists on Conduit (0005 §Handshake `sync_token_hash`). Neither side needs to re-handshake on the other's restart; the tokens are the same before and after.

**How to apply**: no code. This is a doc-only rule so devs don't reach for auto-reconnect logic — there is nothing to reconnect. If Conduit restarts, the peer's next authenticated call carries the same `sync_token` and works. If the peer restarts, its `LinkStore.load()` reads the same `link.json` and it works.

**Log line at boot** (both sides): `linked to <other> since <linked_at>` if a persisted link is found, `unlinked` otherwise. Makes it easy to see at a glance whether a restart broke anything.

## `dev.sh` shape (proposal)

```
scripts/dev.sh conduit          # starts conduit-api locally on :8080
scripts/dev.sh vox              # starts services/vox on :8081
scripts/dev.sh memoria          # starts services/memoria on :8082
scripts/dev.sh <service>        # each service the same way
scripts/dev.sh link <service>   # convenience: POST /link with sensible defaults
                                # from env (CONDUIT_LINK_URL + operator token)
```

The `dev.sh link <service>` helper is a **thin shell wrapper around curl**, not part of the module — it's just a shortcut that saves typing the same POST body twice a day. Every service still exposes its own `POST /link` and honours it identically.

## Non-goals

- Automatic reconnection / retry on Conduit outage (base link is stateless per-request; nothing to reconnect).
- Zero-config dev auto-link (rejected above — implicit ordering is a footgun).
- Cross-service dependency modelling (each service is its own boot; `dev.sh` doesn't know).
