# Architecture

Conduit is a local-first voice assistant framework built around three
boundaries: pipeline definitions are data, providers are interfaces, and
runtime progress is published as events.

## Workspace Layout

| Crate | Responsibility |
| --- | --- |
| `conduit-core` | Shared identifiers, audio formats, device protocol types, event vocabulary, event bus, errors, and the serializable pipeline graph. |
| `conduit-provider` | Object-safe provider traits for speech recognition, language models, synthesis, tools, storage, wake word, speaker identification, and memory. |
| `conduit-runtime` | Resolves a graph against registered providers and runs one conversation turn from captured audio to synthesized speech. |
| `conduit-openai` | OpenAI-compatible provider implementations for chat completions, audio transcriptions, and audio speech. |
| `conduit-wyoming` | Wyoming protocol speech recognition, synthesis, and wake word detection over a TCP endpoint. |
| `conduit-speaker` | Speaker identification over HTTP: Conduit's own contract, and a client for an existing Diarization_Server. |
| `services/speaker-id` | The reference implementation of that contract, published as `conduit-speaker-id` with CPU and GPU tags. |
| `conduit-mcp` | Model Context Protocol client and tool providers over stdio, streamable HTTP, and SSE. |
| `conduit-store` | Memory, file, and PostgreSQL implementations of the pipeline store contract. |
| `conduit-metrics` | Prometheus metrics derived by subscribing to the event bus. |
| `conduit-api` | HTTP service and ops routers, authentication, pipeline CRUD, event streaming, and the conversation WebSocket. |
| `frontend` | React Operator Console app with token-based Operator Access, snapshot-plus-events client boundaries, and top-level Overview, Pipelines, Providers, Speakers, Events, and Settings sections. |

## Runtime Flow

The API loads a `PipelineGraph` from the configured store, resolves it into a
`Runner`, and upgrades the conversation route to a WebSocket only after auth,
pipeline lookup, graph resolution, and audio-format validation all succeed.

```mermaid
flowchart LR
    device["Device WebSocket"] --> api["conduit-api"]
    api --> store["PipelineStore"]
    api --> runner["Runner"]
    runner --> stt["SpeechToText"]
    stt --> llm["LanguageModel"]
    llm --> tools["Tools"]
    llm --> tts["TextToSpeech"]
    tts --> device
    runner --> bus["EventBus"]
    bus --> events["/v1/events"]
    bus --> metrics["conduit-metrics"]
```

Binary WebSocket frames carry audio in both directions. Text WebSocket frames
carry only device control messages: `end` for utterance completion and `stop`
for user-requested cancellation. Partial transcripts, final transcripts, tool
events, stage failures, token usage, cancellation reasons, and timing all travel
over the event bus instead of the socket audio path.

## Graphs

`PipelineGraph` is a serializable list of nodes and edges. Validation catches
duplicate node ids, dangling edges, cycles, missing source or sink nodes,
disconnected subgraphs, and edges whose two ends disagree about what they
carry. The graph model can describe more than the runtime can execute;
`Runner::prepare` refuses unsupported node kinds and topologies rather than
accepting and ignoring them.

Edges are typed by modality — `audio`, `text`, or `utterance`. A `source` and a
`sink` declare theirs, because nothing about a websocket says whether it carries
microphone samples or typed words; every other kind derives one from what it
does. Recognition reads audio and writes text, a model reads text and produces
an utterance, a transform reads an utterance and produces one, synthesis speaks
an utterance or plain text, and a text sink writes an utterance down. An utterance is what a model said before anything
decided how to render it, which is what keeps a model unaware of whether it will
be heard or read. A graph wired `tts -> llm -> stt` therefore fails validation
naming the offending edge, rather than failing a stage-order assertion in the
runtime.

`tool`, `memory`, and `router` nodes are not modality transforms — they are the
visible half of a call-and-return arc — so edges touching them are not checked.
[ADR-0012](adr/0012-transport-pipeline-and-reasoning-core.md) removes those
kinds rather than inventing modalities for them.

Modality compatibility is a property of a single edge, so it does not say
anything about paths. A graph wiring `stt -> llm` beside `stt -> tts` has two
compatible edges and still drops the model's answer on the floor, which is why
`Plan::resolve` still asks whether each stage is reachable from the one before
it. Core reachability replaces that check once a graph has exactly one core to
state the rule about.

