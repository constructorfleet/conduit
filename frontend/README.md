# Conduit Operator Console

React, TypeScript, and Vite frontend for Conduit's Operator Console.

## Commands

```sh
npm ci
npm run dev
npm run lint
npm run test
npm run contract:check
npm run build
npm run format
```

The access foundation uses management bearer tokens or explicit anonymous mode.
Bearer tokens stay in `sessionStorage` unless the operator chooses
`Remember on this browser`, which writes to `localStorage`.

The app shell is organized around Overview, Pipelines, Providers, Events, and
Settings. Runtime integration starts from `/v1/status` and then applies
`/v1/events` updates after the snapshot is loaded.
After operator access is selected, the console loads `/v1/status`, lists
`/v1/pipelines`, and fetches each pipeline view through the generated API
client. Frontend tests mock those HTTP responses with non-fixture data so the
rendered UI proves it is using the data client path.
Set `VITE_CONDUIT_DATA_SOURCE=mock` to run the console against generated
contract fixtures instead of live HTTP while developing the UI without a
backend.
In live mode, Vite proxies `/v1/*` requests to
`VITE_CONDUIT_API_TARGET` or `http://127.0.0.1:8080` by default, so
`npm run dev` can talk to a local `conduit-api` service without hardcoding an
API origin into the browser bundle.

The Overview section is the Operations Workspace landing surface when a usable
pipeline exists. It renders current exceptions before baseline status, keeps
connected satellites separate from recently active satellites, preserves the
last known snapshot as Stale State when the event stream disconnects, and
expects a refreshed status snapshot before clearing stale state after reconnect.

When `/v1/status` reports `runtime.launch_state` as `first_run_setup`, the app
routes into Guided Setup. Guided Setup builds a minimal source-to-sink voice
loop graph, invokes reusable Provider Settings inline, lets optional tool setup
be skipped, validates required fields, and transitions back to Overview after
the pipeline graph is saved.

The Providers section lists providers as one table per stage, all sharing one
set of column widths so the groups read as one list under headings. Clicking a
row expands it into the editor for that provider, in place and under the row it
belongs to, so the state and the pipelines using it stay readable while it is
changed. Only "Add provider" opens a dialog, because a provider being created
has no row to expand yet.

The Events section defaults to Turn Reconstruction, which renders the ordered
component story for a conversation turn from generated event envelopes. Raw
event inspection is secondary and filterable, while stale or disconnected event
streams keep the last reconstructed turn visible with Stale State marked.

The Pipelines section starts with a first-party React/SVG graph editor
foundation rather than a drag-and-drop graph package. That keeps the initial
surface tied to Conduit's real `PipelineGraph` JSON shape and backend validation
API seams while leaving room to adopt a heavier graph library when advanced
desktop editing needs it. Small screens render graphs read-only.

Generated TypeScript contracts and reviewable JSON fixtures live under
`src/contracts`. Rust owns those artifacts. After an intentional API or event
contract change, update them from the repository root with:

```sh
CONDUIT_UPDATE_FRONTEND_CONTRACTS=1 cargo test -p conduit-api --test frontend_contract
```
