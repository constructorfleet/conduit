# conduit-api

HTTP, event-stream, and WebSocket API for Conduit.

This crate wires configuration, authentication, storage, providers, metrics,
and runtime execution into the server binary.

## Routers

`router(state)` builds the authenticated service router:

- `GET /v1/events`
- `GET /v1/turns`
- `GET /v1/turns/{turn_id}`
- `GET /v1/turns/{turn_id}/events`
- `GET /v1/turns/live`
- `GET /v1/pipelines`
- `POST /v1/pipelines/validate`
- `GET /v1/pipelines/{name}`
- `PUT /v1/pipelines/{name}`
- `DELETE /v1/pipelines/{name}`
- `POST /v1/pipelines/{name}/test-turn`
- `GET /v1/pipelines/{name}/converse`

`ops_router(state)` builds the unauthenticated ops router:

- `GET /health`
- `GET /ready`
- `GET /metrics`

The service router has a 1 MiB body limit, a 30 second request timeout, request
tracing without headers, and JSON-shaped API errors.

## Authentication

Handlers enforce auth by asking for typed extractors:

- `ManagementCaller` for pipeline management and event streaming
- `DeviceCaller` for conversation sockets

This makes accidentally adding an unauthenticated service route harder: a route
without the right extractor does not receive an identity.

Token files are loaded once at startup unless anonymous mode is explicitly
enabled.

## Configuration

`config.rs` reads provider, store, auth, logging, and timeout configuration from
environment variables. See [../../docs/configuration.md](../../docs/configuration.md).

## Conversation Route

The WebSocket route resolves the pipeline before upgrading. Missing pipelines,
unrunnable graphs, unauthorized callers, forbidden devices, and unsupported
audio formats therefore fail as HTTP responses instead of sockets that open and
then die.

After upgrade, binary frames are audio and text frames are JSON control
messages. Runtime events are published to the bus and can be observed through
`/v1/events`.

## Turn Reconstruction

`/v1/events` remains the raw evidence stream. Operator-facing turn inspection
uses the server-owned reconstruction read model:

- `GET /v1/turns` lists recent reconstructed turns.
- `GET /v1/turns/{turn_id}` returns one ordered turn snapshot.
- `GET /v1/turns/{turn_id}/events` returns retained raw evidence for that turn.
- `GET /v1/turns/live` streams typed reconstruction updates.

The first history store is in-memory. Retention is bounded by
`CONDUIT_TURN_HISTORY_MAX_TURNS=500` and
`CONDUIT_TURN_HISTORY_RETENTION_SECS=86400` by default. Set either value to `0`
to remove that individual bound; setting both to `0` is allowed but logged as
unbounded history.
