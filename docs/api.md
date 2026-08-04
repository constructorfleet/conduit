# API Reference

Conduit exposes a service API and an ops API. The service API is authenticated;
the ops API is intentionally unauthenticated and must be protected by where it
is bound and what the network publishes.

## Listeners

| Listener | Default | Routes | Authentication |
| --- | --- | --- | --- |
| Service | `0.0.0.0:8080` | `/v1/status`, `/v1/events`, `/v1/catalog/providers`, `/v1/providers`, `/v1/providers/{id}`, `/v1/providers/{id}/rename`, `/v1/providers/{id}/test`, `/v1/providers/{id}/voices`, `/v1/pipelines`, `/v1/pipelines/{name}`, `/v1/pipelines/validate`, `/v1/pipelines/{name}/test-turn`, `/v1/pipelines/{name}/converse` | Bearer token unless anonymous mode is explicitly enabled |
| Ops | `0.0.0.0:9090` | `/health`, `/ready`, `/metrics` | None |

Service responses use JSON for ordinary API errors:

```json
{
  "error": "not_found",
  "detail": "no pipeline named `kitchen`"
}
```

Malformed JSON, unsupported content type, and oversized request bodies use the
same shape. Request bodies on the service router are capped at 1 MiB. Service
requests have a 30 second router-level timeout.

## Authentication

Use the `Authorization` header:

```http
Authorization: Bearer <token>
```

Device tokens may open conversation sockets. Management tokens may manage
pipelines, read events, and read operator status snapshots. Management tokens
may also open a conversation for manual testing. Device tokens cannot call
management routes.

| Situation | Status |
| --- | --- |
| Missing, malformed, or unknown bearer token | `401 Unauthorized` with `WWW-Authenticate: Bearer` |
| Device token on a management route | `403 Forbidden` |
| Device restricted away from the requested pipeline | `403 Forbidden` |

## Operator Status

### `GET /v1/status`

Returns the coherent operator status snapshot used by the Operator Console
before it subscribes to `/v1/events`. This route is a management route: a
management bearer token may read it, device tokens may not, and anonymous mode
only applies when the server has explicitly been configured as open.

The snapshot contract is defined by `conduit_api::status::OperatorStatusSnapshot`.
The response shape is:

```json
{
  "generated_at": "2026-08-01T01:02:03Z",
  "runtime": {
    "launch_state": "operations_workspace",
    "stale_state": "fresh"
  },
  "pipelines": [
    {
      "name": "kitchen",
      "usable": true,
      "health": {
        "state": "unhealthy",
        "summary": "speech synthesis failed after the model completed",
        "last_successful_turn": null,
        "last_failed_turn": "00000000-0000-0000-0000-000000000003"
      },
      "components": [
        {
          "kind": "reasoning",
          "provider": "openai-primary",
          "state": "healthy",
          "detail": "last invoked turn completed",
          "last_turn": "00000000-0000-0000-0000-000000000003"
        },
        {
          "kind": "synthesis",
          "provider": "piper-local",
          "state": "unhealthy",
          "detail": "connection refused",
          "last_turn": "00000000-0000-0000-0000-000000000003"
        }
      ],
      "affected_providers": ["piper-local"]
    }
  ],
  "providers": [
    {
      "id": "piper-local",
      "kind": "tts",
      "state": "configured",
      "configured": true,
      "reachable": false,
      "proven_by_turn": null,
      "message": "no successful reachability check yet",
      "affects_pipelines": ["kitchen"]
    }
  ],
  "satellites": {
    "connected": [],
    "recently_active": [
      {
        "device": "00000000-0000-0000-0000-000000000001",
        "name": "Kitchen Satellite",
        "last_seen_at": "2026-08-01T01:01:58Z",
        "last_event": "TtsStarted"
      }
    ],
    "recent_window_seconds": 300
  },
  "active_turns": [
    {
      "pipeline": "kitchen",
      "conversation": "00000000-0000-0000-0000-000000000002",
      "turn": "00000000-0000-0000-0000-000000000003",
      "trace": "00000000-0000-0000-0000-000000000004",
      "started_at": "2026-08-01T01:01:59Z",
      "invoked_components": ["reasoning", "synthesis"]
    }
  ],
  "recent_failures": [
    {
      "pipeline": "kitchen",
      "turn": "00000000-0000-0000-0000-000000000003",
      "component": "synthesis",
      "provider": "piper-local",
      "message": "connection refused",
      "at": "2026-08-01T01:02:01Z"
    }
  ],
  "event_stream": {
    "route": "/v1/events",
    "stale_state_on_disconnect": "stale",
    "refresh_snapshot_after_reconnect": true,
    "bindings": [
      {
        "resource": "pipeline_health",
        "events": [
          "TurnStarted",
          "StageFailed",
          "ConversationCompleted",
          "ConversationCancelled"
        ]
      },
      {
        "resource": "active_turns",
        "events": [
          "TurnStarted",
          "ConversationCompleted",
          "ConversationCancelled"
        ]
      },
      {
        "resource": "recent_failures",
        "events": ["StageFailed", "ConversationCompleted"]
      },
      {
        "resource": "provider_status",
        "events": [
          "SpeechFinal",
          "LlmFinished",
          "ToolCompleted",
          "TtsFinished",
          "ConversationCompleted"
        ]
      },
      {
        "resource": "satellite_status",
        "events": [
          "ConversationStarted",
          "AudioStarted",
          "ConversationCompleted",
          "ConversationCancelled"
        ]
      }
    ]
  }
}
```

