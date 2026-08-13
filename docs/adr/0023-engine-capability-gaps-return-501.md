# Engine Capability Gaps Return HTTP 501

Excita's engine adapters are partial by design
([0020](0020-wake-engine-adapters-are-partial.md)): asking a
microWakeWord adapter to `load` a live detector, or a nanoWakeWord
adapter to `train`, is a request the server *cannot* fulfil, not a
request that was malformed. The HTTP surface translates
`NotSupportedError` into **HTTP 501 Not Implemented** with a structured
body:

```json
{
  "code": "engine_capability_missing",
  "engine": "microwakeword",
  "capability": "load",
  "message": "microWakeWord does not run live host-side detection in Excita; detection is on the ESP32."
}
```

**Why 501 and not 422**

422 says "your request body is unprocessable" and invites clients to
retry with a different payload. There is no payload the client can send
that will make an µWW adapter run live host-side detection. 501 is the
exact HTTP semantic ("the server does not support the functionality
required to fulfil the request") and it is preserved through proxies,
curl output, log aggregators, and OpenAPI code generators without any
of them needing special-case knowledge of Excita's error taxonomy.

**Why not 200 with a discriminated body**

Absorbing capability gaps into 200 responses breaks the invariant that
"non-2xx status = something needs attention" — a wire-level status
that log dashboards, uptime probes, and client-side error boundaries
already key on. Hiding a real capability mismatch inside a 200 body is
a form of the same dishonesty the null-stub pattern was replacing when
`NotSupportedError` was introduced (see spec 0011).

**The body shape**

- `code: "engine_capability_missing"` — the discriminant. New
  capability-adjacent errors (e.g. engine unknown at all) get their
  own codes, so clients can key on `code` rather than parsing the
  message.
- `engine` — the `EngineKind` string.
- `capability` — one of `load`, `feed`, `score`, `train`, `package`.
- `message` — a human-readable sentence with the *reason*, not just
  the fact. "microWakeWord does not run live host-side detection in
  Excita" tells the operator *why*, which is the difference between a
  useful error and an insulting one.

**Consequences**

- The frontend can render "this engine doesn't do that" as a disabled
  control with the message as tooltip, using `code` as the discriminant
  and never parsing the message.
- The `GET /engines` endpoint (planned) can surface the same
  capability matrix eagerly so the UI grays out unsupported controls
  before the operator clicks them — the 501 is the belt to that
  suspenders.
- The OpenAPI schema declares 501 with this body on every route that
  dispatches to an engine adapter.
