# 0006 — `vox.roster` side channel

Concrete side channel layered on the [0005 §Side channels](0005-link-protocol.md#side-channels) contract. Retrofits the existing Vox roster pull (`HttpConduitSpeakerClient` in `services/vox/app.py`, Conduit's `/v1/speakers` endpoint) onto the common shape so it earns a capability name in the handshake manifest and follows the base rules.

Anchors: `services/vox/app.py::HttpConduitSpeakerClient` (peer-side pull), `crates/conduit-api/src/speakers.rs` (Conduit endpoint).

---

## Capability name

`vox.roster` — declared in the peer's `capabilities` array at handshake time (0005 §Handshake).

## Direction

**Peer → Conduit pull.** Confirmed: the peer already holds the `sync_token`, the roster is small, and pull semantics match the current Vox loop that reconciles labels every 300 s. A push variant would require an authenticated Conduit→peer channel; that is worth avoiding for a channel this simple.

## Endpoint

`GET /v1/speakers` on Conduit. **Keep the current path** rather than moving it under `/v1/linked-services/{peer_id}/roster`. Two reasons:

1. Roster is a **shared resource** across all linked Vox peers — every Vox pulls the same list. Nesting under a peer id would suggest per-peer rosters, which is not what Conduit stores.
2. The endpoint already exists and is stable; a rename is churn with no upside.

The base-spec rule that peer→Conduit calls authenticate with `Bearer {sync_token}` still holds, so the auth story is unchanged.

## Auth

`Authorization: Bearer {sync_token}` (peer→Conduit direction, per 0005 §Side channels).

## JSON shape

Response body:

```
[
  { "id": "<uuid>", "name": "<label>" },
  ...
]
```

**Keep today's minimal shape.** Do not add labels/enrollment counts on this channel: (a) the current consumer (`Syncer` in Vox) uses only `id` and `name`, (b) enrollment counts are per-peer state that Conduit does not own, and (c) the frontend already renders richer roster views over `/v1/speakers` when it needs them — this channel is a machine-readable subset for Vox reconciliation.

Empty roster returns `[]`, not `204`.

## Delta vs. full sync

**Full list per pull, with `ETag` + `304`.** Today's `Syncer` re-materialises the full roster every 300 s and reconciles by id — that stays. The optimisation is bandwidth-only:

- Conduit computes an `ETag` from the deterministic JSON body (SHA-256 hex of the sorted-by-id serialisation is fine).
- Peer sends `If-None-Match: <etag>` on subsequent pulls; Conduit returns `304 Not Modified` when unchanged.

An operator with tens of speakers pulling every 5 minutes is not the bottleneck. ETag is a small nicety that also lets us prove the payload is stable.

Explicitly NOT doing delta (`?since=<opaque_cursor>`) — a full list on 304-miss is O(roster size) which is small, and delta means Conduit tracks per-peer sync state we currently don't.

## Frequency / trigger

**Peer polls on a schedule.** Keep the existing 300 s interval and exponential backoff on error (per `Syncer.max_backoff_seconds = 900.0`). Roster changes are operator-driven — someone types a name — so 5-minute lag is acceptable.

**No SSE push in this revision.** An SSE invalidation channel would be a Conduit→peer channel authenticated by `peer_token`, and 0005 §Side channels forbids side-channel failures from flipping `reachability`. The current polling loop already tolerates a Conduit outage cleanly (backoff + retry); adding push would double the failure surface for a use case that does not need low latency. Left as fog for a future revision.

## Failure semantics

Per 0005 §Side channels: log-and-retry, MUST NOT flip base `reachability`. Vox's `Syncer` already does this — non-2xx and connection errors trigger exponential backoff up to 900 s, and the base link continues to serve. No change needed beyond ensuring the log line names both the peer id and `vox.roster` so multi-service logs stay legible.

## Retrofit scope (implementation ticket)

- Ensure Vox declares `vox.roster` in its handshake `capabilities` array (lands with the Python extraction, #171, once handshake carries `capabilities`).
- Add ETag support to Conduit's `/v1/speakers` handler; add `If-None-Match` handling to `HttpConduitSpeakerClient`.
- Add "peer=<peer_id> capability=vox.roster" prefix to Vox roster sync logs.
- Conformance-test the ETag round-trip against a real HTTP server per 0005 §Verification.

## Non-goals

- Any change to `/v1/speakers`' response shape (still `[{id, name}, ...]`).
- Diarization or per-peer roster views.
- SSE invalidation push (deferred; see above).
