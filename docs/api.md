# API Reference

Conduit exposes a service API and an ops API. The service API is authenticated;
the ops API is intentionally unauthenticated and must be protected by where it
is bound and what the network publishes.

## Listeners

| Listener | Default | Routes | Authentication |
| --- | --- | --- | --- |
| Service | `0.0.0.0:8080` | `/v1/status`, `/v1/events`, `/v1/pipeline-components`, `/v1/pipelines`, `/v1/pipelines/{name}`, `/v1/pipelines/validate`, `/v1/pipelines/{name}/converse` | Bearer token unless anonymous mode is explicitly enabled |
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

Provider Status is currently projected from runtime provider registrations and
stored pipeline graph references. A registered provider is Configured. Its
`Provider::health()` result is the active reachability check: usable health
means Reachable, while an unhealthy result remains Configured with the reason
in `message`. Proven Provider status comes only from a completed successful
turn that invoked the provider's component in a real pipeline. Missing core
runtime capabilities are reported as Unavailable provider slots, and graph
references to unregistered providers are reported as Unavailable references.

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

## Pipeline Routes

### `GET /v1/pipeline-components`

Lists known pipeline component descriptors and the configuration schema each
component accepts. The Operator Console uses this catalog to render
provider-specific node configuration forms backed by `graph.nodes[].config`.

Success body:

```json
{
  "components": [
    {
      "id": "openai.responses",
      "label": "OpenAI Responses",
      "kind": "llm",
      "schema": {
        "properties": {
          "base_url": { "type": "string", "format": "url" },
          "api_key": { "type": "string" },
          "model": { "type": "string", "pattern": "[a-z0-9.]+" },
          "streaming": { "type": "boolean" }
        },
        "required": ["base_url", "model"]
      }
    }
  ]
}
```

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

Validates and stores a pipeline graph.

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
