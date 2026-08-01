# Implementation Gaps

Findings from a 2026-08-01 audit comparing the codebase against `docs/architecture.md`,
`docs/api.md`, `docs/configuration.md`, the crate READMEs, and `docs/adr/*.md`. Each
entry is a genuine mismatch between documented design and actual code, not a missing
feature or style nitpick.

## Runtime / Core (`conduit-core`, `conduit-runtime`)

1. **Node `config` field removed but still documented.** `docs/architecture.md:64` and
   `crates/conduit-core/README.md:19-20` say provider-specific configuration lives in a
   per-node JSON `config` value. `crates/conduit-core/src/graph.rs:51-58` no longer has
   that field (removed in commit `8e0f344`, "remove provider config from pipeline
   nodes"). The `Node::new` doc comment at `graph.rs:61` is also stale ("Creates a node
   with empty configuration").
2. **Inline Wyoming TTS construction documented, removed from code.**
   `docs/configuration.md:86-102` and `README.md:499-503` describe the operator console
   embedding a provider definition directly into a saved graph node, with "Wyoming TTS
   ... currently supported this way." The `wyoming` module and this construction path
   have been removed from `conduit-runtime`; `plan.rs` never reads node config and
   hardcodes the TTS voice to `None`.
3. **Leftover wyoming provider descriptors.** `crates/conduit-api/src/pipelines.rs:300`
   and `:338` still emit `wyoming`/`wyoming.tts` provider descriptors even though the
   runtime construction path described in #2 was removed — inconsistent with the
   removal.

## API layer (`conduit-api`)

4. **Turn-reconstruction API undocumented.** `/v1/turns`, `/v1/turns/live`,
   `/v1/turns/{id}`, `/v1/turns/{id}/events` are live (per ADR 0010) and present in the
   typed client, but entirely absent from `docs/api.md`.
5. **Contract fixture drift.** The checked-in `status_fixture()` in
   `crates/conduit-api/tests/frontend_contract.rs:211-242` lists only 4 status
   bindings; the real `/v1/status` response has 5 (missing `ProviderStatus`, documented
   at `docs/api.md:164-173`). This undermines ADR 0006's guarantee that the generated
   contract matches the runtime shape exactly.
6. **`recent_failures[].provider` always null.** Documented as populated
   (`docs/api.md:128-136`, e.g. `"provider": "piper-local"`), but
   `crates/conduit-api/src/status.rs:1090` hardcodes `provider: None` when building the
   `FailureRecord`.
7. **`pipelines[].affected_providers` over-reports.** Docs describe this field as the
   provider identifiers *currently affecting* the pipeline (`docs/api.md:91` example
   shows only the failing provider). `pipeline_provider_ids`
   (`crates/conduit-api/src/status.rs:784-794`) returns every provider referenced by the
   graph regardless of health.

## Frontend

8. **Live event stream never wired up.** `frontend/README.md:22-23,39-40` and ADR 0009
   describe status snapshot loading followed by applying `/v1/events` updates, with
   stale-state handling on disconnect and refresh on reconnect. `frontend/src/App.tsx`
   never opens a connection to `/v1/events`; `frontend/src/eventStream.ts` exports the
   helpers for this (`transitionEventStream`, `applySnapshotEvent`) but nothing calls
   them at runtime, and `initialEventStreamPlan()` hardcodes posture to `"live"`
   (`eventStream.ts:42`, `App.tsx:319-324,620`).
9. **Client-side turn reconstruction contradicts ADR 0010.** ADR 0010 mandates
   server-owned turn reconstruction and explicitly forbids browser-side inference over
   raw runtime events. `App.tsx:1757-1761` falls back to `reconstructTurn(events)`
   whenever `turnSnapshot` is null (the common case, since panel events default to
   static fixtures); `reconstructTurn` (`App.tsx:3582-3608`) does exactly the
   browser-side inference the ADR forbids.
10. **Wrong ordering key in client-side reconstruction.** ADR 0010:17 specifies a
    server-assigned monotonic sequence as canonical ordering, with timestamps as display
    metadata only. `reconstructTurn` sorts by `left.at.localeCompare(right.at)`
    (`App.tsx:3583-3585`) — timestamp order, not sequence order.

## Providers / Store / Metrics / OpenAI / ADRs

11. **`docs/adr/README.md:4`** still reads "There are no ADRs recorded yet." This is
    false — 10 ADRs (0001–0010) exist and are implemented in code.
12. **`docs/architecture.md:87-88`** (HTTP Boundaries) says the service router carries
    only "conversations, pipeline CRUD, validation, and event streaming." This omits the
    implemented `/v1/status` and `/v1/turns*` surfaces — an entire subsystem backed by
    two ADRs and live code is missing from the architecture doc.
13. **Minor: provider capability table mismatch.** `crates/conduit-provider/README.md`
    lists `storage | PipelineStore` in the "every provider implements `Provider` plus
    one capability trait" table. `PipelineStore` (`storage.rs:78`) has no `Provider`
    supertrait and no store backend implements `Provider` — storage is a store contract,
    not a registrable provider capability.

## Verified consistent (not gaps)

For reference, the following areas were checked and found to match their documentation:
token storage location (session vs. local storage per ADR 0008), the five-section
operator UI information architecture (ADR 0003), `/v1/status` management-token gating
(ADR 0001), test-turn error codes, `/v1/events` unknown-stage handling, metrics
signal coverage, store contract semantics (name validation, `is_listable`,
conflict-based replace), and OpenAI provider codec constraints (STT/TTS format
support).
