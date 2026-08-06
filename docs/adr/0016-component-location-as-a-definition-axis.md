# Component Location As A Definition Axis

Where a component runs becomes a field on its **provider definition**, and a
documented **component protocol** becomes the way a process serves a component
Conduit was not compiled with. There is no group-level partition of the
pipeline: co-location is what happens when several definitions name the same
host, and a development deployment running everything in one process is what
happens when none of them name a host at all.

The requirement that produced this is not "distribute Conduit". It is being able
to answer *which recognizer, which engine, streaming or batch, CPU or GPU, on
device or on a hypervisor* by editing configuration and measuring, rather than by
rearchitecting per experiment. Distribution is one axis of that question and not
the interesting one; the ADR that makes the question decidable is
[ADR-0018](0018-comparison-judged-by-agreement.md).

**Most of this already exists, unevenly**

Location is already a definition field for some components and absent for
others, and the unevenness is accidental rather than designed.
`SttVariant::Wyoming` carries a `url` and a `streaming` flag
(`conduit-provider/src/storage/stt.rs`); `conduit-wyoming` reaches recognition,
synthesis, and wake word detection over TCP; `conduit-speaker` reaches speaker
identification over HTTP; `conduit-mcp` reaches tools over stdio, streamable
HTTP, and SSE.

Two components have no remote variant at all. `VadVariant` has exactly one arm,
`Silero`, and its doc comment states the reason plainly — "No base URL and no API
key: there is no service" (`storage/vad.rs`). `TransformVariant` has `Builtin`
and `Script`, both in-process by construction. So a deployment can put its
recognizer on a GPU host today and cannot put its detector anywhere, which is
the gap this decision closes rather than a new capability it invents.

Engine choice is likewise already a configuration axis, and outside Rust.
`services/wyoming-asr/app.py` says it in its own header: "The protocol is the
stable part. The engine is not: `ASR_ENGINE` chooses it." `services/speaker-id`
has the same shape, with a `requirements-*.txt` per backend. Comparing two ASR
engines is therefore an environment variable on an image this repository already
publishes, not a Rust change — and adding a third engine is an addition to
`build_engine`, not to `ProviderDefinitionVariant`.

**Per-component seams, and one fast path**

Every provider trait is remotable independently. This follows the ports-and-
adapters shape the workspace already has — the ten traits in `conduit-provider`
are the ports, each `conduit-*` crate is an adapter, and no trait mentions a
transport. Location does not leak into the port.

Independent per-component remoting has one real cost: the components that read
captured audio would each receive the same frames over the network separately,
and a voice activity detector gating a recognizer is a per-frame gate, so the
gating decision would cost a round trip per frame. The **audio stage set** —
detection, wake word, recognition, identification — therefore gets a fast path:
one connection carrying the audio once, plus declarative chaining so a caller can
say *run detection, and run recognition on the same frames only if detection
reports speech* without the frames or the intermediate answer crossing the
network twice.

The chaining is declarative on purpose. A coarser alternative — a group-level
"audio in, utterance out" operation that decides internally how the components
compose — would be faster still and would destroy the observability the product
is built on: `Component Health` for four components collapses into one opaque
box, `Turn Reconstruction`'s ordered story of invoked components loses its
steps, and `Proven Provider` stops being answerable per component. The runtime
keeps the decisions; only the data stays put.

There is no corresponding fast path for the other components, because none has
been identified that saves anything. A transform is a string in and a string
out; a memory call is a query. Symmetry is not a reason to build a second
surface.

**One event writer**

A remote host returns per-step outcomes — what ran, how long it took, what it
produced, what failed — as **data**. The Conduit runtime, which is the only
thing holding the turn, emits every event itself.

The alternative is letting each remote host publish into the event bus directly,
and it reintroduces a failure this project already ruled out.
[ADR-0010](0010-server-owned-turn-reconstruction.md) made turn reconstruction
server-owned with a server-assigned monotonic sequence as canonical ordering,
specifically so clients do not invent ordering. Multiple hosts producing into
that stream is the same defect relocated from the browser to the GPU box: two
writers, two clocks, one sequence that has to be reconciled. One writer means
there is nothing to reconcile.

