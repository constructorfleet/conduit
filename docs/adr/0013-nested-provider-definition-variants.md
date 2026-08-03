# Nested Provider Definition Variants

Provider definition variants are grouped by capability rather than listed flat.
The provider definition variant is a two-level discriminated union: an outer
`type` names the capability (`llm`, `stt`, `tts`, `tool`, `wake`,
`speaker_id`) and an inner `variant.type` names the vendor (`openai`,
`wyoming`, `mcp`, `device`, `http`, `diarization_server`).

This refines [0011-typed-provider-definitions](0011-typed-provider-definitions.md),
where every variant was a flat atom (`openai_llm`, `wyoming_wake`, ...). The
closed-set model is unchanged; the shape is reorganized so each capability owns
its vendors.

**Motivation**

- A flat enum paid a full pairwise name for every capability–vendor
  combination, so the type grew quadratically with both dimensions.
- Capability-wide behavior — which kind a variant registers under, which
  pipeline stage consumes it — had to match every flat tag by name instead of
  the capability it already encodes.
- Adding a vendor to an existing capability meant touching every consumer of
  the flat tag list rather than one inner enum.

**Consequences**

- The wire format for `variant` changes to two levels:
  `{ "type": "llm", "variant": { "type": "openai", ... } }`. This is a
  breaking change: stored records written in the flat shape must be migrated.
- Legacy flat tags are still read on deserialize — `openai_llm`, `wyoming_stt`,
  and the rest map onto the new nested shape — so older saved definitions load
  unchanged. Serialization always emits the new two-level shape, so re-saving a
  record upgrades it in place.
- The outer capability is structural: `kind` in a provider definition view is
  the outer variant, and `definition_variant` in the component catalog names
  the inner vendor.
- Each capability owns its inner `*Variant` enum in the `storage` module, one
  file per capability after the module split, keeping per-capability
  `redacted()` and `with_secret_updates_from()` behavior co-located.
- Adding a new vendor to a capability extends one inner enum and its
  deserializer; adding a new capability adds one outer variant and a new inner
  enum.
