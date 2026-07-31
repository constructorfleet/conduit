<p align="center">
  <img src="assets/logo.png" alt="Conduit" width="160">
</p>

# Conduit

A modular, vendor-neutral, local-first voice assistant framework. Every stage of the
voice pipeline is replaceable, observable, and independently scalable.

Whisper + Ollama + Piper, or Azure STT + Claude + ElevenLabs, or an entirely
local pipeline — the pipeline stays the same, only the providers change.

## Status

Early. The contracts are in place; the runtime that executes them is not.

| Crate | What it holds |
| --- | --- |
| [`conduit-core`](crates/conduit-core) | Identifiers, the event vocabulary, the event bus, the pipeline graph |
| [`conduit-provider`](crates/conduit-provider) | The traits every STT, TTS, LLM, wake word, speaker ID, tool, and memory plugin implements |
| [`conduit-runtime`](crates/conduit-runtime) | Executes a graph: audio in, speech out, events throughout |
| [`conduit-api`](crates/conduit-api) | HTTP API: pipeline CRUD and a live event stream |

## Design

Three rules explain most of the code.

**Everything is an event.** Stages publish to a bus rather than calling each
other. A publisher never blocks on a subscriber — a slow event viewer drops
events instead of stalling the audio path, and drops are counted so they stay
visible.

**A pipeline is data.** [`PipelineGraph`](crates/conduit-core/src/graph.rs) is a
serializable list of nodes and edges. The API validates and stores it, the UI
edits it, the runtime walks it. None of that requires knowing what a `whisper`
node is.

**Providers are interfaces, not special cases.** Adding ElevenLabs means
implementing [`TextToSpeech`](crates/conduit-provider/src/tts.rs) and registering
it under a name. It never means editing the pipeline.

A consequence worth spelling out: the runtime speaks each sentence as soon as
the model completes it, rather than waiting for the full response. A reply of
"Turning on the light. Anything else?" begins playing while the second sentence
is still being generated. The bounded output channel is the backpressure — if a
device stops draining audio, synthesis stops rather than buffering ahead.

## Running

```sh
cargo run -p conduit-api
```

| Variable | Default | Purpose |
| --- | --- | --- |
| `CONDUIT_BIND` | `0.0.0.0:8080` | Listen address |
| `CONDUIT_LOG` | `info` | `tracing` filter |

```sh
# Store a pipeline (rejected with 422 if it does not validate)
curl -X PUT localhost:8080/v1/pipelines/kitchen \
  -H 'content-type: application/json' -d '{
    "name": "kitchen",
    "nodes": [
      {"id": "mic", "kind": "source", "provider": "websocket"},
      {"id": "stt", "kind": "stt", "provider": "whisper"},
      {"id": "llm", "kind": "llm", "provider": "ollama"},
      {"id": "tts", "kind": "tts", "provider": "piper"}
    ],
    "edges": [
      {"from": "mic", "to": "stt"},
      {"from": "stt", "to": "llm"},
      {"from": "llm", "to": "tts"}
    ]
  }'

# Watch the pipeline run, live
curl -N localhost:8080/v1/events?stages=reasoning,tools
```

## Developing

Read [AGENTS.md](AGENTS.md) first — it is the canonical engineering standard for
this repository, and it applies to human and agent contributors alike.

These four gates are what CI runs, and what "done" requires:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo audit
```

## Next

- Reference providers: Whisper, Ollama, Piper
- Device transport (WebSocket, then gRPC)
- Persistent storage behind the pipeline store
- Wake word, speaker identification, tools, and memory in the runtime

## Known gaps

Tracked here rather than as TODOs in the source.

- **Metrics and traces.** [AGENTS.md](AGENTS.md) asks every significant
  operation to expose metrics, traces, and logs. Only structured logs and
  HTTP-level spans exist today; there is no Prometheus endpoint and no
  OpenTelemetry export.
- **The pipeline store is in-memory.** Restarting the API loses every stored
  pipeline.
- **Only linear graphs execute.** The runtime rejects router fan-out rather
  than pretending to run it; the graph model already describes it.
