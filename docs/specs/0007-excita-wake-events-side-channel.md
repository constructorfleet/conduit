# 0007 — `excita.wake-events` side channel

Concrete side channel layered on the [0005 §Side channels](0005-link-protocol.md#side-channels) contract. Excita has no code yet — this is green-field, but decisions land now so the reusable side-channels pattern isn't retrofit-only.

Excita's purpose: run wake-word detection off-box (a dedicated CPU-heavy service) and inform Conduit whenever an utterance should begin. Conduit today emits `WakeWordDetected` and `WakeWordRejected` from an in-process wake stage (`crates/conduit-wake`); this channel is how an out-of-process Excita produces those same signals.

---

## Capability name

`excita.wake-events` — declared in the peer's `capabilities` array at handshake time (0005 §Handshake).

## Direction

**Peer → Conduit push.** Excita is the detector; it decides when a wake fires. A Conduit→peer subscribe (Conduit opens SSE on Excita and reads events) is technically symmetric but wastes a persistent connection Conduit does not otherwise need — most Excita boxes will sit idle for minutes at a stretch. A short POST per detection is the smallest possible surface.

## Transport

**One `POST` per event.** No streaming.

- Wake events are discrete, rare (seconds-to-minutes apart in practice), and small (< 1 KB JSON).
- A long-lived SSE from Excita would need reconnect/backoff logic on both sides and would keep a socket open through Conduit restarts, which currently need no coordination with satellites.
- `POST /v1/wake-events` on Conduit, one event per body. If Excita ever wants to batch (e.g. after a network partition heals), that's an additive `POST /v1/wake-events/batch` for later, not this revision.

## Endpoint on Conduit

```
POST /v1/wake-events
```

Not nested under `/v1/linked-services/{peer_id}/…` — same reason as `vox.roster`'s endpoint decision: this is a **shared inbox** across all Excita peers linked to one Conduit, not a per-peer resource. Peer identity comes from the `sync_token` on the request; Conduit resolves it back to the row.

## Auth

`Authorization: Bearer {sync_token}` (peer→Conduit direction per 0005 §Side channels). Conduit hashes-and-matches to identify which linked-service row is speaking; the `peer_id` field of the event body is a redundancy check, not the source of trust.

## Event JSON shape

```json
{
  "event_type": "detected",
  "peer_id": "excita-kitchen",
  "phrase": "hey jarvis",
  "confidence": 0.87,
  "detected_at": "2026-08-10T14:22:03.412Z",
  "source_device": "kitchen-satellite",
  "audio_clip_ref": null
}
```

Fields:

- **`event_type`** (REQUIRED, `"detected" | "rejected"`) — mirrors Conduit's existing `WakeWordDetected` / `WakeWordRejected` split, so an operator sees the same evidence for a near-miss as they do today from the in-process stage.
- **`peer_id`** (REQUIRED) — cross-check against the `sync_token` resolution. Mismatch is `403`.
- **`phrase`** (REQUIRED) — configured name of the wake phrase, matching Conduit's existing `phrase` field on `WakeWordDetected`.
- **`confidence`** (REQUIRED, `f32` in `0.0..=1.0`) — detector confidence; same range as in-process events.
- **`detected_at`** (REQUIRED, RFC3339) — Excita's timestamp; used for logs and jitter measurement, not for turn ordering (Conduit stamps its own receive time).
- **`source_device`** (OPTIONAL) — device id / label if Excita knows which satellite fed the audio. Conduit uses this to route the utterance to the right pipeline; absent means "route by peer's default binding".
- **`audio_clip_ref`** (OPTIONAL) — opaque pointer to the pre-roll clip if Excita retains one. Deliberately opaque here; retrieval mechanism (if any) is a separate future channel, not this one.

Response: `202 Accepted` on success (empty body), `403` on `peer_id` mismatch, `422` on shape violation.

## Delivery guarantees

**At-least-once, peer-driven retry.** Excita retries a non-2xx (or connection error) with exponential backoff (start 1 s, cap at 60 s) until it gets a 2xx. Conduit MUST treat repeated events as idempotent — a wake that arrives twice is one wake being retried, not two wakes.

Idempotency implementation: Excita SHOULD include `Idempotency-Key: <opaque>` on retries (RFC-style single-use token per detection). Conduit remembers the last 1024 keys per peer in memory; a repeat within that window is `202` with no side effect. Outside the window, at-least-once semantics apply and the operator sees a duplicate — this is an acceptable failure mode for a channel whose downside is one extra utterance.

At-most-once (fire-and-forget) was rejected: a wake dropped by a network hiccup is a user typing the phrase again, which is worse UX than a rare duplicate.

## Timeouts

Excita times out its POST at 3 s. Conduit MUST respond in well under that — this endpoint is a receive-and-enqueue, not a synchronous handoff to the pipeline. The wake event triggers pipeline work, but the response fires before that work starts.

## Failure semantics

Per 0005 §Side channels: log-and-retry, MUST NOT flip base `reachability`. Excita's `Bearer sync_token` failing (401) is a link-level problem for a human to fix; log with `peer=<peer_id> capability=excita.wake-events` prefix and stop retrying that specific request (it won't recover on its own). All other errors retry.

## Implementation scope (future ticket)

- Add `POST /v1/wake-events` handler on Conduit; resolve `sync_token` → peer, cross-check `peer_id`, dispatch a `WakeWordDetected` / `WakeWordRejected` event on the bus keyed by `source_device`'s pipeline.
- Idempotency-key ring buffer (per peer, 1024 keys).
- Client SDK stub in `packages/conduit-link` (or a companion `conduit-link-excita`) so a future Excita implementation isn't hand-rolling this — shape TBD when Excita is scaffolded.
- Conformance test over real HTTP: POST/retry/dedup all covered.

## Non-goals

- Excita service scaffolding itself (separate future effort).
- Audio clip transport (opaque ref only in this revision).
- Server-push variant (rejected above; revisit only if a class of Excita deployments emerges that can't POST out).
- Batch POST (additive; not needed until measured).
