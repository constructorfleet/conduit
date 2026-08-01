# Typed UI API Contract

The operator UI will consume Conduit's API and event vocabulary through a typed
contract rather than hand-written browser-side shapes. The frontend build should
generate or validate its API client from a formal service contract and keep
event fixtures in sync with the Rust event vocabulary so UI/API drift fails in
quality gates instead of at runtime.

The Rust source of truth for the first operator status contract is
`conduit_api::status::OperatorStatusSnapshot`. Contract tests serialize the Rust
types into exact JSON fixtures so field names, enum casing, optional fields, and
event-binding vocabulary cannot drift silently.

The dedicated frontend contract-gate slice will either generate TypeScript
types from these Rust contracts or validate frontend-owned fixtures against
schemas generated from them. Until that slice exists, browser code must not
hand-author a competing status shape.
