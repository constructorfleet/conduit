# Transport Pipeline And Reasoning Core

A pipeline graph will model two different things with two different shapes: a
**transport pipeline**, which is an acyclic dataflow graph of modality
transforms, and a **reasoning core**, which is a typed configuration record
occupying exactly one node in that graph. The flat graph that today mixes
`stt`, `llm`, `tool`, and `memory` into a single `NodeKind` enum is retired.

The two halves have genuinely different execution models, and forcing them into
one DAG has been producing false structure. Transport stages run once per turn
in topological order, each consuming one stream and producing another; the
ordering, the cycle rejection, and the connectivity checks in
`conduit_core::graph` are all load-bearing for them. The reasoning core runs an
iteration whose length is decided at runtime by the model, and whose calls out
to tools and memory return to the model rather than continuing downstream.
There is no static order among a core's tools, so a topological position for a
tool node carries no information.

Modelling tool and memory calls as edges required stating things that are not
true. `Plan::resolve` requires every tool node to be *downstream* of the model,
because the real shape — `llm -> tool -> llm` — is a cycle the graph rejects;
the return arc exists only as `max_tool_rounds` in the runtime. Memory is worse,
because retrieval is an inflow and writing is an outflow on the same node, and
graph connectivity is deliberately computed ignoring edge direction so that
either wiring passes. The operator console then discards these edges entirely:
it partitions tool and memory nodes out as "augments", re-derives the spine by
sorting the remaining nodes into a hardcoded stage order rather than following
edges, and defaults an augment's target to the literal id `"llm"`. Three layers
independently work around the same modelling error.

Nodes become typed variants carrying their own configuration, consistent with
[ADR-0011](0011-typed-provider-definitions.md). A node still references provider
definitions by stable id only, but per-node settings that belong to one pipeline
— which model to request, which voice, retrieval limits, tool confirmation —
live on the node instead of being smuggled through provider registration. This
removes the rule by which a language model node's model is whatever its provider
definition serves first, which today makes it impossible for two pipelines to
use one provider definition with different models.

Edges become modality-typed, and `source` and `sink` nodes declare a modality.
Validation checks that an edge connects compatible modalities rather than
relying on the runtime's hand-written expectation that recognition precedes
reasoning precedes synthesis. A graph wired `tts -> llm -> stt` fails
structurally instead of failing a stage-order assertion in the runtime.

The reasoning core's output is an **utterance**, not speech. Speech is one
rendering of an utterance, produced by a `tts` node; text is another, produced
by a text `sink`. This is what makes text and hybrid pipelines fall out of the
same model rather than requiring a second one: a text pipeline is
`source(text) -> core -> sink(text)`, a voice pipeline is
`source(audio) -> stt -> core -> tts -> sink(audio)`, and a hybrid pipeline
fans several sources into one core and one core out to several sinks. If the
core knew about speech instead, every added modality would reopen the core.

The graph model stays deliberately wider than the runtime can execute. Refusing
an expressible-but-unrunnable topology at prepare time, rather than accepting
and silently ignoring it, remains the rule; this decision changes which shapes
are expressible, not the size of the gap between expressible and executable.

**Consequences**

- `NodeKind::Tool` and `NodeKind::Memory` are removed. Tools and memory become
  bindings inside a reasoning core, so an operator attaches them to a core
  rather than wiring them as pipeline stages.
- `NodeKind::Router` is removed. Choosing between models with the conversation
  in hand is core routing policy, not a static graph fan-out, so the runtime
  refusal of router nodes is deleted rather than implemented.
- `NodeKind::Llm` is replaced by `NodeKind::Core`. A graph has exactly one core
  node for now; multi-core graphs are rejected at validation rather than at
  prepare time.
- `Node` becomes an internally tagged enum of typed variants rather than
  `{ id, kind, provider }`. Provider references remain stable provider
  definition ids.
- Cycle, duplicate-id, dangling-edge, and connectivity validation are unchanged
  and continue to apply to the transport pipeline.
- Modality compatibility becomes a validation error, and the runtime's
  `require_downstream` stage-order checks are removed as redundant.
- Speech-to-text and text-to-speech become optional stages. A plan for a text
  pipeline resolves no recognizer and no synthesizer.
- Turn reconstruction's spoken-segment vocabulary from
  [ADR-0010](0010-server-owned-turn-reconstruction.md) generalizes to utterance
  segments carrying a modality. A text pipeline produces segments that were
  never spoken, and reporting them as spoken would misdescribe the turn.
- Stored pipelines and browser-local drafts are not migrated. Operators
  recreate pipelines through the editor, consistent with the precedent set by
  [ADR-0011](0011-typed-provider-definitions.md).
- The operator console's augment partitioning, hardcoded stage ordering, and
  `"llm"` target fallback are deleted. Core tools and memory render from the
  core's own bindings instead of being recovered from nodes and edges.
