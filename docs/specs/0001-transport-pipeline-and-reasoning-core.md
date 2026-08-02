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

- Multiple reasoning cores in one graph. Validation rejects them for now.
- Executing core routing policy between models. The binding is modelled so the
  vocabulary exists, but resolution picks the primary model.
- Migrating stored pipelines.
- Changing provider traits in `conduit-provider`. Memory, wake word, and
  speaker identification traits already exist and are unchanged.

---

## Domain Vocabulary

New terms for `CONTEXT.md`:

**Transport Pipeline**: The acyclic dataflow portion of a pipeline graph — the
modality transforms from source through sink. Runs once per turn in topological
order. _Avoid_: Spine, main flow.

**Reasoning Core**: The single graph node that holds a language model binding
and its tool and memory bindings, and runs a model-driven iteration whose length
is decided at runtime. _Avoid_: Agent, LLM node, brain.

**Modality**: The kind of data an edge carries — `audio`, `text`, or
`utterance`. Sources and sinks declare theirs; other stages derive theirs from
their kind. _Avoid_: Media type, format.

**Utterance**: What a reasoning core emits, before any decision about how to
render it. Speech is an utterance rendered by a synthesizer; text is an
utterance rendered by a text sink. _Avoid_: Reply, response text.

**Core Binding**: A tool or memory attachment on a reasoning core, referencing a
provider definition by id and carrying the per-pipeline settings for that
attachment. _Avoid_: Augment, orbital, spoke.

---

## Backend

### B1. Graph model — `conduit-core/src/graph.rs`

Replace `Node { id, kind, provider }` and `NodeKind` with typed variants,
internally tagged on `kind` so the wire format stays readable and the frontend
discriminates on one field.

```rust
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
    Source     { id: NodeId, modality: Modality, provider: String },
    WakeWord   { id: NodeId, provider: String },
    Stt        { id: NodeId, provider: String },
    SpeakerId  { id: NodeId, provider: String },
    Core       { id: NodeId, core: ReasoningCore },
    Tts        { id: NodeId, provider: String, voice: Option<String> },
    Sink       { id: NodeId, modality: Modality, provider: String },
}

#[serde(rename_all = "snake_case")]
pub enum Modality { Audio, Text, Utterance }

#[serde(deny_unknown_fields)]
pub struct ReasoningCore {
    pub model: ModelBinding,
    pub system: Option<String>,
    pub tools: Vec<ToolBinding>,
    pub memory: Vec<MemoryBinding>,
    pub max_rounds: usize,          // default 4, was DEFAULT_MAX_TOOL_ROUNDS
}

pub struct ModelBinding { pub provider: String, pub model: Option<String> }

pub struct ToolBinding {
    pub provider: String,
    pub confirm: ConfirmPolicy,     // Never | Always
}

pub struct MemoryBinding {
    pub provider: String,
    pub mode: MemoryMode,           // Read | Write | ReadWrite
    pub scope: Option<Scope>,       // conduit_provider::memory::Scope
    pub limit: usize,               // retrieval cap, ignored for Write
}
```

`Node::id` and `Node::kind_name` become accessor methods so existing call sites
that read `node.id` keep a one-line equivalent.

`ModelBinding::model` being `None` means "the provider definition's first served
model", preserving today's behavior as an explicit opt-in rather than the only
option. This deletes the fallback comment at `plan.rs:90-99`.

`ConfirmPolicy` is modelled and validated now, and enforced in B4. The
`Event::ToolConfirmationRequested` variant already exists and is currently
unreachable.

### B2. Validation — `conduit-core/src/graph.rs`

Unchanged: duplicate ids, dangling edges, cycles, no-source, no-sink,
disconnected subgraphs, and `reaches`. These apply to the transport pipeline and
keep their current tests.

Added to `PipelineGraph::validate`:

- **Exactly one core.** `GraphError::NoCore` when zero,
  `GraphError::MultipleCores(Vec<NodeId>)` when more than one.
- **Modality compatibility per edge.** Each node kind declares
  `input_modality()` and `output_modality()`:

  | Kind | In | Out |
  | --- | --- | --- |
  | `source` | — | declared |
  | `wake_word` | audio | audio |
  | `stt` | audio | text |
  | `speaker_id` | audio | audio |
  | `core` | text | utterance |
  | `tts` | utterance, text | audio |
  | `sink` | declared; a `text` sink also accepts `utterance` | — |

  Mismatch is `GraphError::ModalityMismatch { edge, from, to, produced, expected }`.

- **Core reachability.** Every source must reach the core and the core must
  reach every sink, as `GraphError::CoreNotReachable(NodeId)` /
  `GraphError::SinkNotFedByCore(NodeId)`. This subsumes the runtime's
  `require_downstream` checks.

