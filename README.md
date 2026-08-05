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
| [`conduit-http`](crates/conduit-http) | Shared HTTP plumbing every HTTP-backed provider uses: sending, failure classification, SSE framing |
| [`conduit-openai`](crates/conduit-openai) | OpenAI-compatible models, speech recognition, and synthesis |
| [`conduit-anthropic`](crates/conduit-anthropic) | Language models over Anthropic's Messages API |
| [`conduit-bedrock`](crates/conduit-bedrock) | Language models over Amazon Bedrock's Converse API |
| [`conduit-wyoming`](crates/conduit-wyoming) | Wyoming protocol speech recognition, synthesis, and wake word detection |
| [`conduit-elevenlabs`](crates/conduit-elevenlabs) | Speech recognition and synthesis over ElevenLabs' REST API |
| [`conduit-google`](crates/conduit-google) | Speech recognition and synthesis over Google Cloud's Speech APIs |
| [`conduit-marytts`](crates/conduit-marytts) | Synthesis over a self-hosted MaryTTS server |
| [`conduit-wake`](crates/conduit-wake) | In-process wake word detection, scoring openWakeWord models with no service to run |
| [`conduit-speaker`](crates/conduit-speaker) | Speaker identification over HTTP, and a client for an existing Diarization_Server |
| [`services/speaker-id`](services/speaker-id) | The reference identification service, published as `conduit-speaker-id` |
| [`conduit-transform`](crates/conduit-transform) | The rewrites that ship with Conduit: flatten markdown, strip emoji, collapse whitespace |
| [`conduit-script`](crates/conduit-script) | The same job written by the operator: utterance transforms on a sandboxed Rhai interpreter |
| [`conduit-mcp`](crates/conduit-mcp) | Model Context Protocol tools over stdio, streamable HTTP, and SSE |
| [`conduit-metrics`](crates/conduit-metrics) | Prometheus metrics, derived from the event bus |
| [`conduit-store`](crates/conduit-store) | Storage backends for pipeline definitions |
| [`conduit-memory`](crates/conduit-memory) | What the assistant remembers: BM25 in process, or PostgreSQL with `pgvector` |
| [`conduit-api`](crates/conduit-api) | HTTP API: pipeline CRUD, a live event stream, and the conversation socket |
| [`frontend`](frontend) | React Operator Console shell and browser-side access foundation |

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

