# 0008 — `dicta.transform` side channel

Concrete side channel layered on the [0005 §Side channels](0005-link-protocol.md#side-channels) contract. Dicta has no code yet — green-field design. Spec 0005 §Extension points describes Dicta as an *"utterance transform surface consumed by Conduit's pipeline"*, i.e. an out-of-process peer that plays the role currently held by the in-process `UtteranceTransform` trait in `crates/conduit-transform`.

This channel is on **Conduit's pipeline hot path**: it runs between reasoning and TTS on every turn, so latency budget and idempotency shape the whole design.

---

## Capability name

`dicta.transform` — declared in the peer's `capabilities` array at handshake time (0005 §Handshake). Reserved by 0005 to `LinkedServiceKind::Dicta`.

## Direction

**Conduit → peer request/response.** Confirmed by the transform's role — Conduit has an utterance in hand and needs a rewritten one to feed TTS. A peer→Conduit variant makes no sense here.

Since this is a Conduit→peer direction, per 0005 §Side channels the request MUST be authenticated with `Authorization: Bearer {peer_token}` — the token minted by Dicta at handshake and stored (as hash) on the Conduit row.

## Transport

**Single HTTP request/response with the full transform in the response body.** No streaming in this revision.

- The unit of work is one utterance segment (typically < 1 KB). Streaming a 1 KB body via SSE saves nothing and costs a persistent connection.
- Reasoning already streams into `UtteranceTransform` today, but the transform boundary is per-segment: each segment goes in whole and comes out whole. That segment boundary carries forward to Dicta.
- If a Dicta implementation ever emerges whose transform is genuinely incremental (e.g. an LLM-based rewriter that streams tokens), that's an SSE-response variant of this endpoint — additive, not a rewrite of this spec.

Endpoint on the peer:

```
POST {peer_base_url}/transform
```

## Request JSON shape

```json
{
  "segment": "…the utterance to transform…",
  "context": {
    "speaker_id": "…optional…",
    "session_id": "…optional…",
    "turn_id": "…optional…",
    "prior_turns": ["…optional, most-recent-last, small…"]
  }
}
```

- **`segment`** (REQUIRED) — the text to rewrite. Same input the in-process `UtteranceTransform::transform` receives today.
- **`context`** (OPTIONAL object) — every field OPTIONAL:
  - `speaker_id` — the identified speaker, if any, so a Dicta rewrite can personalise.
  - `session_id` / `turn_id` — conversation/turn identity, useful for a Dicta that keeps its own memory.
  - `prior_turns` — bounded (recommendation: last 4, and each MAY be truncated by Conduit) so a Dicta that keys off the recent thread has enough to work with without Conduit forwarding the whole history on every call.

Every field a Dicta doesn't want it MUST ignore. Additive fields on this shape are non-breaking (serde defaults on both sides).

## Response JSON shape

```json
{
  "segment": "…the rewritten utterance…"
}
```

Single transformed string. **Reject** richer shapes (multiple candidates, scored alternatives) — Conduit's pipeline consumes exactly one string, and a Dicta that returns three needs to pick one anyway. If a scoring layer ever earns its place, that's an additive `candidates` field, not a replacement.

A Dicta that wants to leave the segment unchanged returns the same string. Empty string is allowed and means *"say nothing"* — matches the in-process contract where a rule can strip an entire segment.

## Auth

`Authorization: Bearer {peer_token}` (Conduit→peer direction per 0005 §Side channels). Dicta hashes-and-matches against its stored `peer_token_hash`.

## Idempotency / retries

**Idempotent by request contents.** A Dicta implementation MUST make the same `(segment, context)` produce the same output for a small window (a few seconds), so Conduit can retry a lost connection without duplicating side effects. This is a light requirement — most transforms are naturally pure — but stated explicitly because it makes the retry policy safe.

Conduit retries on connection error or 5xx with a single retry, 250 ms backoff. No `Idempotency-Key` header is required — the transform is pure enough that dedup on the peer side is optional. A peer that runs a non-idempotent transform (e.g. one that logs each call as billed) SHOULD implement its own dedup keyed by a hash of the request body.

Explicitly NOT retrying 4xx: those are shape violations or auth failures, a human problem to fix, not a network hiccup.

## Timeouts / SLO

Pipeline hot path: this call runs synchronously between reasoning and TTS, so its latency lands directly in the user-perceived response time.

- Conduit deadline: **500 ms** per call, including the one retry above. A Dicta that misses this budget is treated as **absent** — Conduit falls back to the segment as-is and continues the pipeline.
- Peer SHOULD respond in < 200 ms P99. A Dicta running an LLM will need to keep the model warm or hedge to a cheaper fallback.

The fallback-to-passthrough on timeout is the important part: a slow Dicta MUST NOT stall a user's turn. This is why the "return segment unchanged" behaviour is a first-class case above.

## Failure semantics

Per 0005 §Side channels: side-channel failure MUST NOT flip base `reachability`. A Dicta that returns 500s on every call is broken *for its capability* but its base link stays "reachable" — the operator sees a healthy tab and a specific `dicta.transform: unavailable` counter on that peer's row (that counter lands with the implementation ticket).

Log with `peer=<peer_id> capability=dicta.transform` prefix. Every timeout is a log line.

## Implementation scope (future ticket)

- New provider variant `TransformVariant::Dicta { peer_id }` in `crates/conduit-provider/src/storage/transform.rs`, resolved at build time against the `LinkedService` row for its `peer_base_url` and `peer_token`.
- A `DictaTransform` impl of `UtteranceTransform` in a new `crates/conduit-dicta` (or added to `conduit-transform` — TBD when Dicta ships).
- 500 ms deadline + one retry policy + fallback-to-passthrough.
- `dicta.transform` capability advertised in the row; if a peer's row doesn't declare it, `TransformVariant::Dicta { peer_id }` cannot resolve at pipeline build.
- Conformance test over real HTTP: happy path, timeout falls back, 5xx retries once then falls back.

## Non-goals

- Dicta service scaffolding itself (separate future effort).
- Any change to Conduit's audio pipeline outside the `UtteranceTransform` plug point.
- Streaming response body (deferred; additive).
- Multiple-candidate response (deferred; additive).
- A caching layer on Conduit for previously seen segments — Dicta owns its own cache if it wants one.