`Edge` keeps `{ from, to, port }`. `port` stays for future core-adjacent fan-out
and is currently only meaningful on multi-output nodes; nothing produces one
after `router` is removed, so validation rejects a `port` on any current kind.

Tests: keep every existing test in `graph.rs`, rewritten against the new node
constructors, plus one per new error variant, a text pipeline that validates
with no `stt`/`tts`, and a hybrid pipeline with two sources and two sinks.

### B3. Plan resolution — `conduit-runtime/src/plan.rs`

```rust
pub struct Plan {
    pub pipeline: String,
    pub input: Modality,
    pub output: Vec<Modality>,
    pub stt: Option<Resolved<dyn SpeechToText>>,   // None for text input
    pub tts: Option<Resolved<dyn TextToSpeech>>,   // None for text-only output
    pub core: CorePlan,
}

pub struct CorePlan {
    pub node: String,
    pub llm: Arc<dyn LanguageModel>,
    pub model: String,
    pub system: Option<String>,
    pub tools: BTreeMap<String, ResolvedTool>,     // name -> provider + confirm
    pub memory: Vec<ResolvedMemory>,
    pub max_rounds: usize,
}
```

Removals: `require_downstream` and its three call sites, `reject_duplicate`
(single-core validation covers the model; multiple `stt`/`tts` on distinct
branches becomes legal and is resolved per branch), and the `router` refusal.

Retained: `Error::UnknownProvider` on unregistered ids, and the tool-name
collision check, which now reports the binding index rather than a node id.

New refusal: a core with memory bindings resolves them, but if the runtime
cannot yet execute a binding mode it must refuse rather than drop it — the same
rule that made the router refusal correct.

Tests: `plan.rs` currently asserts the router refusal against a specific graph
shape; replace with a multi-core refusal. Add a text-pipeline plan that resolves
`stt: None`, and a per-pipeline model override that differs from the provider
definition's first model.

### B4. Turn execution — `conduit-runtime/src/turn.rs`

The turn's input becomes an enum of `ChunkStream<AudioChunk>` or a text
utterance; when `Plan::stt` is `None` the transcription stage is skipped and
`SpeechFinal` is published directly from the supplied text so reconstruction
still sees a turn opening.

Output becomes an utterance stream fanned to the resolved sinks. Sentence
segmentation in `sentences.rs` is already the natural boundary for both
renderings and is unchanged; a text sink emits the same segments without
synthesis. Barge-in, idle deadline, and stop handling are unchanged.

Memory bindings run inside the round loop: `Read`/`ReadWrite` retrieve before
the first model call and inject as context; `Write`/`ReadWrite` store after the
turn's final round. Tool confirmation publishes
`Event::ToolConfirmationRequested` and awaits a decision before dispatch when
`ConfirmPolicy::Always`.

### B5. Events and reconstruction — `conduit-core/src/event.rs`, `conduit-api`

`SpokenSegmentRole` and `Event::SpokenSegmentStarted` generalize to
`UtteranceSegmentRole` and `Event::UtteranceSegmentStarted { modality, .. }`.
Per ADR-0012 this is the ADR-0010 reconstruction vocabulary generalizing, not a
new contract; `Stage::Synthesis` remains but is absent from text turns.

`Event::contract_examples` must cover a text-modality segment, since it is the
generation source for frontend fixtures.

### B6. API — `conduit-api/src/pipelines.rs`

`PipelineView { graph, order }` is unchanged in shape. `order` continues to
report transport nodes only; core bindings have no execution order.

`ProviderComponentDescriptor::kind` currently types a component by `NodeKind`.
Tool and memory components no longer have a node kind, so this becomes the
existing `ProviderCapability` from `conduit-provider::storage`, which already
distinguishes the capabilities and does not conflate them with graph position.

The pipeline test turn (`PipelineTestRequest`) already takes an `utterance`
string and feeds it to STT. For a text pipeline it feeds the core directly, and
`PipelineTestResult.audio_bytes` / `reply_audio` become `None` with a new
`reply_text` field.

Regenerate contracts:

```sh
CONDUIT_UPDATE_FRONTEND_CONTRACTS=1 cargo test -p conduit-api --test frontend_contract
```

---

## Frontend

All in `frontend/src/App.tsx` unless noted. Types in `frontend/src/contracts/`
are generated — do not hand-edit; regenerate via B6.

### F1. Delete the workarounds

`pipelineGraphFlow` (App.tsx:4450) partitions `tool`/`memory` nodes into
`augmentNodeIds`, sorts the remainder by `LINEAR_STAGE_ORDER` while discarding
`graph.edges`, and defaults an augment's target to the literal `"llm"`. All
three go away:

- The spine is `graph.order` from `PipelineView`, which the backend already
  computes. Stop re-deriving it in the browser.
