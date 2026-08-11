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

1. **Handshake tokens are never stored raw.** This applies symmetrically to
   BOTH `sync_token` (Conduit→peer direction) and `peer_token` (peer→Conduit
   direction). Each side stores only the SHA-256 hex of the token it received;
   the raw value crosses the wire exactly once, at the handshake, and is held
   only by the party that will present it. A leaked storage snapshot on
   either side cannot be replayed against the other.
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
  `dicta`, `forma`, `generic`. New named kinds are added when a service earns
  a stable typed fallback panel (see [Panel manifest](#panel-manifest));
  `generic` is always available for anything without a typed kind.
- **`sync_token`** — 256-bit base64url-no-pad, minted by Conduit, presented
  by the peer to authenticate peer→Conduit calls. Stored on both sides only
  as its SHA-256 hex (see [Rules that must hold](#rules-that-must-hold)).
- **`peer_token`** — 256-bit base64url-no-pad, minted by the peer, presented
  by Conduit to authenticate Conduit→peer side-channel calls. Same
  hash-only-stored invariant. See [Side channels](#side-channels) for what
  this authenticates.
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
  "panel":         { "id": "memoria", "label": "Memory", "icon": "brain", "path": "/ui/" },
  "peer_token":    "<opaque 256-bit base64url no-pad>",
  "capabilities":  ["memoria.mcp"]
}
```

`peer_token` is REQUIRED (256-bit base64url-no-pad, minted by the peer).
`capabilities` is REQUIRED but MAY be an empty array; each entry is a
side-channel capability name (see [Side channels](#side-channels)). Unknown
capability names are stored verbatim — Conduit does not enforce a registry.

Conduit responds `201`:

```
{ "sync_token": "<opaque 256-bit base64url no-pad>" }
```

The `201` acknowledges that BOTH `peer_token` and `capabilities` were stored.
Conduit stores the row with `sync_token_hash = sha256_hex(sync_token)` and
`peer_token_hash = sha256_hex(peer_token)`; the raw `sync_token` is returned
once and never persisted server-side, and the raw `peer_token` is discarded
after the hash is written. The peer persists
`{conduit_url, peer_id, sync_token, peer_token, panel, granted_at}` locally in
a `link.json` with file-owner-only permissions; a world-readable file is a
hard error at load (see Vox's `LinkStoreSecurityError` pattern).

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
`instrumenta`, `excita`, `dicta`, `forma`) Conduit synthesises a stable
fallback so the tab keeps appearing. `Generic` has no fallback — a `Generic`
row without a manifest is filtered out of `GET /v1/linked-services`.

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

**Side-channel health is NOT part of `reachability`.** A failing side channel
(see [Side channels](#side-channels)) MUST NOT flip `reachability` to
`unreachable`. That field is the base-protocol health signal only, so a broken
extension never hides a service's UI tab from the operator.

## Side channels

The generic protocol above is the floor. Service-specific side channels layer
on top of it, keyed by `service_kind` and advertised in `capabilities` at
handshake time. This section is normative for every side channel; individual
channel specs (e.g. `vox.roster`, `dicta.transform`, `excita.wake-events`)
refine the shape per capability.

**Direction.** Side channels are dual-mode. A capability MAY be peer→Conduit
(the peer calls Conduit with its `sync_token`), Conduit→peer (Conduit calls
the peer with its `peer_token`), or both. Each per-capability spec MUST
declare which direction(s) it uses.

**Transport.** The baseline is HTTP+JSON — one request, one JSON response,
using the standard status codes. A capability MAY upgrade to
Server-Sent Events (SSE) for streaming when latency or push semantics require
it; long-polling and websockets are NOT part of the baseline. Whatever the
transport, requests and responses ride the same TLS/origin as the base link
endpoint on that side.

**Authentication.**

- **Peer→Conduit** capabilities MUST authenticate with
  `Authorization: Bearer {sync_token}`. Conduit matches by hashing the
  presented bearer against `sync_token_hash`.
- **Conduit→peer** capabilities MUST authenticate with
  `Authorization: Bearer {peer_token}`. The peer matches by hashing the
  presented bearer against `peer_token_hash`.
- The rule in [Rules that must hold](#rules-that-must-hold) applies: a
  presented value that happens to equal the stored hash is rejected. Replaying
  the hash must not authenticate.

**Capability manifest.** The `capabilities` array in the handshake is the
authoritative list of channels this peer will speak. A capability that is not
declared at handshake is not usable — either side MAY reject the call. To add
a capability after linking, the operator MUST unlink and re-link. Capability
names are dotted-lowercase strings; the base spec RESERVES the top-level
prefixes for `LinkedServiceKind` values (`vox.*`, `memoria.*`, `dicta.*`,
`excita.*`, `instrumenta.*`, `forma.*`).

**Failure semantics.** A side-channel failure (timeout, non-2xx, connection
error) MUST be logged with the peer id and capability name and retried per
the capability's own retry policy. It MUST NOT change the base row's
`reachability` field. This keeps a broken extension from disabling the tab.

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

Concrete side channels layered on top of the [Side channels](#side-channels)
contract, keyed by `service_kind`:

- **Vox** — `vox.roster`: roster sync (peer→Conduit today; retrofit ticket in
  flight).
- **Memoria** — memory/MCP surface — orthogonal to the panel, not part of this
  spec.
- **Dicta** — `dicta.transform`: utterance transform surface consumed by
  Conduit's pipeline (Conduit→peer).
- **Excita** — `excita.wake-events`: wake-word event surface.

Every channel above reuses base identity and the tokens issued at handshake;
none require a second handshake. Their per-capability contracts live in each
service's own spec.

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
| GET    | `/v1/linked-services`             | operator            | List links + panels + reachability + `capabilities` |
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
- Building the Forma standalone service itself is out of scope for THIS spec,
  but `forma` is a first-class `LinkedServiceKind` so a future standalone
  Forma will link like any other service. (The former non-goal — migrating
  the in-Conduit rule engine to Dicta — is unrelated and remains excluded.)

## Verification

- The conformance test suite (`crates/conduit-api/tests/link_protocol_conformance.rs`,
  landed in the follow-up ticket) is the authoritative check. Every rule in
  this document must have at least one asserting case there.
- Every listed service must pass that suite as a client — the shared Python
  `link` module is the reference implementation.
- Reachability probing must be exercised over real HTTP against a live-then-
  dead peer, not mocked.
