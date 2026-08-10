# 0005 — Bidirectional Link Protocol

Normative contract for satellite services (Vox, Memoria, Dicta, Excita) that
run standalone in any project and may optionally **link** to a Conduit instance
so their operator UI appears in Conduit's navigation, reverse-proxied through
Conduit's origin.

Spec [0004](0004-embedded-service-visual-consistency.md) is the visual
companion: this spec defines the wire; 0004 defines how those surfaces look
when Conduit renders them.

Current implementation anchors: `crates/conduit-api/src/linked_services.rs`
(Conduit-side handlers, proxy, panel resolution), `LinkedServiceKind` /
`LinkedServicePanel` in `conduit-provider::storage`, and Vox's Python
`LinkStore` / `ConduitLinkClient` / `/link` FastAPI surface in
`services/vox/app.py` — extracted per this spec into a shared module.

---

## Rules that must hold

Two invariants govern every implementation:

1. **Sync tokens are never stored raw.** Conduit stores only the SHA-256 hex of
   the minted token. The raw token is returned once to the peer and held only
   there. A leaked Conduit storage snapshot cannot be replayed against the
   peer.
2. **Reachability is not optional.** Conduit probes each link's health on
   startup and on link creation (any *change*), and the probe is a real HTTP
   request against the peer — never a short-circuit or mock. A link that fails
   the probe is marked unreachable but retained; the operator sees the state
   rather than a silently broken tab.

Every subsequent section below is subordinate to these two rules.

## Standalone posture