- Orbitals render from `core.tools` and `core.memory` — arrays with a defined
  order — so `spokesByTarget`, `orbitPositionForNode`, and the `?? "llm"`
  fallback are replaced by indexing into the core's own bindings.
- `LINEAR_STAGE_ORDER` is deleted rather than updated.

`OrbitPosition`, `AugmentDragState`, and the drag handlers survive as
presentation: dragging repositions a binding's orbital, it does not rewire an
edge. Persisted orbit positions key on binding index instead of node id.

### F2. Core inspector

Selecting the core node opens an inspector with model binding (provider
definition picker plus optional model override), system prompt, max rounds, and
two binding lists. Each tool binding row has a provider picker and a confirm
toggle; each memory binding row has a provider picker, mode, scope, and limit.
`addReasoningAugment` (App.tsx:2344) becomes `addCoreBinding`, appending to the
core's array rather than pushing a node and an edge.

### F3. Node configuration

Transport nodes gain inspector fields for their own variant: modality on source
and sink, voice on TTS. This is the first per-node configuration in the editor;
`ProviderEditorFields` (App.tsx:1392) is for provider definitions and is not
reused — node config is a distinct form driven by the node variant, not by the
component catalog schema.

### F4. Modality in the canvas

Edges are labelled or colour-coded by modality. An edge the operator draws
between incompatible modalities is refused at draw time with the reason, rather
than waiting for backend `Pipeline Validation`. Backend validation remains
authoritative; this is a fast local echo of one rule.

### F5. Pipeline shapes in guided setup

`Guided Setup` gains a pipeline-shape choice ahead of `Provider-First Setup`:
voice, text, or hybrid. Voice remains the default and produces today's
`source(audio) -> stt -> core -> tts -> sink(audio)`. Text produces
`source(text) -> core -> sink(text)` and asks for no speech providers, which
makes the minimal working loop reachable with only a language model configured.

### F6. Turn view

Turn reconstruction rendering keys off segment modality (B5) so a text turn
shows utterance segments without a synthesis stage or a playback control.

---

## Acceptance Criteria

1. A voice pipeline built in the editor validates, runs a `Pipeline Test Turn`,
   and produces audio — unchanged operator-visible behavior.
2. A text pipeline with no speech providers configured validates and runs a
   test turn returning `reply_text`.
3. A hybrid pipeline with an audio and a text source, and both sink kinds,
   validates and routes either input to the same core.
4. Two pipelines reference one language model provider definition with
   different `model` overrides, and each test turn requests its own model.
5. A graph wired `tts -> core -> stt` is refused with a modality mismatch naming
   the offending edge, not a stage-order message.
6. A graph with two core nodes is refused at validation, not at prepare.
7. A tool binding with `confirm: always` publishes
   `Event::ToolConfirmationRequested` and does not dispatch until answered.
8. A core with a `read_write` memory binding retrieves before the first model
   call and stores after the final round, both visible on the event stream.
9. `npm run contract:check` passes without hand-edited files under
   `frontend/src/contracts/`.
10. No source file contains a fallback to the literal node id `"llm"`.

---

## Phasing

Each phase is a commit, TDD per `AGENTS.md`, quality gates green before the
next.

| # | Change | Surface |
| --- | --- | --- |
| 1 | Typed `Node` variants, `Modality`, `ReasoningCore` types | B1 |
| 2 | Core-count, modality, and core-reachability validation | B2 |
| 3 | `Plan`/`CorePlan` resolution; delete `require_downstream`, router refusal | B3 |
| 4 | Optional STT/TTS; utterance output fan-out | B4 |
| 5 | Utterance segment events and reconstruction | B5 |
| 6 | API shapes, catalog capability, test-turn text reply; regenerate contracts | B6 |
| 7 | Delete augment partitioning and `LINEAR_STAGE_ORDER`; render from `order` | F1 |
| 8 | Core inspector and binding editing | F2 |
| 9 | Node configuration fields; modality in canvas | F3, F4 |
| 10 | Memory binding execution | B4 |
| 11 | Tool confirmation enforcement | B4 |
| 12 | Guided setup pipeline shapes; turn view modality | F5, F6 |

Phases 10 and 11 are separated from phase 4 because each closes a gap listed in
[README.md](../../README.md) and [IMPLEMENTATION_GAPS.md](../IMPLEMENTATION_GAPS.md)
in its own right, and each is independently shippable once the core exists.

## Documentation

- `CONTEXT.md`: the five terms above.
- `docs/architecture.md`: the Graphs section describes the two-tier model; the
  Runtime Flow diagram shows the core with bindings rather than `llm -> tools`;
  the Current Limits section drops router, memory, and tool confirmation as
  those phases land.
- `README.md` and `docs/IMPLEMENTATION_GAPS.md`: known gaps updated per phase.
