# Typed UI API Contract

The operator UI will consume Conduit's API and event vocabulary through a typed
contract rather than hand-written browser-side shapes. The frontend build should
generate or validate its API client from a formal service contract and keep
event fixtures in sync with the Rust event vocabulary so UI/API drift fails in
quality gates instead of at runtime.

The Rust source of truth for the first operator status contract is
`conduit_api::status::OperatorStatusSnapshot`. The Rust source of truth for the
live event contract is `conduit_core::event::Event::contract_examples`.
Contract tests serialize the Rust types into exact JSON fixtures so field
names, enum casing, optional fields, and event-binding vocabulary cannot drift
silently.

The frontend consumes generated TypeScript and fixtures from
`frontend/src/contracts`. The check is:

```sh
cd frontend
npm run contract:check
```

After an intentional API or event vocabulary change, regenerate artifacts from
the repository root:

```sh
CONDUIT_UPDATE_FRONTEND_CONTRACTS=1 cargo test -p conduit-api --test frontend_contract
```

Browser code must import these generated types instead of hand-authoring a
competing status or event shape.
