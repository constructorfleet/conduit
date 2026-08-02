# Transport Pipeline And Reasoning Core

Implementation spec for [ADR-0012](../adr/0012-transport-pipeline-and-reasoning-core.md).

Splits the pipeline model into an acyclic transport pipeline of modality
transforms and a reasoning core that is a typed configuration record, then adds
text and hybrid pipelines on top of that split.

Stored pipelines and browser-local drafts are not migrated, per ADR-0012.

---

## Goals

1. Tools and memory attach to a reasoning core instead of being wired as
   pipeline stages, so the graph stops encoding half of a call-and-return arc.
2. A node carries its own configuration, so two pipelines can use one provider
   definition with different models, voices, and limits.
3. Edges are modality-typed, so structural validation replaces the runtime's
   hand-written stage-order assertions.
4. Text and hybrid pipelines run through the same core as voice pipelines, with
   speech-to-text and text-to-speech as optional adapters.

## Non-Goals

- Multiple reasoning cores in one graph. Validation rejects them.
- Executing core routing policy between models. The binding is modelled so the
  vocabulary exists, but resolution picks the primary model.
- Migrating stored pipelines.
- Changing the provider traits in `conduit-provider`. Memory, wake word, and
  speaker identification traits already exist and are unchanged.

---

## How This Is Broken Up

The four goals are close to independent, and only one of them needs the
reasoning core:

| Goal | Depends on |
| --- | --- |
| Per-node configuration | — |
| Modality typing | — |
| Text and hybrid pipelines | Modality typing |
| Reasoning core | Per-node configuration |

Work is therefore cut by capability rather than by layer. Each track below
changes `conduit-core` through to the operator console together, builds and
passes quality gates on its own, and is independently valuable. A track sliced
by layer instead — types, then validation, then resolution — cannot be
committed independently, because removing a `NodeKind` variant breaks
`Plan::resolve` in the same change that introduces its replacement.

Two rules keep the diffs reviewable:

- **Mechanical churn is separated from semantic change.** Track A reshapes
  `Node` while preserving today's node kinds and behavior; track D changes what
  the kinds mean on an already-typed model. Combined, the real change would be
  invisible inside ~110 lines of fixture churn.
- **Track D expands before it contracts.** `Core` lands alongside the existing
  kinds, resolution learns both, fixtures migrate, then the old variants are
  deleted. This is a transitional seam for the build, not user-facing
  compatibility; the final step removes it.

| # | Track | Delivers |
| --- | --- | --- |
| 0 | Graph fixture builders | Shrinks every later diff |
| A | Typed node variants with per-node config | Per-pipeline model and voice |
| B | Modality typing and edge validation | Structural wiring errors |
| C | Optional recognition and synthesis | Text pipelines |
| D | Reasoning core | Core bindings; deletes frontend workarounds |
| E | Multiple sources and sinks | Hybrid pipelines |
| F | Memory binding execution | Closes a listed gap |
| G | Tool confirmation enforcement | Closes a listed gap |

---

## Resolved Decisions

**Memory scope lives in `conduit-core`.** A `MemoryBinding` in the graph needs a
scope, but `Scope` is defined in `conduit_provider::memory` and
`conduit-provider` depends on `conduit-core`, so the graph cannot reference it.
`Scope` moves to a new `conduit_core::memory` module — it is already expressed
in terms of `conduit_core::id::{ConversationId, SpeakerId}` — and is re-exported
from `conduit_provider::memory` so provider code is unchanged. It has no other
consumers today, so the move is mechanical. Duplicating the enum in the graph
was rejected: two spellings of the same three-variant vocabulary is exactly the
competing-shape problem ADR-0011 avoids.

**Node configuration is typed per variant, not a config map.** Consistent with
[ADR-0011](../adr/0011-typed-provider-definitions.md). Adding a configurable
node kind expands the typed contract.

**`order` in `PipelineView` stays transport-only.** Core bindings have no
execution order, so they never appear in it.

