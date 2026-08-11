# 0009 — Memoria side channels

Concrete side channels layered on the [0005 §Side channels](0005-link-protocol.md#side-channels) contract. Ends the punt in spec 0005 §Extension points (*"Memoria — memory/MCP surface — orthogonal to the panel, not part of this spec"*).

Two decisions in one spec: how Memoria's **MCP surface** relates to the link protocol, and whether Memoria needs any **non-MCP** side channel.

Anchors: `services/memoria/mcp_server.py`, `services/memoria/app.py`, `crates/conduit-provider/src/storage/tool.rs` (`McpTransport` — Conduit already knows how to talk to MCP servers as tool providers).

---

## MCP is not a side channel

**Option B (chosen).** The link handshake advertises Memoria's MCP endpoint(s) in a capability field for **discovery only**; MCP itself keeps its own transport, its own auth, its own capability negotiation. Wrapping MCP calls in the symmetric-token auth (Option C) is rejected outright — MCP has a mature auth story and forcing `peer_token` on top of it doubles the surface for no gain. Leaving MCP fully outside (Option A) works but throws away the one thing the link protocol adds: the operator only has to point Conduit at Memoria's base URL once, and the MCP endpoint discovery falls out for free.

The rule: **the link protocol advertises MCP; MCP speaks MCP.** No wire encapsulation.

### Capability

`memoria.mcp` — declared in the peer's `capabilities` array at handshake. The peer additionally includes an MCP-discovery field in the handshake body:

```json
{
  "service_kind": "memoria",
  "peer_id": "memoria-prod",
  ...,
  "capabilities": ["memoria.mcp"],
  "capability_endpoints": {
    "memoria.mcp": {
      "transport": "streamable_http",
      "url": "https://memoria.example/mcp"
    }
  }
}
```

- **`capability_endpoints`** (OPTIONAL, at the top level of the handshake) — a `Map<capability_name, endpoint_object>`. Endpoint object shape is defined per-capability by that capability's spec, not by 0005 itself. A capability that needs no endpoint metadata (e.g. `vox.roster`, `excita.wake-events`, `dicta.transform` — all of which live on well-known paths relative to `peer_base_url`) simply omits its entry.
- For `memoria.mcp`, the endpoint object is `{transport: "sse" | "streamable_http" | "stdio", url?: string, command?: string, args?: string[]}` — mirroring `McpTransport` in `crates/conduit-provider::storage::tool::McpTransport` so a Conduit MCP tool provider can consume it directly.
- **Stdio transport** on this field is deliberately allowed but exotic — a linked Memoria running MCP over stdio would only make sense in a co-located deployment where the peer's process is spawnable by Conduit's host. Documented for symmetry; not a first-class deployment.

Conduit stores the endpoint verbatim on the row alongside the capability. `GET /v1/linked-services` surfaces `capability_endpoints` back so an operator screen can display "Memoria's MCP is at `<url>`" without a second call.

**Auth is MCP's problem.** Whatever token/authz MCP wants on that transport is negotiated at MCP layer, out of band from the link. If an operator's deployment wants to give an MCP client its bearer token, that happens through MCP config, not through Conduit.

### Failure semantics

MCP transport failure is not part of base `reachability`. If Conduit consumes `memoria.mcp` (via a `ToolVariant::Mcp` provider that resolves against the row's endpoint) and MCP is down, that surfaces as a **tool-provider** failure on the operator UI, not as an unreachable Memoria tab. The base link tab keeps showing up.

## Non-MCP side channel: none needed

**No `memoria.ingest`, no `memoria.remember`.** MCP itself already exposes memory-write tools (`store`, `remember`, whatever Memoria's MCP server names them); adding a parallel HTTP `POST /remember` on the link plane would be two paths to do one thing. If a specific ingest workflow ever needs a channel MCP can't express, that's a fresh ticket and a fresh capability name — not a placeholder to reserve now.

The one exception considered and rejected: a "sync memories from Conduit to Memoria" bulk channel. Not needed — memory is Memoria's, not Conduit's, so there is nothing to push.

## Capability registry entries

Per this spec:

- `memoria.mcp` — discovery-only, endpoint in `capability_endpoints`.

That's it. `memoria.*` prefix stays reserved to `LinkedServiceKind::Memoria` by 0005; future Memoria capabilities land under it.

## 0005 amendment (docs-only follow-up)

The `capability_endpoints` field is an ADDITIVE change to spec 0005 §Handshake. A separate ticket adds:

- Optional `capability_endpoints: Map<capability_name, object>` to the handshake request.
- One sentence in §Side channels: *"A capability MAY require peer-supplied endpoint metadata; the peer includes it under the capability's key in `capability_endpoints`. The endpoint object shape is defined by that capability's spec."*
- Note on `GET /v1/linked-services`: `capability_endpoints` surfaces alongside `capabilities`.

This amendment is small and non-breaking (serde defaults on both sides). Track as a follow-up ticket rather than blocking this design.

## Implementation scope (future ticket)

- Extend `LinkedService` storage row to carry `capabilities: Vec<String>` and `capability_endpoints: Map<String, serde_json::Value>` (both `#[serde(default)]`).
- Extend the handshake handler (`crates/conduit-api/src/linked_services.rs::create`) to accept and persist both fields.
- Surface both fields on `LinkedServiceView` returned by `GET /v1/linked-services`.
- Add a helper on `LinkedService` to synthesize an `McpTransport` when the row carries `memoria.mcp`, so a `ToolVariant::Mcp` provider definition can reference the peer by id and pick up the endpoint automatically.

## Non-goals

- Any implementation of MCP inside Conduit (Conduit already consumes MCP as a tool provider; nothing new needed).
- A non-MCP write path for memories.
- Wrapping MCP auth in `peer_token`.
