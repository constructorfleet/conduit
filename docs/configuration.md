# Configuration Reference

Conduit is configured with environment variables for server, authentication, and
storage concerns. Product provider configuration is saved as server-owned
Provider Definitions through the management API.

## Server

| Variable | Default | Description |
| --- | --- | --- |
| `CONDUIT_BIND` | `0.0.0.0:8080` | Service API listen address. |
| `CONDUIT_OPS_BIND` | `0.0.0.0:9090` | Ops API listen address for `/health`, `/ready`, and `/metrics`. |
| `CONDUIT_LOG` | `info` | `tracing_subscriber` filter used for structured JSON logs. |
| `CONDUIT_TURN_IDLE_TIMEOUT_SECS` | `60` | How long a turn may publish no progress events before cancellation as `idle_timeout`. `0` removes the bound. |

## Authentication

| Variable | Default | Description |
| --- | --- | --- |
| `CONDUIT_TOKENS` | unset | Path to a JSON token file. Required unless `CONDUIT_ALLOW_ANONYMOUS` is set. |
| `CONDUIT_ALLOW_ANONYMOUS` | unset | Set to `1`, `true`, or `yes` to serve the service API without tokens. Cannot be combined with `CONDUIT_TOKENS`. |

There is no open default. A server with neither variable set refuses to start.

Token file shape:

```json
{
  "devices": [
    {
      "token": "64-or-more-hex-characters-from-openssl-rand",
      "device": "kitchen",
      "pipelines": ["default"]
    }
  ],
  "management": [
    {
      "token": "another-generated-token",
      "name": "ui"
    }
  ]
}
```

Rules enforced at startup:

- tokens must be at least 32 characters
- the file must declare at least one token
- the same token cannot appear twice
- every device entry needs a `device` name
- every management entry needs a `name`
- on Unix, the token file must not be group- or world-readable

`pipelines` is optional for device tokens. Omit it to allow any pipeline; use an
empty list to allow none.

## Pipeline Storage

| Variable | Default | Description |
| --- | --- | --- |
| `CONDUIT_DATABASE_URL` | unset | PostgreSQL URL for pipeline storage. Takes precedence over `CONDUIT_PIPELINE_DIR` when the `postgres` feature is enabled. |
| `CONDUIT_DATA_DIR` | `$XDG_DATA_HOME/conduit` or `$HOME/.local/share/conduit` | Base directory for Conduit-managed local data. |
| `CONDUIT_PIPELINE_DIR` | `$CONDUIT_DATA_DIR/pipelines` | Directory for JSON pipeline files. Used when no database URL is configured. Set to `:memory:` only for disposable development storage. |
| `CONDUIT_PROVIDER_DIR` | `$CONDUIT_DATA_DIR/providers` | Directory for JSON Provider Definition files. Set to `:memory:` only for disposable development storage. |
| `CONDUIT_SPEAKER_DIR` | `$CONDUIT_DATA_DIR/speakers` | Directory for the speaker roster. Overridden by `CONDUIT_DATABASE_URL` when the `postgres` feature is enabled. Set to `:memory:` only for disposable development storage. |

If neither database nor pipeline directory is set, pipelines are stored as JSON
files in the default local data directory and survive API restarts. The server
uses memory only when `CONDUIT_PIPELINE_DIR=:memory:` is set, and logs a warning
for that disposable mode.

Provider Definitions use their own store. If `CONDUIT_PROVIDER_DIR` is unset,
definitions are stored as JSON files under the default local data directory and
survive API restarts. A server rebuilds the Runtime Provider Registry Snapshot
from those definitions during startup and after successful provider writes or
deletes.

The speaker roster — who has been enrolled, and what each of them is called —
has its own store too. A database URL wins over `CONDUIT_SPEAKER_DIR` for the
same reason it does for pipelines, and more so: the roster is what turns an id
into a person, so replicas reading different copies would answer to the wrong
name.

The `conduit-api` crate enables PostgreSQL support by default. A
`--no-default-features` build refuses to start if `CONDUIT_DATABASE_URL` is set.

## Provider Definitions

The service exposes `GET /v1/catalog/providers` so the Operator Console can
render provider-specific creation forms from backend-owned component metadata.
Operators save Provider Definitions with stable ids, then select those ids from
pipeline graph nodes. Pipeline graphs store only the selected provider id on
each node; runtime component settings do not belong to graph nodes.