---

## Domain Vocabulary

Recorded in [CONTEXT.md](../../CONTEXT.md): Transport Pipeline, Reasoning Core,
Core Binding, Modality, Utterance. `Spoken Segment` narrowed to the audio
rendering of an `Utterance Segment`.

---

## Track 0 — Graph Fixture Builders

`Node::new(id, kind, provider)` appears ~110 times across 12 files, nearly all
of them test fixtures rebuilding the same voice pipeline. Every later track
would rewrite all of them.

Add a `conduit-core` test-support module, behind a `testing` feature matching
the existing `conduit-provider` precedent, exposing intent-named builders:

```rust
pub fn voice_graph(name: &str) -> PipelineGraph;   // mic -> stt -> llm -> tts
pub fn voice_graph_with_tools(name: &str, tools: &[&str]) -> PipelineGraph;
```

Rewrite existing fixtures in `conduit-runtime/tests/`, `conduit-api/tests/`, and
`conduit-store/tests/` to call these where they are asserting something other
than graph shape. Tests that assert on graph structure itself — most of
`graph.rs` and `plan.rs` — keep building their graphs explicitly, because the
shape is the subject.

No behavior change. Quality gates prove it: the same tests pass unchanged.

## Track A — Typed Node Variants With Per-Node Config

`Node` becomes an internally tagged enum, **keeping today's node kinds**:

```rust
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
    Source    { id: NodeId, provider: String },
    WakeWord  { id: NodeId, provider: String },
    Stt       { id: NodeId, provider: String },
    SpeakerId { id: NodeId, provider: String },
    Router    { id: NodeId, provider: String },
    Llm       { id: NodeId, provider: String, model: Option<String>,
                system: Option<String>, max_rounds: usize },
    Tool      { id: NodeId, provider: String },
    Memory    { id: NodeId, provider: String, mode: MemoryMode,
                scope: Option<Scope>, limit: usize },
    Tts       { id: NodeId, provider: String, voice: Option<String> },
    Sink      { id: NodeId, provider: String },
}
```

`Node::id()` and `Node::kind_name()` become accessors. `NodeKind` survives as a
discriminant for the provider catalog until track D.

