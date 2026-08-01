# API Reference

Conduit exposes a service API and an ops API. The service API is authenticated;
the ops API is intentionally unauthenticated and must be protected by where it
is bound and what the network publishes.

## Listeners

| Listener | Default | Routes | Authentication |
| --- | --- | --- | --- |
| Service | `0.0.0.0:8080` | `/v1/events`, `/v1/pipelines`, `/v1/pipelines/{name}`, `/v1/pipelines/validate`, `/v1/pipelines/{name}/converse` | Bearer token unless anonymous mode is explicitly enabled |
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
pipelines and read events. Management tokens may also open a conversation for
manual testing. Device tokens cannot call management routes.

| Situation | Status |
| --- | --- |
| Missing, malformed, or unknown bearer token | `401 Unauthorized` with `WWW-Authenticate: Bearer` |
| Device token on a management route | `403 Forbidden` |
| Device restricted away from the requested pipeline | `403 Forbidden` |

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
not an event history.

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