Product runtime providers are not configured from `CONDUIT_OPENAI_*`
environment variables. Use saved Provider Definitions for operator-managed
providers, or compile with `dev-providers` for the direct in-memory
development seam.

### Runtime Providers Built From Definitions

Each provider definition variant is two-level: an outer `type` names the
capability and an inner `variant.type` names the vendor.

| Capability (`type`) | Vendor (`variant.type`) | Runtime provider | Endpoint |
| --- | --- | --- | --- |
| `llm` / `stt` / `tts` | `openai` | `conduit-openai` | `base_url`, `http` or `https` |
| `llm` | `anthropic` | `conduit-anthropic` | `base_url`, defaulting to the public API |
| `llm` | `bedrock` | `conduit-bedrock` | `region`: the AWS region is the endpoint |
| `stt` / `tts` | `wyoming` | `conduit-wyoming` | `url`, `tcp://host:port` |
| `stt` / `tts` | `elevenlabs` | `conduit-elevenlabs` | none: there is one ElevenLabs |
| `tts` | `deepgram` | `conduit-deepgram` | none: there is one Deepgram |
| `tts` | `polly` | `conduit-polly` | `region`: the AWS region is the endpoint |
| `stt` / `tts` | `google` | `conduit-google` | none: the Cloud Speech APIs |
| `tts` | `marytts` | `conduit-marytts` | `url`, `http` or `https` |
| `transform` | `builtin` | `conduit-transform` | none: the rules run in process |
| `transform` | `script` | `conduit-script` | none: the interpreter runs in process |
| `tool` | `mcp` | `conduit-mcp` | stdio, streamable HTTP, or SSE transport |
| `wake` | `openwakeword` / `nanowakeword` | `conduit-wyoming`, or in process | `runtime.where`: `wyoming` (`url`) or `local` (`models_dir`) |
| `wake` | `microwakeword` | `conduit-wyoming`, or the satellite | `runtime.where`: `wyoming` (`url`) or `device` (no endpoint) |
| `speaker_id` | `http` / `diarization_server` | `conduit-speaker` | `base_url`, `http` or `https` |
| `memory` | `builtin` | `conduit-memory` | none: the records are in this process |
| `memory` | `pgvector` | `conduit-memory` | `url`, `postgres://…`, plus an embedding `embedding_base_url` |

Every variant registers under its definition id, so a graph node naming the id
resolves to the provider that definition describes.

### Reaching Bedrock

`bedrock` is the one variant whose credential is usually not in the definition
at all. A definition names a `region` and a `model`; if it names neither
`profile` nor `api_key`, the AWS default chain resolves whatever the deployment
already holds, in its usual order: the environment, the shared config file, a
web identity token, a container or task role, then instance metadata. SSO
sessions and `credential_process` helpers resolve too — both features are
compiled in.

`profile` selects a named profile from the shared config file, which is also how
a deployment assumes a role: put `role_arn` and `source_profile` in the profile
and let the chain perform the `sts:AssumeRole`. There is no `role_arn` field on
the definition, so an ARN cannot be passed inline.

`api_key` is a Bedrock API key — a bearer token, not an access key pair — for a
deployment with no AWS identity to give this process. Naming it also makes bearer
auth the preferred scheme, so the key is used rather than quietly passed over in
favour of an ambient role. It is stored as a secret and redacted on read-back
like every other inline credential. See
[`crates/conduit-bedrock/README.md`](../crates/conduit-bedrock/README.md) for the
mechanism-by-mechanism table.

### Reaching A Locally Hosted Recognizer

Several of the recognizers an operator hears about — NVIDIA Canary, Qwen's audio
models, IBM Granite Speech — are not services. They are published weights, run
by loading them into a Python process (Canary through NeMo's
`ASRModel.from_pretrained()`), and no vendor offers a hosted endpoint for them.
An `stt` definition names an endpoint, so there is nothing for one to point at
until a server is in front of the weights.

None of them needs a Conduit change to be reachable, though, because the
protocol that wraps a local ASR model already has a variant. `wyoming` takes a
`url` and nothing else: the console form is **Wyoming** (descriptor id
`wyoming`), where `url` is the one required field and `model` and `streaming` are
optional. A Canary server that speaks Wyoming is reachable today by filling in
that URL.

```json
{
  "id": "canary-local",
  "label": "Canary (local)",
  "variant": {
    "type": "stt",
    "variant": {
      "type": "wyoming",
      "url": "tcp://canary-wyoming:10300",
      "model": "nvidia/canary-1b-flash"
    }
  }
}
```