A `transform` node sits between a core and whatever renders what it said. It
reads an utterance and produces one, so transforms chain and either rendering
can read the result — and which renderings a rewrite reaches is a property of
its edges. A transform wired only to the `tts` node cleans up what is spoken
and leaves a text sink showing the markdown the model actually wrote, which is
usually what a transcript is for. Accepting only an utterance is what keeps one
out of the input path: rewriting what a person said to the assistant is a
different proposition from rewriting what it says back.

`Plan::resolve` walks back from each renderer to collect its chain, rather than
forward from the core, because that is the question being asked — not which
transforms exist but what happens to what this one speaks. A transform nothing
renders through is refused there: one edge is the difference between a rewrite
that runs and one that never will. A transform that fails ends the turn rather
than passing the segment through, because a redaction that silently stops
redacting is worse than a turn that stops.

A node is a typed variant rather than a generic record, tagged by `kind`, and
carries the configuration belonging to that kind: an `llm` node names its
model, system prompt, and round cap; a `tts` node its voice; a `memory` node its
mode, scope, and limit. A node still selects a provider definition by stable id
only. Settings that belong to one pipeline live on the node, so two pipelines
may share a provider definition and request different models from it; settings
that belong to the provider live in its definition. An absent `model` means
whichever model the provider serves first, so a node that expresses no
preference behaves as it did before nodes could express one.

The same division governs provider-specific settings. A Configured Provider
carries the reusable ones — set once, applied to every pipeline that names it —
and a node carries only what this pipeline wants different, on `settings`: an
`stt` node, a `tts` node, and a core's model binding each take one. An override
is checked against the provider's declared settings schema when the pipeline is
prepared, so a mistyped setting is a graph the operator is told to fix rather
than a turn that fails. Crucially it is checked as an *override* — declared
defaults are not filled in and `required` is not enforced — because a node that
named one setting must not thereby displace every stored default beside it. What
a node leaves out stays with the Configured Provider, which layers the request's
settings over its own.

[ADR-0012](adr/0012-transport-pipeline-and-reasoning-core.md) replaces this flat
model with a transport pipeline plus a reasoning core;
[docs/specs/0001](specs/0001-transport-pipeline-and-reasoning-core.md) tracks
that work.

A `core` node is that replacement, and it currently stands beside the kinds it
replaces rather than instead of them. It holds a model binding together with
its tool and memory bindings, so what the model may reach for is configuration
on one node instead of edges describing half of a call-and-return arc. A core
occupies the same place in the transport pipeline as an `llm`: it reads text and
produces an utterance. A tool binding also says whether this pipeline wants to
be asked before that tool runs, which is not visible in the tool's own schema.

A graph reasons in one place. Validation refuses a second reasoning node,
counting `core` and `llm` nodes together, because a pipeline that reasons twice
says nothing about which answer is the reply — and the refusal is about there
being two models rather than about how the graph spells them.

Today the runtime executes at most one wake stage, one identification stage,
one recognizer, one language model, at most one synthesizer, and any number of
tool branches downstream of the model. `router` and `memory` nodes exist in the
graph vocabulary but are not runnable runtime stages yet.

A wake stage gates capture: every chunk reaches the detector and nothing
reaches the recognizer until a phrase is accepted. The gate forwards half a
second of audio from before the activation, because a detector reports some way
after the phrase ended and opening at that instant would clip the first word of
the command. Where detection runs — on a Wyoming server or on the satellite
itself — is a property of the provider definition rather than of the node.

An identification stage forks capture rather than queuing behind it: it and the
recognizer both want the whole utterance, and asking in sequence would double
how long the person waits. The identity it finds is what a tool's per-speaker
permission check sees. A detector that fails ends the turn, because a pipeline
that cannot tell whether it was addressed should not guess; an identifier that
fails does not, because not knowing who is speaking is how every pipeline
behaved before the stage existed.

Validation requires every origin to reach the core and the core to reach every
terminal, so a graph cannot branch past the model and deliver something the
model never saw. A pipeline may have several sources and several sinks: one
core can be fed from a microphone and a chat box and deliver to a speaker and
a transcript, and the same segment is then spoken and written.

Recognition and synthesis are both optional. A turn starts from audio or from
words a client typed, and delivers a `Reply` that is either synthesized speech
or a written segment, so `source(text) -> llm -> sink(text)` runs on a
deployment where the only configured provider is a language model. Absence is
what selects the modality: a graph carrying audio to a model without
transcribing it fails modality validation long before resolution. Sentence
segmentation is shared — what a voice pipeline speaks a piece at a time is what
a text pipeline writes a piece at a time.