Every listed service ships as its own container and runs with **zero** Conduit
reachable. Linking is purely additive: it registers the peer with a Conduit,
exposes a panel through Conduit's proxy, and enables any service-specific side
channels (see [Extension points](#extension-points)). Each service handles its
own local auth for its own UI when unlinked.

## Identity

- **Peer id** — chosen by the peer, normalised by Conduit to
  `[a-z0-9-_]{1,128}`. Case-folded lowercase. Empty, over-long, or
  out-of-charset ids are rejected with `422`.
- **`LinkedServiceKind`** — one of `vox`, `memoria`, `instrumenta`, `excita`,
  `generic`. New named kinds are added when a service earns a stable typed
  fallback panel (see [Panel manifest](#panel-manifest)); `generic` is always
  available for anything without a typed kind.
- **One row per peer id.** Re-linking a peer id replaces the prior row rather
  than accumulating stale rows, so operator recovery is idempotent.
- **Multiple peers of the same kind** are supported — an operator may link two
  Memoria instances to one Conduit under distinct peer ids.

## Handshake

The service initiates:

```
POST /v1/linked-services
{
  "service_kind":  "memoria",
  "peer_id":       "memoria-prod",
  "peer_name":     "Memoria (prod)",
  "peer_base_url": "https://memoria.example",
  "panel": { "id": "memoria", "label": "Memory", "icon": "brain", "path": "/ui/" }
}
```

Conduit responds `201`:

```
{ "sync_token": "<opaque 256-bit base64url no-pad>" }
```

Conduit stores the row with `sync_token_hash = sha256_hex(sync_token)`. The raw
token is never persisted server-side and never returned again. The peer
persists `{conduit_url, peer_id, sync_token, panel, granted_at}` locally in a
`link.json` with file-owner-only permissions; a world-readable file is a hard
error at load (see Vox's `LinkStoreSecurityError` pattern).

Re-issuing `POST /v1/linked-services` for an existing `peer_id` returns `409`;
the peer must `DELETE` first and re-link (see [Revocation](#revocation)).

## Panel manifest

Every link carries a `LinkedServicePanel`:

| Field   | Constraint                                             |
|---------|--------------------------------------------------------|
| `id`    | non-empty, lowercased                                  |
| `label` | non-empty, operator-visible tab label                  |
| `icon`  | lowercased Lucide icon name (see spec 0004)            |
| `path`  | non-empty, MUST start with `/` — the peer's UI entry   |

Conduit iframes the panel at `/linked-services/{peer_id}{path}`.

**Fallback panels.** Rows written before the panel manifest existed have no
inline panel. For a typed `LinkedServiceKind` (`vox`, `memoria`,
`instrumenta`, `excita`) Conduit synthesises a stable fallback so the tab keeps
appearing. `Generic` has no fallback — a `Generic` row without a manifest is
filtered out of `GET /v1/linked-services`.

Panels appear in navigation only when the manifest resolves *and* the peer's
reachability state is not `unreachable` (see [Reachability](#reachability)).

## Reverse-proxy contract

```
ANY /linked-services/{peer_id}/{*rest}
   → {peer_base_url}/{rest}?{same query string}
```

**Request headers**: `authorization`, `host`, `content-length`, `connection`
are stripped before forwarding; all other headers pass through.

**Bodies**: streamed in both directions. Bodies of any size and content-type
are supported (uploads, SSE, chunked responses).

**Response headers**: `content-length` and `connection` are dropped.
`Location` is rewritten so the client stays inside the proxy:

| Redirect form                            | Rewritten to                                    |
|------------------------------------------|-------------------------------------------------|
| `/foo` (absolute-path)                   | `/linked-services/{peer_id}/foo`                |
| `https://peer.example/foo` (same-origin) | `/linked-services/{peer_id}/foo`                |
| `https://other.example/foo` (foreign)    | passed through untouched                        |

Same-origin is measured against `peer_base_url`'s scheme+host+port. All other
response headers pass through.

**Peer UI guidance.** Peer UIs SHOULD emit relative asset URLs and relative
form actions. Absolute same-origin URLs work via the `Location` rewrite for
redirects only; asset URLs must be relative or the proxy prefix must be
respected client-side.

## Reachability

Conduit issues `GET {peer_base_url}/link/health` in two situations:

1. On `POST /v1/linked-services` — before the create response returns, bounded
   by a short timeout (see below); result recorded on the new row.
2. On Conduit startup — one probe per stored link, in parallel; results
   recorded.

The probe never removes a row. The row carries:

- `reachability`: `"ok" | "unreachable" | "unknown"`
- `last_probed_at`: timestamp of the last probe attempt

Both fields are surfaced in `GET /v1/linked-services` so the frontend can
render unreachable state distinctly (dimmed tab, status pill) rather than a
broken iframe. Reachability transitions (`ok`→`unreachable`,
`unreachable`→`ok`) emit a log line naming the peer.

`last_seen` is a separate signal: any successful proxied request, any
sync-token-authenticated call from the peer, and a successful reachability
probe all bump `last_seen`.

**Timeout.** The create-time probe is bounded (a few seconds) so a slow peer
does not stall the create response beyond that bound. A timeout is a probe
failure, not an error to the caller — the create still returns `201` with
`reachability: "unreachable"`.

**Probe is real.** Conformance tests must exercise the probe over real HTTP;
no in-crate short-circuit.

## Revocation

Two paths, one endpoint:

- **Operator-delete** — `DELETE /v1/linked-services/{peer_id}` with operator
  credentials removes the row.
- **Peer-revoke** — `DELETE /v1/linked-services/{peer_id}` with
  `Authorization: Bearer {sync_token}` removes the row. The token is compared
  by hashing the presented bearer and matching against the stored hash. A
  presented value that happens to be the raw hash is rejected — it is not the
  token, and replaying the hash must not authenticate.

The peer, on local unlink, deletes its `link.json`, stops the sync loop, and
keeps serving standalone. It also issues a best-effort peer-revoke to Conduit
so the row is cleaned up, tolerating a non-2xx or network failure.

## Extension points

The generic protocol above is the floor. Service-specific side channels layer
on top of it, keyed by `service_kind`:

- **Vox** — roster sync (Conduit posts speaker changes to the peer,
  authenticated by the peer's sync token).
- **Memoria** — memory/MCP surface — orthogonal to the panel, not part of this
  spec.
- **Dicta** — utterance transform surface consumed by Conduit's pipeline.
- **Excita** — wake-word event surface.

Side channels reuse the same sync-token trust and the same peer identity; they
do not require a second handshake. Their contracts live in each service's own
spec.

## Versioning

The protocol version is implicit in the panel manifest shape and the endpoint
paths. Additive changes (new optional `LinkedServicePanel` field, new
`LinkedServiceKind` variant, new optional response field) do not break
existing peers thanks to serde defaults. Breaking changes (removed field,
renamed endpoint) require a coordinated rollout and a spec revision.

Deprecated shim: `POST /v1/vox/links` is retained temporarily to keep
un-upgraded Vox peers linking during rollout; it is removed once all Vox
callers use `/v1/linked-services`.

## Endpoint summary

**Conduit**

| Method | Path                              | Auth                | Purpose                         |
|--------|-----------------------------------|---------------------|---------------------------------|
| POST   | `/v1/linked-services`             | none (peer-initiated) | Create link, return `sync_token` |
| GET    | `/v1/linked-services`             | operator            | List links + panels + reachability |
| DELETE | `/v1/linked-services/{peer_id}`   | operator OR bearer  | Operator-delete or peer-revoke  |
| ANY    | `/linked-services/{peer_id}/{*}`  | operator            | Reverse proxy to peer           |

**Service (via the shared link module)**

| Method | Path            | Purpose                                              |
|--------|-----------------|------------------------------------------------------|
| GET    | `/link`         | Current `LinkStatus`                                 |
| POST   | `/link`         | Create link with a Conduit                           |
| DELETE | `/link`         | Locally unlink; best-effort peer-revoke to Conduit   |
| GET    | `/link/health`  | 200 when service is up; consumed by reachability probe |
| GET    | `{panel.path}`  | The UI Conduit iframes                               |

## Non-goals

- Multi-Conduit federation (a peer linked to more than one Conduit at once).
- Automatic discovery / zeroconf — linking stays operator-initiated.
- Cross-origin absolute URL rewriting in proxied responses.
- Per-user ACLs inside a linked service — each service handles its own local
  auth.
- Any change to Conduit's audio pipeline (STT/TTS/wake). This spec covers
  linking only.
- Migrating the in-Conduit Forma rule engine (`frontend/src/forma/`) to Dicta —
  Dicta is a separate standalone service; the in-Conduit rule engine is
  unrelated.

## Verification

- The conformance test suite (`crates/conduit-api/tests/link_protocol_conformance.rs`,
  landed in the follow-up ticket) is the authoritative check. Every rule in
  this document must have at least one asserting case there.
- Every listed service must pass that suite as a client — the shared Python
  `link` module is the reference implementation.
- Reachability probing must be exercised over real HTTP against a live-then-
  dead peer, not mocked.