The `url` must be `tcp://host:port`. Wyoming is a TCP event protocol, not HTTP,
and a `ws://` or `http://` URL is refused when the definition is saved rather
than at the first turn. The audio is sent as `pcm_s16le`, which is the one
encoding Wyoming events carry, and `model` is passed through on `audio-start` as
a hint the server may honour or ignore — it is a name the server recognizes, not
a name Conduit validates.

`streaming` decides whether partial transcripts arrive as the server recognizes,
and it asks the server before assuming:

- **Off** — no partials, whatever the server offers. One final transcript per
  utterance.
- **On** — Conduit sends a `describe` and reads the server's `info`. A server
  advertising `supports_transcript_streaming` gets asked for partials, and its
  `transcript-chunk` events arrive as they are recognized, followed by the final.
- **On, against a server that cannot stream** — the server says so, Conduit logs
  it once naming the server, and the session proceeds to a single final
  transcript. This is the designed fallback, not a misconfiguration: a
  non-streaming recognizer is a fully working recognizer. faster-whisper answers
  `describe` with `supports_transcript_streaming: False`, and a NeMo model loaded
  per utterance behaves the same way, so this is the common case for local
  weights today.

A server that answers `describe` without mentioning the capability is asked for
partials anyway. The key was added to Wyoming after transcript streaming itself,
so its absence means "did not say" rather than "cannot" — and reading silence as
a refusal would turn partials off against servers that support them. A server
that will not answer `describe` at all is treated the same way, and negotiation
never fails a turn: a recognizer that transcribes correctly is not taken out of
service for declining to introduce itself.

The handshake is a short second connection per session, not a cached lookup.
That is deliberate — a Wyoming server can be replaced under a stable address, and
a cache would keep serving the old answer until something invalidated it. The
cost is one TCP round trip against a recognizer about to do far more expensive
work, and it is only paid when `streaming` is on, since a definition that wants
no partials has nothing to negotiate.

The alternative is an OpenAI-compatible shim, which the existing `openai` STT
variant reaches with a `base_url`. **Chat-completions compatibility does not
imply audio compatibility.** A great many local servers advertise "OpenAI
compatible" and serve only `/v1/chat/completions`; Conduit posts to
`audio/transcriptions` under the `base_url`, and a server that does not serve
that path fails at the first turn with a configuration that looked correct when
it was saved. Confirm the audio endpoint specifically before choosing this
route. Note also that this variant sends a complete recording and returns one
final transcript. Its `stream` flag is **reserved and changes nothing**: there
are no partials for it to gate, because Conduit posts to `audio/transcriptions`
and reads one response. The vendor does offer a streaming transcription mode, but
it is a different request shape and a server-sent event stream rather than a
parameter, so it is a change rather than a setting. Unlike the `wyoming`
variant's `streaming`, turning this on asks for nothing and reports nothing.

The server that fronts the weights ships with Conduit:
[`services/wyoming-asr/`](../services/wyoming-asr/README.md), a Python service in
the shape of `services/speaker-id/` rather than a provider crate. Bring it up with
`docker compose --profile wyoming-asr up` and point a `wyoming` definition at
`tcp://wyoming-asr:10300`. `ASR_ENGINE` picks the backend, so adding Qwen or
Granite is a class in that service and nothing Conduit has to learn. It is the
supported wrapper, not the only reachable one — any server speaking Wyoming works.

If Canary is the model chosen, its weights are CC-BY-4.0. Commercial use is
permitted and attribution is required, so a deployment that ships it owes the
credit.

### Rewriting What Is Spoken

A model writes for a reader. It emphasises with asterisks, punctuates with
emoji, and links with brackets, and asking it not to works until it does not.
A `transform` definition names the rewrites to apply instead, so what reaches a
synthesizer is the pipeline's decision rather than the model's willingness to
comply.

```json
{
  "id": "speech-cleanup",
  "label": "Speech cleanup",
  "variant": {
    "type": "transform",
    "variant": {
      "type": "builtin",
      "rules": ["markdown_to_speech", "strip_emoji"]
    }
  }
}
```

| Rule | What it does |
| --- | --- |
| `markdown_to_speech` | Headings, emphasis, lists, tables, links and code spans become the words they wrap. A link reads as its text and not its address. |
| `strip_emoji` | Removes pictographs and respaces what is left, so the sentence reads as though the emoji was never written. Currency, arithmetic and degree signs stay: a voice reads those aloud and is meant to. |
| `collapse_whitespace` | Runs of whitespace, including line breaks, become single spaces. |