The consequence is that a remote cannot report what the runtime has no event
for, so extending observability means extending the shared event vocabulary.
That is the intended trade: the vocabulary is the contract, and a remote that
could invent events would let a provider define its own observability surface.

**Compiled adapters plus an escape hatch, not dynamic loading**

Adapters remain compile-time: a crate, usually behind a feature, plus a
`ProviderDefinitionVariant` arm. On top of that, one adapter per capability
speaks the **component protocol** to any process implementing it.

That combination is what "plugin architecture" should mean here. Dynamic loading
— shared libraries or WASM resolved at runtime — would buy the same
extensibility at the price of ABI stability across ten traits, unsafe loading,
and a third-party crash landing inside the process that owns every turn. For a
system that already reaches components over a network, a process boundary is the
cheaper and stronger isolation. It is also the pattern this repository has
already chosen twice without naming it: `services/wyoming-asr` and
`services/speaker-id` are out-of-process adapters written in Python. The
component protocol names that pattern and generalizes it.

**Transport is deferred to measurement, deliberately**

Wyoming stays the transport for the components that already use it, and HTTP for
the rest. Whether Conduit needs a protocol of its own is not decided here,
because the instrument that would answer it is being built in
[ADR-0018](0018-comparison-judged-by-agreement.md), and guessing ahead of a
measurement this work produces would be strange.

Two things are already known and point in different directions. Wyoming's
observed problem in this deployment has been semantic rather than latency —
[ADR-0012](0012-transport-pipeline-and-reasoning-core.md)'s successor work found
a server advertising streaming partials and sending none, which is why
`SttVariant::Wyoming`'s `streaming` flag documents that a server saying no still
returns a correct single final. Against that, the audio-stage fast path's
chaining and per-step outcomes have no expression in Wyoming at all. The
resolution is that the component protocol ships from the start, since it is the
plugin story; the fast-path extensions to it wait for a measured reason.

**Consequences**

- `VadVariant` and `TransformVariant` gain remote arms. `VadVariant::Silero`'s
  "there is no service" doc comment describes that arm and stays true of it.
- A component protocol is specified and documented well enough for someone to
  implement a provider in another language against it. It is a public contract
  from the moment it ships.
- Remote responses carry per-step outcomes as data. No remote host publishes
  events, and the runtime's sequence stays single-writer per
  [ADR-0010](0010-server-owned-turn-reconstruction.md).
- The audio stage set is a modelled concept because the fast path needs a name
  for what it negotiates over. It is not a partition of the graph and does not
  appear in `PipelineGraph`.
- No `Vox`, `Echo`, or `Nexus`. A grouping of transform beside synthesis pins a
  pure-CPU rewrite to a GPU host for nothing, and a grouping of tools with
  memory names as peers two things
  [ADR-0012](0012-transport-pipeline-and-reasoning-core.md) established are
  bindings on a reasoning core rather than transport stages. Images are named
  literally and added when a component needs one, as `conduit-speaker-id`
  already is.
- Stored pipelines and provider definitions are not migrated. Location arms are
  additive: an existing definition names no host and continues to run in
  process, which is the same additive posture
  [ADR-0014](0014-voice-activity-detection-as-two-decisions.md) took and
  narrower than the reshaping in
  [ADR-0011](0011-typed-provider-definitions.md).
- A remote component is a new failure surface for `Provider Status`. A definition
  that is reachable in one deployment and unreachable in another is the normal
  case now, so reachability reporting must name the host.

**Open questions**

- Does the component protocol carry audio as frames or as a stream, and is that
  the same framing the fast path uses? Answering it before there is a measured
  fast-path requirement risks designing the fast path twice.
- Does a remote host authenticate to Conduit, Conduit to it, or both? The
  existing remotes disagree: Wyoming has no authentication, and
  `conduit-speaker` has HTTP.
- Is there a single generic adapter per capability, or one adapter that covers
  every capability? The traits differ enough in shape that this is not obvious.
