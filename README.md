<p align="center">
  <img src="assets/logo.png" alt="Conduit" width="160">
</p>

# Conduit

A modular, vendor-neutral, local-first voice assistant framework. Every stage of the
voice pipeline is replaceable, observable, and independently scalable.

Whisper + Ollama + Piper, or Azure STT + Claude + ElevenLabs, or an entirely
local pipeline — the pipeline stays the same, only the providers change.

## Status

Early, but end to end: a device can open a socket, speak, and be answered.
What is missing is real speech — there is no Whisper or Piper provider yet, so
the only complete pipelines today run on a language model plus the in-memory
echoes described under [Running](#running).

| Crate | What it holds |
| --- | --- |
| [`conduit-core`](crates/conduit-core) | Identifiers, the event vocabulary, the event bus, the pipeline graph |
| [`conduit-provider`](crates/conduit-provider) | The traits every STT, TTS, LLM, wake word, speaker ID, tool, and memory plugin implements |
| [`conduit-runtime`](crates/conduit-runtime) | Executes a graph: audio in, speech out, events throughout |
| [`conduit-openai`](crates/conduit-openai) | OpenAI-compatible models, speech recognition, and synthesis |
| [`conduit-metrics`](crates/conduit-metrics) | Prometheus metrics, derived from the event bus |
| [`conduit-api`](crates/conduit-api) | HTTP API: pipeline CRUD, a live event stream, and the conversation socket |

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

## Tools

Models interleave speech and tool calls, and the runtime treats that as the
normal case:

```
"Sure, let me look that up for you."   ← spoken immediately
        │
        ├── search(...)                ← runs *while* the preamble plays
        │
"It's sunny and 24 degrees."           ← spoken when the model answers
```

The preamble is only worth saying if it overlaps the work, so speech and tool
execution are joined rather than sequenced — a tool that blocks until speech
starts still completes. Tools requested together run together.

Every call produces a result for the model, whatever happens. A tool that
fails, is denied by its permission check, or does not exist is reported back as
a result rather than ending the turn, so the assistant can explain itself
instead of going silent. Turns are capped at `max_tool_rounds` model calls
(default 4) so a model that will not stop calling tools cannot loop forever.

## Running

```sh
cargo run -p conduit-api
```

| Variable | Default | Purpose |
| --- | --- | --- |
| `CONDUIT_BIND` | `0.0.0.0:8080` | Listen address |
| `CONDUIT_LOG` | `info` | `tracing` filter |
| `CONDUIT_OPENAI_BASE_URL` | the hosted API | An OpenAI-compatible server |
| `CONDUIT_OPENAI_API_KEY` | — | Bearer token; local servers rarely need one |
| `CONDUIT_OPENAI_NAME` | `openai` | Registry name, so two servers can coexist |
| `CONDUIT_OPENAI_STT_MODEL` | — | Enables speech recognition, e.g. `whisper-1` |
| `CONDUIT_OPENAI_TTS_MODEL` | — | Enables synthesis, e.g. `tts-1` |

Nothing is registered unless it is asked for, and a model named without a
server to run it on stops the server at startup rather than failing halfway
through someone's first sentence. A whole local pipeline:

```sh
CONDUIT_OPENAI_BASE_URL=http://localhost:8000/v1 \
CONDUIT_OPENAI_STT_MODEL=Systran/faster-whisper-small \
CONDUIT_OPENAI_TTS_MODEL=piper \
cargo run -p conduit-api
```

To hold a conversation without any speech engine or model server, build with
the `dev-providers` feature. It registers in-memory providers that treat audio
as UTF-8 text, so you can talk to a pipeline with a text WebSocket client:

```sh
cargo run -p conduit-api --features dev-providers
```

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

## Talking to a pipeline

`GET /v1/pipelines/{name}/converse` upgrades to a WebSocket. The protocol is
deliberately small — binary frames are audio in both directions, text frames
are JSON control messages:

```
→  <binary>                  captured audio
→  {"type":"end"}            the utterance is over
←  {"type":"started", "conversation":"…"}
←  <binary>                  the reply, as each sentence is synthesized
←  {"type":"done"}
```

Everything else about the turn — partial transcripts, tool calls, timings —
is on `/v1/events`, tagged with the conversation id the socket announces. That
keeps the audio path free of anything that is not audio, and it is why the
socket names the conversation before sending a single sample.

A missing or unrunnable pipeline is refused with an HTTP status *before* the
upgrade, so a client never has to diagnose a socket that opens and then dies.

## Observability

`/metrics` serves Prometheus text. Nothing in the pipeline calls into the
metrics crate: every stage already publishes what it did, so the collector is
an ordinary bus subscriber. A new event is counted the day it is added, and the
audio path never pays for instrumentation it does not know about.

| Metric | What it answers |
| --- | --- |
| `conduit_time_to_first_audio_seconds` | How long before the assistant *started* speaking — the latency a person actually feels |
| `conduit_turn_duration_seconds` | How long a whole turn took, by outcome |
| `conduit_conversations_total` | Turns by outcome: completed, barge-in, error, timeout |
| `conduit_conversations_active` | Turns in progress right now |
| `conduit_tool_calls_total`, `conduit_tool_duration_seconds` | Tool volume and cost |
| `conduit_stage_failures_total` | Failures by node, and whether the pipeline recovered |
| `conduit_llm_tokens_total` | Token usage by direction |

Distributed tracing is not wired up yet — see [Known gaps](#known-gaps).

## Developing

Read [AGENTS.md](AGENTS.md) first — it is the canonical engineering standard for
this repository, and it applies to human and agent contributors alike.

These four gates are what CI runs, and what "done" requires:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo audit
```

## Providers

`conduit-openai` implements the three APIs that come closest to a lingua franca
among model and speech servers, so one implementation of each reaches a great
many of them:

| Capability | Endpoint | Also served by |
| --- | --- | --- |
| `OpenAi` | `/chat/completions` | Ollama, vLLM, LM Studio, OpenRouter |
| `OpenAiStt` | `/audio/transcriptions` | Speaches, `whisper.cpp`, `faster-whisper` |
| `OpenAiTts` | `/audio/speech` | `openedai-speech`, which fronts Piper |

Only the base URL changes:

```rust
// A local Ollama server, no key needed.
OpenAi::new(OpenAiConfig {
    base_url: "http://localhost:11434/v1".to_owned(),
    name: "ollama".to_owned(),
    ..OpenAiConfig::default()
})?;
```

A configuration describes one *server*, not one capability, so a host serving
all three is described once and used three times. Because the registry keys on
the provider name, differently configured servers also coexist in one pipeline:
a local model for most turns and a hosted one for the hard questions.

Two honest limits. Transcription takes a complete recording rather than a
stream, so `OpenAiStt` buffers the utterance and reports no partial
transcripts — it genuinely has none, and inventing them would make the pipeline
look more responsive than it is. And raw Opus frames cannot be uploaded,
because Opus needs a container this code does not build; capture as PCM or
FLAC.

## Next

- gRPC and MQTT device transports alongside the WebSocket one
- Persistent storage behind the pipeline store
- Wake word, speaker identification, and memory in the runtime

## Known gaps

Tracked here rather than as TODOs in the source.

- **No distributed tracing.** Metrics and structured logs are in place, and
  every event already carries a trace id, but nothing exports spans to an
  OpenTelemetry collector yet.
- **The pipeline store is in-memory.** Restarting the API loses every stored
  pipeline.
- **Only linear graphs execute.** The runtime rejects router fan-out rather
  than pretending to run it; the graph model already describes it.
- **`Permission::Confirm` is refused, not asked.** Asking the speaker to
  confirm a tool needs a turn-taking exchange the runtime does not have yet, so
  a tool that asks for confirmation is currently denied with that explanation.
- **`ToolOutput::spoken` is ignored.** The model receives the structured value;
  the tool's own suggested phrasing is not yet spoken directly.