Rules run in the order they are listed, and the order matters: flattening
markdown before stripping emoji means an emoji inside a link's text is seen as
text rather than as part of an address. The console offers the rules that are
left to add and shows the chosen ones as tags in that order, each removable on
its own, so the list is built by picking from the set rather than by spelling
the names out.

A transform runs per sentence, because synthesis begins before the model has
finished writing. A construct spanning several sentences — most obviously a
fenced code block — is therefore judged one line at a time rather than
recognised as one thing.

#### A Rewrite The Operator Writes

A builtin rule is a Rust function somebody had to write and release. A `script`
definition is the other half of that trade: the operator writes the rewrite
themselves and it applies to the next utterance.

```json
{
  "id": "shouting",
  "label": "Shouting",
  "variant": {
    "type": "transform",
    "variant": {
      "type": "script",
      "engine": "rhai",
      "source": "segment.to_upper()",
      "timeout_ms": 50
    }
  }
}
```

The incoming sentence is bound to `segment` and whatever the script evaluates to
is what gets rendered; returning `""` drops the sentence, which is a normal
outcome rather than a failure. [`crates/conduit-script/README.md`](../crates/conduit-script/README.md)
is the language reference, including the mutating-method trap that makes the
obvious `segment.replace("cat", "dog")` one-liner fail.

`engine` is stored rather than assumed. One interpreter exists, and naming it in
the definition is what lets a second arrive without every saved script silently
changing language.

`timeout_ms` is a deadline rather than a suggestion, and is the reason a script
can be run inside the turn loop at all: a transform sits between a model
finishing a sentence and a synthesizer starting to speak it, so a script that
never returns would end every turn on that pipeline rather than one sentence. It
defaults to 50 ms and `conduit-script` refuses anything outside 1 ms..=5 s. The
management API refuses an out-of-range deadline, and a script that does not
compile, when the definition is *saved* — by asking `conduit-script` rather than
keeping a second copy of the rules — so a typo is caught while the script is
still on screen instead of becoming a jammed pipeline discovered by whoever
speaks to it next.

Compilation catches syntax errors and undefined variables. It does not catch an
unknown function, an unknown method, or a script that returns something that is
not a string; those surface as a failed turn on the first sentence that reaches
them.

The script's source is the definition rather than a secret in it, so it is read
back verbatim and the console reopens the program the operator wrote.

### Wake Word Detection Without A Service

A `wake` definition with an `openwakeword` type and a `local` runtime is scored
in the Conduit process: openWakeWord is three small ONNX models, and there is no
server to run. Put `melspectrogram.onnx`, `embedding_model.onnx`, and one
`<phrase>.onnx` per phrase in a directory; `scripts/fetch-wake-models.sh`
downloads a working set. A definition that names no `models_dir` reads
`wake-models` under the data directory, which is the volume the compose file
mounts.

Which phrases the detector has are whichever model files it found, named after
them: `hey_jarvis_v0.1.onnx` is the phrase `hey jarvis`. `GET
/v1/providers/{id}/phrases` reports them, and the console offers them while
editing the definition. A `phrases` list narrows what is loaded; an empty one
loads everything in the directory.

The models are loaded when the definition is saved, so a directory that is
missing or holds nothing the definition asked for is refused there rather than
at the first turn. Detection costs about 3 ms per 80 ms of audio, on a thread
of its own.

The other two engines need a service or a satellite. microWakeWord's models are
tflite-micro graphs Conduit cannot load, so it runs on the device it was built
for or on a Wyoming server. nanoWakeWord's phrase models are recurrent, carrying
LSTM state between chunks, which is a different scorer from the one Conduit has
— for now it runs on a Wyoming server, and a `local` runtime is refused with
that reason.

An MCP definition describes a *server*, which may advertise several tools. Each
advertised tool is registered as `<definition id>.<tool name>`, so a core can
bind one of them by name.

A core may instead bind the definition id itself, which names the whole server:
every tool it registered is offered to the model. That is what to write when a
pipeline should have whatever the server does — it keeps saying so when the
server grows a tool, where a list of names would have to be revisited.

Discovering those tools needs the server to answer, but saving a definition
does not require it: discovery is given five seconds, and a server that does not
answer leaves the definition saved with no tools registered. Running a
reachability test on that definition rediscovers them.

