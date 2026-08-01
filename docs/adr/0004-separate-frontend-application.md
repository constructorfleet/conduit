# Separate Frontend Application

Conduit's operator UI will be built as a separate frontend application in the repository rather than server-rendered from `conduit-api`. The Rust service remains responsible for APIs and runtime state, while the frontend can use appropriate browser tooling for responsive layout, event-driven state, graph editing, and visual interaction; the built assets may be served by the service later if packaging calls for it.