`Llm::model` being `None` means "the provider definition's first served model",
preserving today's behavior as an explicit opt-in rather than the only option.
This deletes the fallback at [plan.rs:90-99](../../crates/conduit-runtime/src/plan.rs#L90-L99),
so two pipelines can use one provider definition with different models.
`max_rounds` replaces the `DEFAULT_MAX_TOOL_ROUNDS` constant.

Includes the `Scope` move described under Resolved Decisions.

Backend: `Plan::resolve` reads config off the node instead of the provider.
API: `PipelineView` shape is unchanged; node JSON gains fields.
Frontend: node inspector gains model, system prompt, max rounds, and voice
fields; regenerate contracts.

Acceptance: two pipelines reference one language model provider definition with
different `model` values, and each test turn requests its own model.

## Track B — Modality Typing And Edge Validation

Add `Modality { Audio, Text, Utterance }`. `Source` and `Sink` gain a declared
modality; every other kind derives its own:

| Kind | In | Out |
| --- | --- | --- |
| `source` | — | declared |
| `wake_word` | audio | audio |
| `stt` | audio | text |
| `speaker_id` | audio | audio |
| `llm` | text | utterance |
| `tts` | utterance, text | audio |
| `sink` | declared; a `text` sink also accepts `utterance` | — |

`PipelineGraph::validate` gains
`GraphError::ModalityMismatch { edge, from, to, produced, expected }`. All
existing validation — duplicate ids, dangling edges, cycles, no-source,
no-sink, disconnected subgraphs, `reaches` — is unchanged and keeps its tests.

Removes `require_downstream` and its three call sites from `plan.rs`: a graph
wired `tts -> llm -> stt` now fails structurally rather than on a runtime
stage-order assertion.

Frontend: links marked with the modality their upstream node writes, and a
source or sink can declare what it carries. Backend validation stays
authoritative; the console mirrors `Node::output_modality` so it can say what a
link carries without a round trip.

There is no draw-time refusal, because there is no draw-time: the editor derives
edges from the stage chain and from tool attachment, so an operator never draws
an edge. Declaring modality on the endpoints is where the same mistake is
actually made, and changing an endpoint re-marks every link downstream of it.

Acceptance: `tts -> llm -> stt` is refused with a modality mismatch naming the
offending edge.

**Landed** as `a08601b` and `2bb3ff5`. `require_downstream` was kept, not
deleted: modality compatibility is a property of one edge, and branching past
the model is a property of a path — `stt -> llm` beside `stt -> tts` has two
compatible edges and still discards the model's answer. Track E's core
reachability is what subsumes it. Pinned by a regression test in
`crates/conduit-runtime/tests/plan.rs`.

## Track C — Optional Recognition And Synthesis

`Plan::stt` and `Plan::tts` become `Option`. A turn's input becomes either an
audio stream or a text utterance; when there is no recognizer the transcription
stage is skipped and `SpeechFinal` is published directly from the supplied text,
so reconstruction still sees a turn opening. Output fans to the resolved sinks;
sentence segmentation in `sentences.rs` is already the right boundary for both
renderings and is unchanged. Barge-in, idle deadline, and stop handling are
unchanged.

`SpokenSegmentRole` and `Event::SpokenSegmentStarted` generalize to
`UtteranceSegmentRole` and `Event::UtteranceSegmentStarted { modality, .. }`.
Per ADR-0012 this is the [ADR-0010](../adr/0010-server-owned-turn-reconstruction.md)
vocabulary widening, not a competing contract. `Event::contract_examples` must
cover a text-modality segment, since it generates the frontend fixtures.

API: `PipelineTestResult` gains `reply_text`; `audio_bytes` and `reply_audio`
are `None` for a text pipeline.

Frontend: `Guided Setup` gains a pipeline-shape choice, voice by default, text
producing `source(text) -> llm -> sink(text)` and asking for no speech
providers. Turn view keys off segment modality.

Acceptance: a text pipeline with no speech providers configured validates and
runs a test turn returning `reply_text`.

**Landed** as `c94758a`, `8b3d6d2`, `4fb11c9`, `43abca4`, and `0ab46f4`. An
operator can pick the text shape in guided setup, save a pipeline whose only
provider is a language model, and read the reply back from a test turn.

Two things came out differently from this spec. Sentence segmentation needed no
change at all — what a voice pipeline speaks a piece at a time is what a text
pipeline writes a piece at a time, trimming included. And resolution gained a
refusal the spec did not call for: a graph with neither a synthesizer nor a
sink is rejected, because making `tts` optional would otherwise have accepted a
pipeline that reasons and then discards the answer.

## Track D — Reasoning Core

Collapse `Llm`, `Tool`, `Memory`, and `Router` into one variant:

```rust
Core { id: NodeId, core: ReasoningCore }

pub struct ReasoningCore {
    pub model: ModelBinding,          // { provider, model: Option<String> }
    pub system: Option<String>,
    pub tools: Vec<ToolBinding>,      // { provider, confirm: ConfirmPolicy }
    pub memory: Vec<MemoryBinding>,   // { provider, mode, scope, limit }
    pub max_rounds: usize,
}
```

The per-node config from track A moves onto the core largely unchanged, which
is why A comes first — the fields already exist and already have tests.

Expand, migrate, contract, as three commits:

1. `Core` lands alongside the existing kinds; `Plan::resolve` produces a
   `CorePlan` from either shape.
2. Fixtures, guided setup, and the operator console move to `Core`.
3. `Llm`, `Tool`, `Memory`, and `Router` variants are deleted, along with the
   router refusal at [plan.rs:124-130](../../crates/conduit-runtime/src/plan.rs#L124-L130)
   and `reject_duplicate`.

Validation gains `GraphError::NoCore` and `GraphError::MultipleCores(Vec<NodeId>)`.
`ProviderComponentDescriptor::kind` moves from `NodeKind` to the existing
`ProviderCapability`, which already distinguishes capabilities without
conflating them with graph position.

Frontend: `pipelineGraphFlow` ([App.tsx:4450](../../frontend/src/App.tsx#L4450))
partitions `tool`/`memory` nodes into `augmentNodeIds`, sorts the rest by
`LINEAR_STAGE_ORDER` while discarding `graph.edges`, and defaults an augment's
target to the literal `"llm"`. All three are deleted: the spine is `order` from
`PipelineView`, and orbitals render from `core.tools` and `core.memory`.
`OrbitPosition` and the drag handlers survive as presentation, keyed on binding
index — dragging repositions an orbital, it does not rewire an edge.
`addReasoningAugment` ([App.tsx:2344](../../frontend/src/App.tsx#L2344)) becomes
`addCoreBinding`.

Acceptance: a graph with two core nodes is refused at validation, not at
prepare; no source file contains a fallback to the literal node id `"llm"`.

**Backend landed** as `10dde09`, `4767d0e`, `8e242d0`, `9f6a3bb`, and `bf2c5fa`.
The `llm`, `tool`, `memory`, and `router` node kinds are gone; a pipeline binds
a model, tools, and stores to one core.

**Console landed** as `01cfb66`. The augment partitioning, the hardcoded stage
ordering, and the fallback to the literal id `llm` are all deleted; orbitals
render from `core.tools` and `core.memory`, keyed by binding index.

Two gaps the deletion exposed, both fixed in `bf2c5fa` and worth knowing about:
`Node::provider_references` exists because a core names a provider per binding
and `Node::provider` answers with the model alone, so the delete refusal and
provider validation were both blind to bindings; and provider validation now
checks a core binding by binding, since no single capability describes one.

## Track E — Multiple Sources And Sinks

Validation requires every source to reach the core and the core to reach every
sink (`GraphError::CoreNotReachable`, `GraphError::SinkNotFedByCore`) rather
than assuming one of each. Runtime fans input in and output out.

This is also what finally replaces `require_downstream` in `plan.rs`, which
Track B kept because no per-edge rule can see a path branching past the model.
Delete it here, once core reachability states the same rule structurally.

Acceptance: a hybrid pipeline with an audio and a text source, and both sink
kinds, validates and routes either input to the same core.

## Track F — Memory Binding Execution

`Read`/`ReadWrite` bindings retrieve before the first model call and inject as
context; `Write`/`ReadWrite` store after the final round. Until this lands,
resolution refuses a binding mode it cannot execute rather than dropping it —
the rule that made the router refusal correct.

Acceptance: a `read_write` binding retrieves before the first model call and
stores after the final round, both visible on the event stream.

## Track G — Tool Confirmation Enforcement

`ConfirmPolicy::Always` publishes `Event::ToolConfirmationRequested` — a variant
that already exists and is currently unreachable — and awaits a decision before
dispatch.

Acceptance: a tool binding with `confirm: always` does not dispatch until
answered.

---

## Cross-Cutting

**Contracts.** Any track changing an API or event shape regenerates in the same
commit; `npm run contract:check` must pass without hand-edited files under
`frontend/src/contracts/`.

```sh
CONDUIT_UPDATE_FRONTEND_CONTRACTS=1 cargo test -p conduit-api --test frontend_contract
```

**Graph width.** The graph model stays deliberately wider than the runtime can
execute. Refusing an expressible-but-unrunnable topology at prepare time,
rather than accepting and silently ignoring it, remains the rule.

**Documentation.** Each track updates `docs/architecture.md`, and tracks C, F,
and G update the known gaps in [README.md](../../README.md) and
[docs/IMPLEMENTATION_GAPS.md](../IMPLEMENTATION_GAPS.md) as they close them.