**What is spoken is the pipeline's decision, not the model's.** A model writes
for a reader — emphasis in asterisks, emoji as punctuation, links as brackets —
and "do not use emoji" holds until it does not. A `transform` node sits between
the core and whatever renders what it said, applying named rewrites
([`markdown_to_speech`, `strip_emoji`, `collapse_whitespace`](docs/configuration.md#rewriting-what-is-spoken))
to each sentence on its way out. Because it is a node, its edges say which
rendering it reaches: wire it to `tts` alone and the voice is cleaned up while
the transcript keeps the markdown the model actually wrote.

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

For local development, one script runs the API and the Operator Console
together, and stops both on Ctrl-C:

```sh
scripts/dev.sh
```

That serves the API anonymously on `127.0.0.1:8080` and the console on
`127.0.0.1:5173`, with the console's `/v1` proxy pointed at whichever API port
is in use. Real providers come from saved Provider Definitions, as they do in
any other deployment. The flags cover the two axes that change:

```sh
scripts/dev.sh --tokens secrets/tokens.json   # authenticate instead of serving openly
scripts/dev.sh --echo                         # echo providers; no speech engine needed
scripts/dev.sh --api-port 8081 --ui-port 5174 # move either listener
scripts/dev.sh --help
```

Or with Docker Compose, which is the shortest path to a working deployment:

```sh
cp .env.example .env      # then set CONDUIT_TOKENS or CONDUIT_ALLOW_ANONYMOUS
docker compose up
```

There is no open default — a server with neither a token file nor anonymous
mode refuses to start — so that copy is a step rather than a courtesy. A token
file goes in `./secrets`, which is mounted read-only and git-ignored.

Optional services sit behind compose profiles, so the default is the smallest
thing that runs:

```sh
docker compose --profile speaker-id up
```

That adds [`services/speaker-id`](services/speaker-id), the reference
implementation of Conduit's speaker identification contract, reachable from
Conduit at `http://speaker-id:8080`. It is published as
`ghcr.io/constructorfleet/conduit-speaker-id` with `latest-speechbrain` and
`latest-speechbrain-gpu` tags; set `CONDUIT_SPEAKER_ID_IMAGE` to pin one rather
than building locally. Its port is deliberately not published: Conduit reaches
it over the compose network, and a mapping would put an unauthenticated model
server on your LAN.

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

The Operator Console frontend lives in [`frontend`](frontend) and shares this
repository's release train. Its local gates are:

```sh
cd frontend
npm ci
npm run lint
npm run test
npm run contract:check
npm run build
npm run format
```

| Variable | Default | Purpose |
| --- | --- | --- |
| `CONDUIT_BIND` | `0.0.0.0:8080` | Service API listen address |
| `CONDUIT_OPS_BIND` | `0.0.0.0:9090` | Ops listen address: `/health`, `/ready`, and `/metrics`, unauthenticated |
| `CONDUIT_TOKENS` | — | Token file; required unless `CONDUIT_ALLOW_ANONYMOUS` is set |
| `CONDUIT_ALLOW_ANONYMOUS` | — | `1` serves the API to anyone who can reach it |
| `CONDUIT_LOG` | `info` | `tracing` filter |
| `CONDUIT_TURN_IDLE_TIMEOUT_SECS` | `60` | How long a turn may publish nothing before it is abandoned; `0` removes the bound |
| `CONDUIT_TURN_HISTORY_MAX_TURNS` | `500` | Completed reconstructed turns retained in memory; `0` removes the count bound |
| `CONDUIT_TURN_HISTORY_RETENTION_SECS` | `86400` | Completed reconstructed turn age retained in memory; `0` removes the age bound |
| `CONDUIT_DATA_DIR` | `$XDG_DATA_HOME/conduit` or `$HOME/.local/share/conduit` | Base directory for local Conduit data |
| `CONDUIT_DATABASE_URL` | — | PostgreSQL for pipelines; wins over a directory |
| `CONDUIT_PIPELINE_DIR` | `$CONDUIT_DATA_DIR/pipelines` | Directory to keep pipelines in; `:memory:` makes them disposable |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | — | Enables OTLP/HTTP span export to a collector |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | — | Trace-specific OTLP/HTTP endpoint; takes precedence for spans |

Product runtime providers come from saved Provider Definitions, not
environment variables. Create them through `/v1/providers` or the Operator
Console, then reference their ids from pipeline graph nodes.

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
| `POST /v1/pipelines/{name}/test-turn` | Management only |
| `GET /v1/events` | Management only |
| `GET /v1/turns`, `/v1/turns/{turn_id}`, `/v1/turns/{turn_id}/events`, `/v1/turns/live` | Management only |

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

A turn is also bounded in time, so a provider that accepts a request and never
answers cannot hold the socket for as long as the device is willing to wait.
What is bounded is *silence*, not length: every event a turn publishes counts as
progress, so a reply that keeps arriving is never given up on however long it
takes, while one that stops reporting for `CONDUIT_TURN_IDLE_TIMEOUT_SECS` (60
by default; `0` removes the bound) is abandoned. Defining progress as publishing
means one deadline covers every stage, including providers this runtime has
never heard of. The device gets a `failed` frame naming the stage that went
quiet, and the turn is cancelled as `idle_timeout` — distinct from
`user_requested`, because a wedged provider and an impatient person call for
different responses. An explicit `stop` outranks the deadline when both are due
at once: a person who pressed the button did press it.

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
| Files | A single node; one readable JSON file per pipeline | default, or `CONDUIT_PIPELINE_DIR` |
| Memory | Development; the server warns a restart will lose them | `CONDUIT_PIPELINE_DIR=:memory:` |

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
| `conduit_conversations_total` | Turns by outcome: `completed`, `user_requested` (a `stop` command), `idle_timeout` (a stage stopped reporting progress), `disconnected` (the listener left), `error` |
| `conduit_conversations_active` | Turns in progress right now |
| `conduit_tool_calls_total`, `conduit_tool_duration_seconds` | Tool volume and cost, by outcome: `completed`, `failed`, `awaiting_confirmation` |
| `conduit_tool_calls_requested_total` | Calls the model asked for; minus the outcomes above, how many are still in flight |
| `conduit_stage_failures_total` | Failures by node, and whether the pipeline recovered |
| `conduit_llm_tokens_total` | Token usage by direction |
| `conduit_events_total` | Event volume by stage — the shape of traffic, and whether a stage has gone quiet |
| `conduit_conversations_forgotten_total` | Turns evicted from tracking before they ended, so a leak of half-finished turns is visible rather than silently skewing the histograms |
| `conduit_events_dropped_total` | Events a subscriber lost to lag, labelled with which subscriber — a consumer that cannot keep up |

The collector can also label a cancellation `shutdown` or `barge_in`, but
nothing in the runtime constructs those two reasons yet, so they do not appear
on a real scrape. `barge_in` is reserved for voice detected over the assistant,
which is not implemented; a turn the client asked to stop is `user_requested`
instead.

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

Pipeline graphs do not carry provider configuration. A graph node stores the
stable provider id it selects; provider-specific settings belong to provider
registration and the Providers page.

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

The Providers page names the common ones — Ollama, vLLM, LM Studio, and
OpenRouter — as presets: the same `openai` variant with the endpoint already
filled in, and still editable. Knowing that a local Ollama is OpenAI-compatible
does not tell anyone it listens on `11434` and wants a `/v1` suffix, and a
preset is the catalogue saying so. No provider code is involved.

`conduit-anthropic` is a second implementation rather than another base URL,
because Anthropic's Messages API differs in the three places that matter: it
authenticates with an `x-api-key` header instead of a bearer token, requires a
pinned `anthropic-version`, and streams typed events that open and close content
blocks rather than uniform chunks. It also rejects `temperature` outright on
current models, so the provider does not offer that setting at all — an operator
is told when they save the definition instead of when a conversation fails.

```rust
Anthropic::new(AnthropicConfig {
    api_key: Some(std::env::var("ANTHROPIC_API_KEY")?),
    name: "claude".to_owned(),
    ..AnthropicConfig::default()
})?;
```

`conduit-bedrock` reaches the same vendor models through Amazon's Converse API,
and is a third implementation for a different reason: not the wire format but
the *credential*. Nobody types a Bedrock key. A definition names a region — the
region is the endpoint — and the AWS default chain resolves whatever the
deployment already holds: a task role, an instance profile, a named profile,
`AWS_ACCESS_KEY_ID`. An operator who types one anyway gets a bearer token used
in preference, which is what naming it was for.

```rust
Bedrock::new(BedrockConfig {
    region: "us-west-2".to_owned(),
    name: "claude-bedrock".to_owned(),
    ..BedrockConfig::default()
})
.await?;
```

Two differences worth knowing before configuring it. Converse insists that a
conversation alternate between user and assistant, which the runtime's history
does not — a memory recall, a tool result, and a spoken utterance arrive as
three consecutive user-side turns — so the provider joins them rather than
letting the API refuse the request. And it takes `temperature` and `maxTokens`
in a field of its own, so those are read from the request and are not offered as
settings; `top_k`, `thinking`, and `anthropic_beta` are, and travel as
additional model request fields.

The AWS SDK is ~40 transitive crates, so it sits behind the `bedrock` feature.
The feature is on by default; a build without it still registers the provider
and refuses by name, so an operator learns which feature is missing when they
save the definition rather than when someone speaks to it.

`conduit-script` is the one provider whose configuration is a *program*. The
three rewrites in `conduit-transform` are Rust functions somebody had to write
and release; a `script` transform definition holds a source an operator wrote,
and it applies to the next utterance.

Running operator code inside the turn loop is only acceptable because the
interpreter is boxed in. An error ends one sentence, and a hang would end every
turn on that pipeline, so a script carries a deadline — 50 ms by default, capped
at 5 s — and a script that does not finish fails its segment rather than the
conversation. The script is compiled and its deadline checked when the
*definition is saved*, so a typo is refused while it is still on screen. The
management API asks `conduit-script` rather than keeping its own copy of those
rules, because two copies are how a form comes to accept a definition that fails
to build on the next server start.

The engine is named in the definition rather than assumed. One exists — Rhai —
and storing which one it is means a second could arrive without every saved
script silently changing language. `conduit-script` is a separate crate for the
same reason `bedrock` is a feature: an interpreter is a large dependency, and a
deployment that wants only `strip_emoji` should not compile one.
[`crates/conduit-script/README.md`](crates/conduit-script/README.md) is the
language reference, including the sandbox limits and the mutating-method trap
that makes the obvious `segment.replace("cat", "dog")` one-liner fail.

Two honest limits. Transcription takes a complete recording rather than a
stream, so `OpenAiStt` buffers the utterance and reports no partial
transcripts — it genuinely has none, and inventing them would make the pipeline
look more responsive than it is. And raw Opus frames cannot be uploaded,
because Opus needs a container this code does not build; capture as PCM or
FLAC.

Speech has the same story as language models: most servers are reached by
changing `conduit-openai`'s base URL, and three vendors are not, each for a
reason that shows up in the API rather than the hostname.

| Crate | Capabilities | Why it is not a base URL |
| --- | --- | --- |
| [`conduit-elevenlabs`](crates/conduit-elevenlabs) | `stt`, `tts` | The credential is an `xi-api-key` header and the voice is a URL *path segment* |
| [`conduit-google`](crates/conduit-google) | `stt`, `tts` | The credential is not typed at all: Application Default Credentials, refreshed per request |
| [`conduit-marytts`](crates/conduit-marytts) | `tts` | A form-encoded request answering with a WAV, and no authentication anywhere |

A voice id reaching a URL path is a security boundary rather than a correctness
one: `../` in a stored definition would move the request to a different API path
with the account's credential attached. So every ElevenLabs voice is checked
against an allowlist — letters, digits, `-`, `_` — before it can reach a URL, and
the console declares the same allowlist as a pattern so the form refuses it
first. The same reasoning covers Google's language tags and voice names, which
reach a query string.

Google's credential is the interesting one to configure, because there is
nothing to configure. A definition carries no key field; the SDK's default chain
resolves whatever the host holds — a workload identity, a service account file at
`GOOGLE_APPLICATION_CREDENTIALS`, a `gcloud` login. That resolution happens when
the definition is *saved*, so an operator on a host with no credentials is told
so while they are still looking at the form. Discovery is what sits behind the
`google` feature — on by default — and only discovery: the REST plumbing is
always compiled, so a deployment that mints its own access tokens works in
either build, and a build without the feature still registers both providers and
refuses by name.

MaryTTS ships no voices, so Conduit suggests none: a default here would be wrong
on every install that did not happen to have it. PicoTTS is deliberately absent.
It is an unmaintained C library with no network interface and no streaming, so
reaching it would mean FFI and a vendored blob in exchange for worse output than
a MaryTTS container gives.

`conduit-memory` is where what the assistant remembers lives, and the two
backends are two *retrievals* rather than two places to put the same records.
`Builtin` ranks with BM25 over unigrams and needs nothing at all: no service, no
database, and — with no `path` — no file, which is what a store configured by
configuring nothing should be. `PgVector` ranks by cosine distance over an
embedding, so a question phrased in words the stored record never used still
finds it; it wants PostgreSQL with the `pgvector` extension, and degrades to
keyword ranking where the extension is missing rather than refusing to answer.
Both sit behind the same trait, so which one runs is configuration.

```rust
// No path, so nothing is written anywhere.
let memory = Builtin::builder("recall").capacity(1_000).build().await?;
```

The `builtin` store is bounded because nothing else forgets a record — the
runtime never calls `forget_conversation` — so an unbounded in-process store
grows for as long as the process runs. A `pgvector` definition supplies the
embedding width rather than discovering it: that number is what the `vector(n)`
column is declared with, and nothing can learn it before the first embedding
exists. Its connection URL may not carry a password, because a password in a
URL's userinfo has no secret field to be hidden by on a read or kept by on a
save, and would round-trip in the clear to every operator who can read the
provider list.

## Next

- gRPC and MQTT device transports alongside the WebSocket one
- Routing in the runtime

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

**A speaker is only identified when a pipeline asks.** A tool's permission
check receives an optional speaker, and it is filled in by a `speaker_id`
stage — a pipeline without one reaches every tool with no speaker, exactly as
before. An identification service that is unreachable also leaves it absent:
the turn still answers, and its per-speaker policies simply do not apply that
turn. Nothing substitutes the device or the conversation for a speaker, and
nothing should: those name which satellite is connected, and a policy satisfied
by the wrong identity is worse than one satisfied by none.

**A voice has to be enrolled before it can be identified.** The Speakers page
in the operator console is where that happens: name somebody, then record a
sample in the browser or upload a WAV. Conduit generates the speaker id and the
identification service stores it as an opaque label, so the service never holds
anyone's name and a deployment can change embedding models without every
enrolled voice becoming a stranger. Until a voice is enrolled, a `speaker_id`
stage matches nobody and every turn reaches a tool with no speaker.

**Speaker identification is remote only.** The embedding models that recognize
a voice are Python and want more memory than an ESP32 has, so there is no
on-device counterpart to a wake definition's `device` runtime. A satellite can
wake itself; it cannot tell who woke it.

**The identification threshold is not calibrated for you.** The 50% default is
a starting point. Cosine similarities from an embedding model depend on the
microphones, the room, and how much audio a turn captured, so tune
`threshold_percent` against your own voices — every `SpeakerIdentified` event
carries its confidence, including the ones that matched nobody, which is what
shows you where the two populations separate.

**Routing is graph-only.** The graph model describes `router` nodes, but the
runtime does not run them and there is no provider trait for one yet. Memory is
no longer in this list: a `memory` definition names a store, a core binds it, and
the turn retrieves before it reasons and writes after it answers.