State vocabulary:

| Field | Values | Meaning |
| --- | --- | --- |
| `runtime.launch_state` | `first_run_setup`, `operations_workspace` | Whether the UI should open Guided Setup or the Operations Workspace |
| `runtime.stale_state` | `fresh`, `stale` | Whether the browser view is live or preserving last known state after stream loss |
| `pipelines[].health.state` | `not_runnable`, `unproven`, `healthy`, `degraded`, `unhealthy` | Pipeline Health from runnable configuration and real turn outcomes |
| `pipelines[].components[].state` | `not_configured`, `unused`, `unproven`, `healthy`, `degraded`, `unhealthy` | Component Health explaining the pipeline state |
| `providers[].state` | `unavailable`, `configured`, `reachable`, `proven` | Provider Status; configured settings, reachability checks, and real turn proof are separate |

Provider Status is projected from saved Provider Definitions, explicit
reachability checks, runtime provider registrations, and stored pipeline graph
references. A saved Provider Definition is Configured until
`POST /v1/providers/{id}/test` records a usable health result. A usable result
marks the provider Reachable; an unhealthy result leaves it Configured with the
reason in `message`. Proven Provider status comes only from a completed
successful turn that invoked the provider's component in a real pipeline.
Missing core runtime capabilities are reported as Unavailable provider slots,
and graph references to unregistered providers are reported as Unavailable
references.

Connected satellites are devices with an open conversation connection right
now. Recently active satellites are devices that emitted events inside the
operator-facing recent activity window, whether or not they remain connected.
Connected satellite names come from the authenticated device identity. Recent
activity that has no known device name uses the device id as its display name.
Satellite identity is device identity only; it is not speaker identity.

Satellite status is an in-memory runtime projection. After a process restart,
Connected Satellites starts empty because no WebSocket from the old process can
still be open, and Recently Active Satellites starts empty until new attributed
events arrive. The event stream then keeps the snapshot current for this
process lifetime.

A Successful Turn means every component actually invoked by that turn completed
without unrecovered error. Optional components that were not invoked, such as
tools during a no-tool turn, are ignored for that turn's success calculation.
A failed turn keeps the affected pipeline unhealthy until a later Successful
Turn proves recovery for the invoked failing path.

Snapshot-plus-events rule: the UI loads `/v1/status` first, then applies events
from `/v1/events` according to `event_stream.bindings`. If the event stream
disconnects, the UI must keep the last known view but mark it with Stale State.
After reconnect, the UI refreshes `/v1/status` before applying new events.

## Provider Routes

### `GET /v1/catalog/providers`

Lists known Provider Component Catalog entries and the configuration schema each
component accepts. The Operator Console uses this catalog to render Provider
Definition creation forms on the Providers page. Pipeline graphs then refer to
saved Provider Definition ids from `graph.nodes[].provider`.

Success body:

```json
{
  "components": [
    {
      "id": "openai.responses",
      "label": "OpenAI Responses",
      "kind": "llm",
      "definition_variant": "openai",
      "schema": {
        "properties": {
          "base_url": { "type": "string", "format": "url" },
          "api_key": { "type": "string" },
          "model": { "type": "string", "pattern": "[a-z0-9.]+" },
          "streaming": { "type": "boolean" }
        },
        "required": ["base_url", "model"]
      }
    },
    {
      "id": "anthropic.messages",
      "label": "Anthropic Messages",
      "kind": "llm",
      "definition_variant": "anthropic",
      "schema": {
        "properties": {
          "base_url": { "type": "string", "format": "url" },
          "api_key": { "type": "string" },
          "model": { "type": "string", "pattern": "[A-Za-z0-9._:/-]+" },
          "streaming": { "type": "boolean" }
        },
        "required": ["model"]
      }
    }
  ]
}
```

Two components can share a `kind` and differ in `definition_variant`, as the two
language model entries above do: a component is a shape to fill in, and the
variant is the wire format the saved definition names. `base_url` is required
for an OpenAI-compatible endpoint, which could be anywhere, and optional for
Anthropic's, which defaults to the public API.

### `GET /v1/providers`

Lists saved Provider Definition ids.

Success body:

```json
["openai-primary", "piper-local"]
```

### `GET /v1/providers/{id}`

Returns one saved Provider Definition with inline secrets redacted.

Success body:

```json
{
  "id": "openai-primary",
  "label": "OpenAI Primary",
  "kind": "llm",
  "variant": {
    "type": "llm",
    "variant": {
      "type": "openai",
      "base_url": "https://api.openai.com/v1",
      "api_key": { "type": "redacted" },
      "models": ["gpt-4.1"],
      "streaming": true
    }
  }
}
```

### `PUT /v1/providers/{id}`

Creates or replaces a typed Provider Definition. The request body id must match
the route id. Saving a Provider Definition rebuilds the active Runtime Provider
Registry Snapshot for new validations and turns. Save validates the typed shape
but does not perform a reachability check.

Inline secrets are accepted on writes but are redacted from read responses.
Sending `{ "type": "redacted" }` for an existing secret keeps the stored secret;
omitting or nulling the secret field clears it.

A provider definition variant is two-level: an outer `type` names the
capability and an inner `variant.type` names the vendor. Each variant registers
a Runtime Provider under the definition id:

| Capability (`type`) | Vendor (`variant.type`) | Registers | Notes |
| --- | --- | --- | --- |
| `llm`, `stt`, `tts` | `openai` | One provider under the definition id | |
| `stt`, `tts` | `wyoming` | One provider under the definition id | `url` must be `tcp://host:port` |
| `wake` | `openwakeword`, `nanowakeword` | One wake word detector under the definition id | `runtime.where` is `local` or `wyoming` |
| `wake` | `microwakeword` | One wake word detector under the definition id | `runtime.where` is `device` or `wyoming` |
| `speaker_id` | `http` | One speaker identifier under the definition id | `base_url` must be `http` or `https` |
| `speaker_id` | `diarization_server` | One speaker identifier under the definition id | For an existing [Diarization_Server](https://github.com/CptCamembert/Diarization_Server); `base_url` must be `http` or `https` |
| `tool` | `mcp` | One tool provider per tool the server advertises | Requires tool discovery, see below |

A `wake` definition names its detector as the variant — `openwakeword`,
`nanowakeword`, or `microwakeword` — and where that detector runs as a
`runtime` inside it. The engine is the variant rather than a field beside the
place because the three do not run in the same places, and a shape that let
them be chosen independently could describe a detector that does not exist:

```jsonc
{ "type": "wake", "variant": {
    "type": "openwakeword",
    "runtime": { "where": "local", "models_dir": "/var/lib/conduit/wake-models",
                 "threshold_percent": 50 },
    "phrases": ["hey jarvis"] } }
```

- `where: "wyoming"` hands audio to a Wyoming server at `url`, which must be
  `tcp://host:port`. Every engine is packaged as one.
- `where: "local"` scores the models in the Conduit process, with no service to
  run. openWakeWord and nanoWakeWord only: microWakeWord's models are
  tflite-micro graphs Conduit cannot load. `models_dir` defaults to
  `wake-models` under the data directory.
- `where: "device"` describes a satellite that wakes itself. microWakeWord
  only, being the one engine small enough for that hardware. There is no
  endpoint because there is no server, and no `threshold_percent` because there
  is nothing left to score: the device only streams once it has activated, so
  the pipeline's wake stage accepts immediately and publishes the activation the
  device already made. It exists so a pipeline can *say* it wakes on-device, and
  have the stage be visible in the editor, in validation, and on the event
  stream.

`phrases` names what to listen for; an empty list asks for whatever was loaded.
`threshold_percent` is the confidence a detection must reach.

Definitions written before the engine became the variant — a `wyoming_wake` or
`device_wake` type, or a `wake` variant of `wyoming` or `device` naming an
`engine` — are still read, and are rewritten into the shape above the next time
they are saved.

A `speaker_id` definition with the `diarization_server` variant points at an
existing [Diarization_Server](https://github.com/CptCamembert/Diarization_Server),
which despite its name performs speaker recognition against enrolled embeddings
rather than diarization. It speaks its own dialect — raw 16 kHz mono 16-bit PCM
bodies and a `name` query parameter — so a pipeline capturing any other format
is refused at the stage rather than sent samples the server would misread. It
has no authentication, so the definition carries no key.

A `speaker_id` definition with the `http` variant points at a service
implementing three requests — `POST {base_url}/identify`,
`POST {base_url}/speakers/{speaker}/enroll`, and
`DELETE {base_url}/speakers/{speaker}` — documented in full on the
`conduit-speaker` crate. `engine` records which embedding model is behind it
(`speechbrain`, `resemblyzer`, or `pyannote`) and `threshold_percent` is the
similarity below which a match is reported as an unknown voice. Conduit owns
the speaker id and the service stores it as an opaque label, so a deployment
can change embedding models without every enrolled voice becoming a stranger.

A `tool` definition with the `mcp` variant registers the tools its server
currently advertises, each as `<definition id>.<tool name>`. A core's tool
binding may name one of those, or the definition id itself — which names the
whole server, and offers the model every tool that definition registered.
Discovery needs the server, but saving does not: a server that cannot be
reached within five seconds saves the definition and registers no tools, and
`POST /v1/providers/{id}/test` rediscovers them once it answers.

### `GET /v1/providers/{id}/voices`

Lists the voices one saved text-to-speech Provider Definition offers, so the
pipeline editor can present a choice rather than a text box an operator fills
in and learns about at the first reply.

Returns `404` when there is no such definition and `422` when the definition is
not a text-to-speech one. An empty `voices` list is a successful answer, not a
failure: a Wyoming synthesizer enumerates nothing and accepts any voice its
server was given, and a definition saved while its service was down is not
registered at all. The console falls back to a typed voice in both cases.

Success body:

```json
{
  "provider": "openai-speech",
  "voices": [
    { "id": "alloy", "name": "alloy", "language": "en-US" }
  ]
}
```

### `POST /v1/providers/{id}/rename`

Moves a Provider Definition to a new id and rewrites every stored pipeline that
referenced the old one, then rebuilds the active Runtime Provider Registry
Snapshot.

Renaming is its own operation because a provider id is not private to its
definition: pipeline nodes name it. A `PUT` under a new id creates a second
definition and leaves every pipeline pointing at the first, and the follow-up
`DELETE` would be refused for exactly that reason.

Request body:

```json
{ "id": "openai-main" }
```

Success body — the definition as it now reads, with the pipelines whose
references were rewritten:

```json
{
  "provider": {
    "id": "openai-main",
    "label": "OpenAI Primary",
    "kind": "llm",
    "variant": {
      "type": "llm",
      "variant": {
        "type": "openai",
        "base_url": "https://api.openai.com/v1",
        "api_key": { "type": "redacted" },
        "models": ["gpt-4.1"]
      }
    }
  },
  "renamed_pipelines": ["kitchen"]
}
```

For an MCP definition the `<definition id>.<tool name>` references a core's tool
bindings hold are rewritten too, keeping the tool name: renaming `weather-tools`
to `forecasting` turns `weather-tools.forecast` into `forecasting.forecast`.

Renaming to the id the definition already has succeeds and changes nothing.
Returns `404` when there is no such definition, `409 conflict` when the new id
is already taken — the definition in the way is never overwritten — and `422`
when the new id is not one the store can use.

### `DELETE /v1/providers/{id}`

Deletes an unreferenced Provider Definition and rebuilds the active Runtime
Provider Registry Snapshot. Deletion is refused with `409 conflict` when stored
pipelines still reference the provider id, or — for an MCP definition — any of
the `<definition id>.<tool name>` ids it registers.

Conflict body:

```json
{
  "error": "conflict",
  "detail": "provider definition is still referenced by pipelines",
  "affected_pipelines": ["kitchen"]
}
```

### `POST /v1/providers/{id}/test`

Runs a narrow active reachability check for one saved Provider Definition
through the active Runtime Provider Registry Snapshot. An OpenAI definition
lists models, a Wyoming definition opens a socket, and an MCP definition lists
tools — none of them invokes anything. The check returns the same Provider
Status shape used by `/v1/status`. A successful check marks the
provider `reachable`; a failed check leaves it `configured` with the provider
error message. The endpoint does not run a pipeline turn and does not prove the
provider inside a real conversation.

Success body:

```json
{
  "id": "openai-primary",
  "kind": "llm",
  "state": "reachable",
  "configured": true,
  "reachable": true,
  "proven_by_turn": null,
  "message": null,
  "affects_pipelines": ["kitchen"]
}
```

## Speaker Routes

The roster: who a deployment has enrolled. Conduit owns a speaker's id and the
identification service stores it as an opaque label, so a person's name lives
in Conduit and nowhere else — which is what lets a deployment change embedding
models without every enrolled voice becoming a stranger.

Naming somebody and recording them are separate requests: an operator names a
household once and records each person when that person is actually there.

A roster entry reads:

```json
{
  "id": "6f1c2d9e-3b7a-4f52-9c0f-2f4b8a1d5e77",
  "name": "Ada Lovelace",
  "samples": 2,
  "provider": "voices",
  "created_at": "2025-01-01T09:00:00Z",
  "enrolled_at": "2025-01-02T18:31:04Z"
}
```

`samples` counts the utterances the service accepted; zero means named but
never heard, and such an entry identifies nobody. `provider` names the
definition the voice prints were enrolled against, because a print does not
travel between services. Both `provider` and `enrolled_at` are absent until a
sample has been accepted.

### `GET /v1/speakers`

Lists the whole roster, in id order. An entry that cannot be decoded is left
out rather than failing the request, so one broken record does not make the
page unopenable.

### `POST /v1/speakers`

Creates somebody from `{"name": "Ada"}`. Answers `201` with the new entry,
including the id Conduit generated.

A name is free text — apostrophes, accents, and spaces are all fine, because
it is never a storage key. It is bounded at 200 characters.

Errors:

- `422` if the name is blank or too long

### `PUT /v1/speakers/{id}`

Renames somebody from `{"name": "Ada Lovelace"}`. Only the name changes: the
samples, the provider, and the id are records of what happened rather than
fields a caller sets.

Errors:

- `400` if `id` is not a speaker id
- `404` if nobody is on the roster under `id`
- `422` if the name is blank or too long

### `POST /v1/speakers/{id}/enroll`

Teaches the identification service one voice. The body is a **WAV file** —
`Content-Type: audio/wav` — and the request may carry up to 8 MiB, rather than
the 1 MiB every other route is held to.

WAV rather than raw samples because the file says its own sample rate: audio
recorded at whatever a microphone runs at arrives correctly rather than at the
wrong speed. Any PCM WAV is accepted; Conduit mixes it down to mono and
resamples it to 16 kHz before sending it on.

`?provider=<id>` names which identification service to enroll against; the
deployment's default is used when it is omitted. Worth naming when a deployment
runs more than one, because enrolling against the wrong one produces an entry
that looks enrolled and identifies nobody.

Answers the updated entry, with `samples` incremented. Enrollment is
cumulative: a second sample improves the voice print rather than replacing it.

Errors:

- `400` if `id` is not a speaker id
- `404` if nobody is on the roster under `id`
- `415` if the body does not claim to be audio
- `422` if the body is not a readable PCM WAV file
- `503` if no identification provider is configured, or the service refused the
  sample — the refusal carries what the service itself said

A refused sample leaves the roster unchanged. An entry that claimed to be
enrolled when nothing was is the worst outcome here: an operator would stop
recording, and the voice would never be recognized.

### `DELETE /v1/speakers/{id}`

Forgets somebody: the voice print first, the name second. If the service
refuses, the entry stays — a print left behind with no name would identify as
an id nobody can look up.

An entry nobody ever enrolled is removed without asking the service anything,
so a deployment with no identification provider configured can still tidy its
roster.

Errors:

- `400` if `id` is not a speaker id
- `404` if nobody is on the roster under `id`
- `503` if the service will not release the voice print

## Pipeline Routes

### `GET /v1/pipelines`

Lists stored pipeline names.

Success body:

```json
["kitchen", "office"]
```

Errors:

- `503` if the store cannot be read

### `GET /v1/pipelines/{name}`

Reads one stored pipeline and returns the graph plus runtime execution order.

Success body:

```json
{
  "graph": {
    "name": "kitchen",
    "nodes": [],
    "edges": []
  },
  "order": ["mic", "stt", "llm", "tts"]
}
```

Errors:

- `404` if no pipeline is stored under `name`
- `422` if `name` is not a usable pipeline name
- `503` if the store cannot be read

### `PUT /v1/pipelines/{name}`

Validates and stores a pipeline graph. Graph topology is always validated before
the existing stored graph is replaced. Graph nodes select providers by id; they
do not embed provider configuration.

Status:

- `201 Created` when the name is new
- `200 OK` when replacing an existing pipeline

Errors:

- `400` for malformed JSON
- `413` when the request body exceeds 1 MiB
- `415` for unsupported content type
- `422` for an invalid graph or unusable name
- `503` if the store cannot write

### `DELETE /v1/pipelines/{name}`

Deletes one stored pipeline.

Status:

- `204 No Content` when deleted

Errors:

- `404` if no pipeline is stored under `name`
- `422` if `name` is not usable
- `503` if the store cannot delete

### `POST /v1/pipelines/validate`

Validates a graph without storing it. The success body has the same shape as
`GET /v1/pipelines/{name}`.

### `POST /v1/pipelines/{name}/test-turn`

Runs one stored pipeline turn through the configured runtime providers. This is
the Operator Console test path; it does not fake success when providers are not
registered or the graph cannot be prepared.

Request body:

```json
{
  "utterance": "conduit test"
}
```

Success body:

```json
{
  "pipeline": "kitchen",
  "conversation": "00000000-0000-0000-0000-000000000000",
  "status": "completed",
  "audio_bytes": 1024,
  "reply_text": "You said: conduit test."
}
```

Errors:

- `404` if no pipeline is stored under `name`
- `422` if no runtime providers are configured or the graph cannot be prepared
- `503` if the test turn fails while running

## Event Stream

### `GET /v1/events`

Returns server-sent events published after subscription. It is a live stream,
not an event history. Runtime turn events include pipeline attribution when
they were emitted by a prepared pipeline, so status projections and operator
views do not have to infer pipeline identity from node names.

Query filters:

| Query | Meaning |
| --- | --- |
| `stages=reasoning,tools` | comma-separated stage names |
| `conversation=<uuid>` | only one conversation |
| `device=<uuid>` | only one device |
| `trace=<uuid>` | only one trace |

Unknown stages and stages with no production emitter are rejected with `422`
instead of returning an endlessly silent stream.

## Conversation WebSocket

### `GET /v1/pipelines/{name}/converse`

Upgrades to a WebSocket after authentication, device authorization, pipeline
lookup, graph resolution, and audio format validation.

Binary frames are audio. Text frames are JSON control messages.

Client to server:

```json
{"type":"end"}
```

```json
{"type":"stop"}
```

Server to client:

```json
{"type":"started","conversation":"00000000-0000-0000-0000-000000000000"}
```

```json
{"type":"done"}
```

The server may also send a `failed` notice when a turn fails after upgrade.

Audio format query parameters:

| Query | Default | Meaning |
| --- | --- | --- |
| `encoding` | `pcm_s16_le` | capture/playback encoding |
| `sample_rate` | `16000` | sample rate in Hz |
| `channels` | `1` | channel count |

Unsupported encodings, zero sample rates, and zero channel counts are refused
before upgrade.

## Ops Routes

### `GET /health`

Static liveness and version probe.

```json
{"status":"ok","version":"0.1.0"}
```

### `GET /ready`

Readiness probe. It lists pipeline names through the configured store and
returns `503` if the store cannot answer.

```json
{"status":"ready","version":"0.1.0"}
```

### `GET /metrics`

Prometheus text exposition. The content type is
`text/plain; version=0.0.4`.
