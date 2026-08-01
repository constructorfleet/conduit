# Typed UI API Contract

The operator UI will consume Conduit's API and event vocabulary through a typed contract rather than hand-written browser-side shapes. The frontend build should generate or validate its API client from a formal service contract and keep event fixtures in sync with the Rust event vocabulary so UI/API drift fails in quality gates instead of at runtime.
