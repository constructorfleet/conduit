# conduit-core

Core types shared by every Conduit crate.

This crate has no provider or HTTP responsibilities. It owns the vocabulary the
rest of the workspace agrees on:

- audio formats and encodings
- device control and notice frames
- identifiers for conversations, turns, traces, devices, speakers, and tools
- structured error types
- event envelopes, event variants, stages, finish reasons, and cancellation reasons
- the bounded event bus and subscription filters
- the serializable pipeline graph

## Pipeline Graphs

`PipelineGraph` is a data model, not executable code. It contains author-chosen
node ids, node kinds, provider names, provider-specific JSON config, and
directed edges.

Validation rejects:

- duplicate node ids
- dangling edges
- cycles
- missing source or sink nodes
- disconnected subgraphs

Deserialization does not validate automatically. That keeps malformed stored
graphs loadable so editors and API clients can inspect and fix them.

The runtime adds a second layer of checks when it resolves a graph. A graph can
be structurally valid and still not executable by today's runtime.

## Event Bus

`EventBus` is a bounded Tokio broadcast channel. Publishing never blocks. Slow
subscribers lose old events rather than slowing the audio path, and each
subscription tracks how many events it dropped.

Filters can narrow events by stage, conversation, device, or trace. All
populated filter fields must match.

## Device Protocol Types

`device::Command` and `device::Notice` are the JSON text-frame contract used by
the WebSocket API and firmware parity tests. Binary frames carry audio; these
types carry control and status.
