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

Generated TypeScript contracts and reviewable JSON fixtures live under
`src/contracts`. Rust owns those artifacts. After an intentional API or event
contract change, update them from the repository root with:

```sh
CONDUIT_UPDATE_FRONTEND_CONTRACTS=1 cargo test -p conduit-api --test frontend_contract
```
