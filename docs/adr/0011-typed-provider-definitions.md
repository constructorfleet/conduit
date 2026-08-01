# Typed Provider Definitions

Provider definitions are server-owned structured records, not generic configuration blobs embedded in pipeline graphs or browser-local state. Conduit will model saved provider definitions as a closed set of component-specific variants for now, trading cheap component extensibility for compile-time API, storage, and runtime-registration safety.

**Consequences**

- Pipeline graphs reference provider definitions by stable provider id only.
- Adding a new provider component requires adding a new provider definition variant and updating generated frontend contracts.
- Component schemas may still drive UI forms, but saved provider definitions are typed records rather than arbitrary config maps.
- The provider component catalog is exposed at `/v1/catalog/providers`; the old pipeline-component route is removed rather than kept as an alias.
- Catalog entries include component identity, provider kind, definition variant identity, and form metadata, while saved provider definitions use typed variants.
- Product runtime providers are built from the provider definition store, not environment variables. Direct provider injection remains only a test/development seam.
- Existing browser-local provider definitions and old graph provider references are not migrated; operators may recreate provider definitions and pipelines through the backend-backed flow.
