# Server-Owned Turn Reconstruction

Conduit will expose turn reconstruction as a server-owned API/read-model contract, not as a browser-side inference over raw runtime events. The raw event stream remains the live evidence channel, but the reconstruction contract is the operator-facing source for ordered turn story, pipeline attribution, spoken segments, tool batches, tool outcomes, and stable item identities.

Turn reconstruction has both live and history surfaces: active turns update as runtime events arrive, and completed or recently observed turns remain queryable according to configurable retention. Raw event envelopes, including sensitive tool arguments and result payloads, may be retained as diagnostic evidence, but default reconstruction views omit those payloads; access to sensitive tool evidence belongs behind an explicit diagnostic path with redaction and stronger access expectations.

The runtime should emit boundary events for facts only it knows directly, including spoken segment boundaries and tool batch boundaries. The reconstruction layer aggregates those events and references raw event ids, rather than asking UI clients to infer concurrency, speech scheduling, or model-round structure from adjacent low-level events.

The first implementation will use a bounded in-memory turn-history read model, with limits configured by environment. The API contract should not expose that storage choice: a later persistent backend may replace the in-memory store without changing how the operator console reads live or historical reconstructions.

Live reconstruction updates will use a reconstruction-specific API surface fed by the same runtime event bus, rather than overloading `/v1/events`. `/v1/events` remains the raw evidence stream for diagnostics and contract verification; reconstruction routes expose operator-facing turn state.

The initial route shape will treat turns as first-class resources: `GET /v1/turns` lists recent reconstructed turns, `GET /v1/turns/{turn_id}` fetches one reconstruction, `GET /v1/turns/{turn_id}/events` fetches raw evidence subject to payload access rules, and `GET /v1/turns/live` streams reconstruction updates for active turns across pipelines.

Live reconstruction streaming will use small typed updates, not full turn snapshots on every change. Clients recover from reconnects or detected gaps by fetching `GET /v1/turns/{turn_id}` for the affected turn.

Canonical ordering inside a reconstructed turn is a server-assigned monotonic sequence per turn. Timestamps remain display metadata; clients use sequence gaps in live updates as a signal to reload the full turn reconstruction.

The reconstruction sequence belongs to reconstruction items and live reconstruction updates, not to raw event envelopes. Raw envelopes remain evidence with event ids, traces, timestamps, conversation ids, and pipeline attribution; the reconstruction layer assigns operator-facing order while referencing raw event ids.

Tool calls in a reconstruction use explicit statuses such as requested, running, completed, failed, denied, and awaiting_confirmation. Tool call status is distinct from turn status: a tool failure or denial is part of the turn story, but it does not by itself mean the whole turn failed if the model recovered and produced an intelligible outcome.

Turn status remains coarse: running, completed, cancelled, failed, or degraded. Interruption is presented from `cancelled` plus the cancellation reason, such as `user_requested` for an explicit stop or future `barge_in` for voice detected over playback, rather than modeled as a separate canonical status.

`degraded` is a terminal top-level status. While a turn is still running, recoverable failures, denied tools, and other non-fatal problems are exposed as live warnings or item statuses; the final turn status is assigned when the turn reaches a terminal event.

Every live reconstruction update carries enough routing identity to stand on its own, including turn id, conversation id, pipeline name, and reconstruction sequence. This duplicates parent turn data deliberately so clients can route updates without relying on local lookup state.

Implementation should land runtime boundary events before the reconstruction API. The server read model should aggregate explicit spoken-segment and tool-batch boundaries instead of first shipping an inference-based API that would immediately become legacy compatibility baggage.
