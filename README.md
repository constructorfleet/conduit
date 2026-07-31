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
| [`conduit-store`](crates/conduit-store) | Storage backends for pipeline definitions |
| [`conduit-api`](crates/conduit-api) | HTTP API: pipeline CRUD, a live event stream, and the conversation socket |

## Design

Three rules explain most of the code.

**Everything is an event.** Stages publish to a bus rather than calling each
other. A publisher never blocks on a subscriber — a slow event viewer drops
events instead of stalling the audio path. A subscription counts what it lost,
and each subscriber exports its own losses, so a consumer that falls behind
shows up on a dashboard rather than only in the logs.

**A pipeline is data.** [`PipelineGraph`](crates/conduit-core/src/graph.rs) is a
serializable list of nodes and edges. The API validates and stores it, the UI
edits it, and the runtime resolves it into providers. None of that requires
knowing what a `whisper` node is.

Edges are load-bearing, not decoration. A graph must be one connected pipeline
— a graph with no edges at all describes nothing and is refused — and the
runtime checks that its edges actually describe the order it will execute:
recognition feeding the model, the model feeding synthesis and any tools. A
pipeline wired `tts -> llm -> stt` is refused rather than quietly run forwards.
The check is reachability rather than adjacency, so a node may sit between two
stages without breaking the chain. The graph model stays the wider of the two
layers — it can express shapes the runtime cannot yet run, such as a `router`
choosing between two models — and those are refused at prepare time, with the
node named.

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