Reachability is probed automatically whenever definitions change — and again at
startup — for every capability, including MCP: a tool provider is checked
through its transport the way the explicit test checks it, so a healthy server
reads `reachable` without an operator pressing Test, however the registry is
populated.

### Remembering What Was Said

A `memory` definition names where the assistant keeps what it should remember,
and a core binds it the way it binds a tool. The two variants are two
*retrievals* rather than two places to put the same records: `builtin` ranks by
keyword, so a question phrased in words the stored record never used misses,
and `pgvector` ranks by embedding distance, so it does not.

```json
{
  "id": "household-recall",
  "label": "Household memory",
  "variant": {
    "type": "memory",
    "variant": { "type": "builtin", "capacity": 1000 }
  }
}
```

`builtin` needs nothing installed and nothing reached. A definition that names
no `path` writes nowhere and is gone at the next restart: a memory store that
silently began recording every conversation to disk is not a default anyone
should get by leaving a field blank. `capacity` bounds it — a thousand records
by default, oldest dropped first — because nothing else forgets one. The runtime
never calls `forget_conversation`, so an unbounded in-process store grows for as
long as the process runs.

`pgvector` keeps the records in PostgreSQL and needs the `postgres` feature; a
deployment with a database for pipeline definitions already has one for this. It
wants the `pgvector` extension, but does not require it — without the extension
the store still works and ranks by keyword, which is why the extension is not
part of the definition. `embedding_base_url` is an OpenAI-compatible
`/embeddings` endpoint, which Ollama serves as well as OpenAI does, and
`dimensions` is how many numbers that model's vectors have. That last one is
supplied rather than discovered because it is what the `vector(n)` column is
declared with, and nothing can learn it before the first embedding exists.

The connection URL may not carry a password, and one is refused rather than
redacted. Every other credential in a definition lives in its own secret field,
which is what lets a read hide it and a later save keep it; a password buried in
a URL's userinfo has neither, so it would be stored in the clear and handed back
in the clear to every operator who can read the provider list. `PGPASSWORD`, a
`.pgpass` file, or a password-less local role are all ways to say it somewhere
the definition does not.

## OpenTelemetry

| Variable | Default | Description |
| --- | --- | --- |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | unset | Enables OTLP/HTTP span export. |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | unset | Trace-specific OTLP/HTTP endpoint. Takes precedence for spans. |

When neither is set, Conduit still writes structured JSON logs and does not try
to connect to an OTLP collector.

## Local Development

`scripts/dev.sh` runs the API and the Operator Console as one pair and sets the
variables below from its flags, so a development loop does not need a `.env` or
an exported environment. It reads neither: a shell that already exported
`CONDUIT_TOKENS` while the script was asked for anonymous mode would hand the
server both variables at once, which it refuses to start on. Every other
`CONDUIT_*` variable passes through untouched.

| Flag | Default | Sets |
| --- | --- | --- |
| `--anonymous` | on | `CONDUIT_ALLOW_ANONYMOUS=1`, and clears `CONDUIT_TOKENS`. |
| `--tokens FILE` | — | `CONDUIT_TOKENS=FILE`, and clears `CONDUIT_ALLOW_ANONYMOUS`. Refused if the file does not exist. |
| `--echo` | off | Builds with `--features dev-providers`. Also spelled `--dev-providers`. |
| `--api-port PORT` | `8080` | `CONDUIT_BIND` and `VITE_CONDUIT_API_TARGET`. |
| `--ops-port PORT` | `9090` | `CONDUIT_OPS_BIND`. |
| `--ui-port PORT` | `5173` | The Vite dev server port. |
| `--dry-run` | off | Prints the resolved configuration and starts nothing. |

Both listeners bind `127.0.0.1` rather than the server's own `0.0.0.0` default,
so starting a development script does not publish an anonymous API to the
network. Ctrl-C stops both processes and everything they spawned.

Providers are the same as in any other deployment: real ones come from saved
Provider Definitions, and `--echo` is the opt-in for the in-memory providers
that treat audio as UTF-8 text, which cannot hear speech.

## Test-Only Variables

| Variable | Used by | Description |
| --- | --- | --- |
| `CONDUIT_TEST_POSTGRES_URL` | `conduit-store` tests | PostgreSQL database URL for store conformance tests. Tests that need it skip themselves when unset. |
| `CONDUIT_REGENERATE_FIXTURES` | firmware protocol parity tests | Regenerates checked-in firmware protocol fixtures when intentionally updating the wire contract. |