## Providers

Providers implement traits from `conduit-provider` and are registered under a
selector — a registry key the deployment chooses, which is what a graph node
names. Provider-specific configuration lives in provider registration, not in
the pipeline graph.

Every provider describes itself through one `Descriptor`, built when the
provider is constructed:

- **id** — the stable identity the provider calls itself, used in metric labels
  and error messages. Distinct from the registry key, so the same
  implementation can be registered twice.
- **label** — the display name for operator screens. Nothing keys off it.
- **version** — surfaced in diagnostics.
- **capability** — which registry it belongs in.
- **metadata** — models, languages, voices, phrases, encodings, and whether it
  can call tools, in one shape shared by every capability. An empty list means
  *unrestricted*, not *none*.
- **settings** — a JSON Schema for the provider-specific settings a request may
  carry. A request's settings are checked against it, so a mistyped setting is
  reported rather than forwarded and silently ignored.

Because the descriptor is uniform, the status layer and the operator UI can
render and validate a provider of any capability without knowing which one it
is.

The runtime stores providers behind trait objects in registries. Dropping a
returned stream is the cancellation mechanism for provider work that is no
longer needed.

### Vendor Factories

A stored provider definition becomes a running provider through a
`ProviderFactory`: one per vendor, each saying what it is called, which
definitions it claims, and how it builds them. `Factories` enumerates the
registered factories and hands each definition to the one that claims it, so
supporting a new vendor is a new type and one line in `Factories::builtin`
rather than an edit to the code that loads every provider a deployment has.

Claims are disjoint — no two factories may claim the same definition — and a
definition nothing claims fails the load rather than being skipped, because a
provider silently missing from the registry surfaces later as a pipeline error
about the graph instead of about the definition. A deployment that embeds
Conduit supplies its own vendor set with `AppState::with_factories`.

### Provider Status

The operator status snapshot reports every registered provider of every
capability, with the selector a pipeline names, the identity, label and version
its descriptor states, and the capability it supplies. It is assembled by
walking the bundle's descriptors rather than by naming stt, llm and tts one at
a time — that is what used to leave transforms, detectors, identifiers and
memory stores out of the snapshot entirely, and would leave out the next
capability too. A provider that was never built has a selector and no
descriptor, so its identity and version are absent rather than invented.

Three states stack, each earned by different evidence:

- **Configured** — settings exist and validate. Says nothing about whether the
  service is there.
- **Reachable** — an active health check answered. Performed when definitions
  change rather than when the console polls, because a probe can mean a request
  to a paid API and the console polls.
- **Proven** — the provider did the job inside a real pipeline turn, derived
  from the event bus.

Proof is per provider, not per pipeline: a turn that failed at synthesis says
nothing about the model that answered in it, so each component keeps or loses
its own proof. A failure marks the provider — outranking a health check that
answered, since failing a real turn is the more recent and more expensive
evidence — and stands until a later successful turn proves recovery. Because
the failure is discovered through a pipeline's components, an unreachable or
unproven provider can name the pipelines it affects, which is what lets the
exception-first overview warn before the next turn fails.

## Events And Metrics

The event bus is a bounded broadcast channel. Publishers never wait for slow
subscribers. A subscriber that falls behind drops events and can report its own
drop count.

`conduit-metrics` is just another subscriber. It derives Prometheus counters,
gauges, and histograms from events that already exist, so the audio path does
not call into metrics code directly.

## HTTP Boundaries

`conduit-api` builds two routers:

- service router on `CONDUIT_BIND`, authenticated by handler extractors
- ops router on `CONDUIT_OPS_BIND`, unauthenticated for probes and Prometheus

The service router carries conversations, pipeline CRUD, validation, and event
streaming. The ops router carries `/health`, `/ready`, and `/metrics`; it must
be protected by network placement rather than bearer tokens.

## Storage

Pipeline storage is abstracted by `PipelineStore`. The memory, file, and
PostgreSQL backends share the same conformance expectations:

- names are validated on every method
- list returns only names that can later be read
- missing entries are not errors
- unreadable stored definitions are errors, not absence
- `put` reports whether it replaced an existing pipeline

PostgreSQL migrations are embedded and applied at startup. File writes use a
temporary file and rename so a crash during write does not leave a truncated
pipeline definition.

## Current Limits

The runtime does not yet route between branches or read and write memory. These
are documented as known gaps in [README.md](../README.md).