A tool decides for itself whether a call may run, and it gets the conversation
and — once a voice has been identified — the speaker to decide with. A
permission that is anything other than "allow" means the tool is not invoked
and the model is told what was refused and why. That includes
`deny_until_confirmed`, which is how a tool says it needs a human in the loop:
Conduit cannot put a question to a speaker mid-turn yet, so such a call is
refused rather than run, and the refusal is worded so a model reports the
action as *not* performed. See [Known gaps](#known-gaps).

## Running

```sh
cargo run -p conduit-api
```

The published container image is `ghcr.io/constructorfleet/conduit:latest`. Every
`main` build also tags it `sha-<commit>`, so whatever `latest` currently points
at can always be named by something immutable. The first build of a new Cargo
package version additionally tags it with that version — for example
`ghcr.io/constructorfleet/conduit:0.1.0` and
`ghcr.io/constructorfleet/conduit:v0.1.0` — and later builds of an
already-released version leave those tags alone, so a version tag names one
build forever. Bump the version to publish a new one.

Builds merged to `main` publish the same set of tags for
`ghcr.io/constructorfleet/conduit/conduit-artifacts`; that OCI package contains
the Linux `conduit-api` binary and the ESPHome firmware YAMLs.

Version policy and bump automation are documented in [VERSIONING.md](VERSIONING.md).

| Variable | Default | Purpose |
| --- | --- | --- |
| `CONDUIT_BIND` | `0.0.0.0:8080` | Service API listen address |
| `CONDUIT_OPS_BIND` | `0.0.0.0:9090` | Ops listen address: `/health`, `/ready`, and `/metrics`, unauthenticated |
| `CONDUIT_TOKENS` | — | Token file; required unless `CONDUIT_ALLOW_ANONYMOUS` is set |
| `CONDUIT_ALLOW_ANONYMOUS` | — | `1` serves the API to anyone who can reach it |
| `CONDUIT_LOG` | `info` | `tracing` filter |
| `CONDUIT_DATABASE_URL` | — | PostgreSQL for pipelines; wins over a directory |
| `CONDUIT_PIPELINE_DIR` | — | Directory to keep pipelines in; unset means memory only |
| `CONDUIT_OPENAI_BASE_URL` | the hosted API | An OpenAI-compatible server |
| `CONDUIT_OPENAI_API_KEY` | — | Bearer token; local servers rarely need one |
| `CONDUIT_OPENAI_NAME` | `openai` | Registry name, so two servers can coexist |
| `CONDUIT_OPENAI_READ_TIMEOUT_SECS` | `60` | How long a provider may go silent mid-response; `0` removes the bound |
| `CONDUIT_OPENAI_STT_MODEL` | — | Enables speech recognition, e.g. `whisper-1` |
| `CONDUIT_OPENAI_TTS_MODEL` | — | Enables synthesis, e.g. `tts-1` |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | — | Enables OTLP/HTTP span export to a collector |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | — | Trace-specific OTLP/HTTP endpoint; takes precedence for spans |

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
  -H "authorization: Bearer $CONDUIT_MANAGEMENT_TOKEN" \
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
curl -N -H "authorization: Bearer $CONDUIT_MANAGEMENT_TOKEN" \
  'localhost:8080/v1/events?stages=reasoning,tools'
```

Both routes take a management token; see [Authentication](#authentication).

## Authentication

Conduit serves two listeners, because the two things an operator needs are in
tension: every route that touches conversations or configuration must require a
credential, and `/health`, `/ready`, and `/metrics` must work without one —
probes cannot present a credential, and a Prometheus scrape that needs one is a
scrape that silently stops working when the token rotates.

| Listener | Default | Carries | Authentication |
| --- | --- | --- | --- |
| Service | `0.0.0.0:8080` | Conversations, pipelines, events | Bearer token, always |
| Ops | `0.0.0.0:9090` | `/health`, `/ready`, `/metrics` | None |

**Do not publish the ops port outside your trust boundary.** Its protection is
which network can reach it, not which credential you hold, and `/metrics`
exposes real operational intelligence: conversation counts, tool names, error
rates. Publish `8080` to your network and keep `9090` on the host — a firewall
rule, a Kubernetes `Service` that does not name 9090, or a `docker run` that maps
only 8080. The `0.0.0.0` default is chosen because the shipped artifact is a
container image, where a loopback default is one every deployment must override,
and a default everyone overrides is not a default.

Tokens come in two audiences, and the split is the load-bearing part of the
design. A device token may only open conversation sockets; a management token may
read events and manage pipelines. A device token presented to a management route
is refused, so a token extracted from a satellite's firmware image cannot read
the household's transcripts.

| Route | Accepts |
| --- | --- |
| `GET /v1/pipelines/{name}/converse` | Device, or management |
| `GET`, `PUT`, `DELETE /v1/pipelines…` | Management only |
| `POST /v1/pipelines/validate` | Management only |
| `GET /v1/events` | Management only |

The asymmetry is deliberate. A management token may open a conversation socket,
because an operator holding one is already trusted with more than a conversation
and refusing them would make the API impossible to try out by hand. A device
token on a management route is refused, which is the direction that matters.

Point `CONDUIT_TOKENS` at a JSON file:

```json
{
  "devices": [
    { "token": "…", "device": "kitchen", "pipelines": ["default"] },
    { "token": "…", "device": "office" }
  ],
  "management": [
    { "token": "…", "name": "ui" }
  ]
}
```

Each device entry names its device, which is what makes an authenticated
conversation know *which* satellite it is talking to. That name reaches the logs,
and each entry also gets a device id that is attached to every event the turn
publishes — so `/v1/events?device=` finally matches something, where before
nothing populated the field it filters on. The id is assigned when the token file
is read, so it is stable for the life of the process but not across restarts; see
[Known gaps](#known-gaps).

`pipelines` optionally restricts a device to named pipelines — a satellite in a
guest room need not reach the pipeline whose tools unlock the front door. Omit
the key for the common case of any pipeline; an empty list permits nothing.

Generate tokens rather than choosing them, and make them long:

```sh
openssl rand -hex 32
```

Tokens shorter than 32 characters are refused at startup. That floor matters more
than usual, because nothing rate-limits authentication attempts yet (see
[Known gaps](#known-gaps)) and entropy is the only defence against someone
guessing. Tokens are stored in plaintext and protected by file
permissions, so **the server refuses to start if the token file is group- or
world-readable** — `chmod 600` it. Hashing them would defend only against a
narrow disclosure, since anyone who can read the file can also read the OpenAI
key out of the process environment. A malformed file, a file declaring no tokens,
and a token appearing twice are all startup errors: a token that maps to two
identities has no correct interpretation, and an ambiguous configuration should
never run.

The file is read once at startup and not watched. Changing tokens means editing
it and restarting.

Credentials travel in the `Authorization` header only:

```sh
curl -H 'authorization: Bearer <token>' localhost:8080/v1/pipelines
```

Never in a query parameter. A token in a URL ends up in device logs — the
firmware logs the full connection URL on two failure paths — and in the request
URI that the HTTP trace layer records into spans, which may be exported to a
collector. Tokens are never logged, and the auth layer is positioned so the
`Authorization` header is not captured into a span.

Failures reuse the ordinary `{"error": …, "detail": …}` shape, with the kinds
`unauthorized` and `forbidden`:

| Situation | Response |
| --- | --- |
| Missing or malformed `Authorization` | 401 naming the expected format |
| Unrecognised token | 401, detail identical to the above |
| Valid token, pipeline not permitted | 403 naming the pipeline |

An unrecognised token gets the *same* message as a missing one on purpose: a
stranger probing the port must not learn whether a guessed token exists. A
pipeline restriction is a distinct 403 because the caller is already
authenticated, nothing further leaks, and it is the failure most likely to be
confusing in the field. Every 401 carries `WWW-Authenticate: Bearer`.

Rejections are logged at warn with the reason, and with the device or management
name when the token was recognised, so a misconfigured satellite is
investigatable.

There is no open default: a server with neither variable set refuses to start,
rather than leaving an operator who forgot the token file exposed and looking
fine. To deliberately run an open server — a development box, a network you
already trust completely — set `CONDUIT_ALLOW_ANONYMOUS=1`, which warns loudly at
every startup. Setting both it and `CONDUIT_TOKENS` is an error rather than a
guess about which was meant.

What a device token is *not* is a speaker. It proves which satellite is
connected, never who is talking — see [Known gaps](#known-gaps).

## Talking to a pipeline

`GET /v1/pipelines/{name}/converse` upgrades to a WebSocket. The protocol is
deliberately small — binary frames are audio in both directions, text frames
are JSON control messages:

```
→  <binary>                  captured audio
→  {"type":"end"}            the utterance is over
→  {"type":"stop"}           stop talking and end the turn
←  {"type":"started", "conversation":"…"}
←  <binary>                  the reply, as each sentence is synthesized
←  {"type":"done"}
```

Control frames are read for the whole turn, not only until `end`, because the
useful moment to say "stop talking" is while the assistant is talking. A turn
ended by `stop` is cancelled as `user_requested`; one whose socket simply died
is cancelled as `disconnected`. Keeping those apart is the point — a metric that
merged them could not answer how often people interrupt.

Everything else about the turn — partial transcripts, tool calls, timings —
is on `/v1/events`, tagged with the conversation id the socket announces. That
keeps the audio path free of anything that is not audio, and it is why the
socket names the conversation before sending a single sample.

The capture/playback format defaults to 16 kHz mono signed 16-bit little-endian
PCM. A device may negotiate a different format with query parameters:

```text
/v1/pipelines/kitchen/converse?encoding=pcm_f32_le&sample_rate=48000&channels=2
```

Unsupported provider encodings are refused before the socket upgrades.

A missing or unrunnable pipeline is refused with an HTTP status *before* the
upgrade, so a client never has to diagnose a socket that opens and then dies.
So is a missing, unrecognised, or insufficiently privileged device token — the
credential is presented as an `Authorization` header on the upgrade request, and
a device restricted away from the pipeline is refused before the pipeline is even
looked up, so a 404 cannot be used to learn which pipelines exist.

Sat1 and VoicePE firmware integration targets live in [`firmware`](firmware).
They are Conduit WebSocket targets, not Home Assistant Assist, Tater native
satellite, ESPHome voice-assistant, or wake-audio UDP firmware.

## Storage

Three backends, chosen by configuration:

| Backend | When | Set |
| --- | --- | --- |
| PostgreSQL | More than one API replica, or you already run one | `CONDUIT_DATABASE_URL` |
| Files | A single node; one readable JSON file per pipeline | `CONDUIT_PIPELINE_DIR` |
| Memory | Development; the server warns a restart will lose them | neither |

A database wins over a directory, because shared state is what more than one
replica needs and a directory only this process can see. Migrations are
embedded and run at startup, so a new replica needs no side-car — and they are
idempotent, so every replica running them is a no-op rather than a race.

Writes are a single `INSERT … ON CONFLICT DO UPDATE`, so two replicas saving at
once cannot interleave a read and a write into a lost update. Graphs are stored
as `jsonb` rather than shredded into tables: the editor round-trips the whole
document, nothing queries inside it, and shredding would cost a migration every
time a node kind is added — while still leaving operators able to ask
`SELECT graph->>'name'`.

PostgreSQL support is on by default; `--no-default-features` drops it, and a
build without it refuses to start when `CONDUIT_DATABASE_URL` is set rather
than silently keeping pipelines in memory.

All three implement the same [`PipelineStore`](crates/conduit-provider/src/storage.rs)
trait and are held to the same expectations, so which one a deployment uses is
configuration rather than behaviour. File writes go to a temporary file and are
renamed, so a crash mid-write leaves the previous definition intact rather than
a truncated one.

Pipeline names are validated before they reach a backend: a name arrives from a
URL path and becomes a file name, so `../../etc/passwd` is refused with a 422
rather than being allowed to escape the directory.

## Observability

`/health` is a liveness probe, and `/ready` verifies that the pipeline store can
answer. `/metrics` serves Prometheus text on the ops listener —
`localhost:9090/metrics` by default, with no credential, alongside both probes. See
[Authentication](#authentication) for why, and for the obligation not to publish
that port outside your trust boundary.

Nothing in the pipeline calls into the metrics crate: every stage already
publishes what it did, so the collector is an ordinary bus subscriber. A new
event is counted the day it is added, and the audio path never pays for
instrumentation it does not know about.

| Metric | What it answers |
| --- | --- |
| `conduit_time_to_first_audio_seconds` | How long before the assistant *started* speaking — the latency a person actually feels |
| `conduit_turn_duration_seconds` | How long a whole turn took, by outcome |
| `conduit_conversations_total` | Turns by outcome: `completed`, `user_requested` (a `stop` command), `disconnected` (the listener left), `error` |
| `conduit_conversations_active` | Turns in progress right now |
| `conduit_tool_calls_total`, `conduit_tool_duration_seconds` | Tool volume and cost, by outcome: `completed`, `failed`, `awaiting_confirmation` |
| `conduit_tool_calls_requested_total` | Calls the model asked for; minus the outcomes above, how many are still in flight |
| `conduit_stage_failures_total` | Failures by node, and whether the pipeline recovered |
| `conduit_llm_tokens_total` | Token usage by direction |
| `conduit_events_total` | Event volume by stage — the shape of traffic, and whether a stage has gone quiet |
| `conduit_conversations_forgotten_total` | Turns evicted from tracking before they ended, so a leak of half-finished turns is visible rather than silently skewing the histograms |
| `conduit_events_dropped_total` | Events a subscriber lost to lag, labelled with which subscriber — a consumer that cannot keep up |

The collector can also label a cancellation `idle_timeout`, `shutdown`, or
`barge_in`, but nothing in the runtime constructs those reasons yet, so they do
not appear on a real scrape. `barge_in` is reserved for voice detected over the
assistant, which is not implemented; a turn the client asked to stop is
`user_requested` instead.

Earlier versions labelled *every* turn that lost its listener `barge_in`,
whether the client interrupted or its connection died. Those are now
`user_requested` and `disconnected` respectively. A dashboard panel or alert
filtering on `outcome="barge_in"` will go quiet after upgrading; point it at
`user_requested` for interruptions, `disconnected` for lost clients, or both.

Set `OTEL_EXPORTER_OTLP_ENDPOINT` or `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` to
export HTTP and runtime spans to an OpenTelemetry collector. Without either
variable, Conduit keeps the same structured JSON logs and does not try to
connect to a collector.

## Developing

Read [AGENTS.md](AGENTS.md) first — it is the canonical engineering standard for
this repository, and it applies to human and agent contributors alike.

These four gates are what CI runs, and what "done" requires:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo audit
```

`cargo audit` is configured by [.cargo/audit.toml](.cargo/audit.toml). One
advisory is ignored there, with a command anyone can run to re-check that the
reason still holds.

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
the provider name, differently configured servers can be registered side by
side — a local model and a hosted one — though today only one of them can be
reached from a given pipeline, because the runtime executes a single `llm` node
per turn.

Two honest limits. Transcription takes a complete recording rather than a
stream, so `OpenAiStt` buffers the utterance and reports no partial
transcripts — it genuinely has none, and inventing them would make the pipeline
look more responsive than it is. And raw Opus frames cannot be uploaded,
because Opus needs a container this code does not build; capture as PCM or
FLAC.

## Next

- gRPC and MQTT device transports alongside the WebSocket one
- Wake word, speaker identification, and memory in the runtime

## Known gaps

Tracked here rather than as TODOs in the source, because a limit someone can
read is cheaper than a limit someone discovers in production.

**Nothing rate-limits authentication.** The API authenticates and authorizes
every caller (see [Authentication](#authentication)), but a client that guesses
wrong is refused and free to guess again immediately, as fast as the network
allows. Long generated tokens are the only defence, which is why the server
refuses ones shorter than 32 characters. Nor is there any rate limiting on
anything else: an authenticated caller can rewrite pipelines or open sockets in a
loop.

**Tokens are static and read once.** There is no rotation, no expiry, and no
revocation short of editing the token file and restarting. A token is plaintext
in that file, so its protection is the file mode, which the server checks at
startup and never rechecks.

**A device id does not survive a restart.** Each token-file device entry is
assigned a fresh id when the file is read, so `/v1/events?device=` matches within
one run of the server but the id it matched yesterday means nothing today, and
nothing joins events across a restart. There is also no route that reports which
id belongs to which device name, so finding one means reading it off an event.
Both follow from tokens being a file rather than a device registry.

**Conduit serves plain HTTP.** TLS termination belongs to a proxy. On a
plaintext LAN a bearer token is sniffable — an accepted risk for a local-first
appliance, not a solved problem.

**One node of each kind.** A second `llm` or `tts` node is rejected as a
duplicate, so the two-model arrangement described under
[Providers](#providers) cannot yet be expressed as a runnable graph. The graph
model can describe it, and a `router` node choosing between the two validates
as a graph; the runtime refuses both, so the shape is expressible before it is
executable rather than silently mis-run.

**Nothing times out.** A speech or model provider that accepts a request and
never answers stalls the turn for as long as the client stays connected. The
bounded output channel bounds memory, not time.

**Barge-in is not detected, only requested.** A client can say `stop`, and that
turn is cancelled as `user_requested`. What no one does is *notice* someone
speaking over the assistant: nothing runs voice activity detection during
playback, so the `barge_in` reason has no emitter. A device that wants the
interrupting behaviour has to decide on its own that it heard something and send
`stop`.

**A tool cannot ask before it acts.** A tool that needs a human in the loop
marks itself `deny_until_confirmed`, and there is nowhere to put the question:
answering one would need a device to send a control message mid-turn and a
bounded wait for the reply, neither of which exists. So such a call is refused
outright. The refusal is deliberately blunt — the model is told the tool was
*not* run — because anything ambiguous reads to a model as permission granted,
and it will announce a door unlocked. Operators see these as the
`awaiting_confirmation` tool outcome; read it as "blocked on a human", not
"waiting for an answer".

**Nothing identifies a speaker, so tools cannot enforce per-speaker policy.**
A tool's permission check receives an optional speaker, and it is always absent:
no speaker identification provider exists and nothing runs one, so a tool that
would allow an action for one household member and refuse it for another has
nothing to branch on. The path from a turn to the permission check exists and is
tested, so registering a provider is the remaining work — but until then, treat
every voice as unidentified. Nothing substitutes the device or the conversation
for a speaker, and nothing should: those name which satellite is connected, and
a policy satisfied by the wrong identity is worse than one satisfied by none.

**Wake word, speaker identification, memory, and routing are graph-only.** The
graph model describes those nodes, but the runtime refuses them. For wake word,
speaker identification, and memory the provider traits exist, and what is
missing is any implementation of them and the runtime wiring to run one. For
`router` there is no trait yet either.

**Several event variants have no emitter yet.** `WakeWordDetected`,
`WakeWordRejected`, `AudioStarted`, `AudioChunkReceived`, `AudioFinished`,
`SpeakerIdentified`, and `TurnStarted` are part of the vocabulary but nothing
publishes them, so `/v1/events?stages=capture` is a valid subscription to a
permanently silent stream. The stages that do carry traffic today are
`transcription`, `conversation`, `reasoning`, `tools`, `synthesis`, and
`diagnostics`.
