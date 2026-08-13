# Phrase Is Engine-Agnostic

A **phrase** in Excita — the wake word an operator arms, like "hey
Jarvis" — is a single row shared across engines. Model rows point at a
phrase and carry the engine kind; phrase rows do not. If the same
operator trains "hey Jarvis" under openWakeWord for a satellite and
imports a "hey Jarvis" microWakeWord model for an ESP32, they see one
phrase with two model bindings, not two phrases.

**Why not engine-scoped phrases**

The alternative — `phrase_id` unique per `(engine, name)` — matches the
storage layer's mechanics but not the operator's mental model. "hey
Jarvis on the ESP32" and "hey Jarvis on the satellite" are the same
wake word to the person who armed them, and the questions Excita exists
to answer depend on that being true:

- "Show me all models trained for this phrase." Comparing OWW v3 against
  µWW v1 for the same wake word is what tells the operator whether the
  µWW model is ready to replace the OWW one on the satellite that has
  both engines available.
- "Retire this phrase everywhere." An operator who stops using "hey
  Jarvis" wants one control, not one control per engine.
- "Which devices arm this phrase?" A cross-engine deployment matrix,
  which is exactly what a wake-word ops tool is for.

Engine-scoped phrases force the operator to maintain a synonym table in
their head and force the UI to render cross-engine questions as joins on
a text field.

**How engine-native identifiers map**

Engines carry their own tag for a phrase inside the trained artifact —
openWakeWord uses the ONNX output key (`hey_jarvis`), microWakeWord
bakes it into the TFLite-Micro manifest, nanoWakeWord carries it in the
gate/verifier config. Excita's `phrase_id` is the source of truth; the
engine-native tag is stored on the *model* row (as `engine_phrase_key`)
so the adapter knows which key to read from the model's own output. Two
models bound to the same phrase can have different `engine_phrase_key`
values — that's fine, they're different files.

**Uniqueness constraints**

- `phrase(name)` is unique globally within an Excita instance —
  duplicating "hey Jarvis" as a second phrase is a mistake, not a
  choice.
- `model(phrase_id, engine, version)` is unique — the same engine can't
  have two v3s of the same phrase.
- Multiple *active* models per `(phrase, engine)` are allowed; a
  separate deploy binding names which one is current on a given target,
  so "test v4 on one device while v3 stays deployed to the fleet" is a
  single-row change, not a phrase-rename dance.

**Consequences**

- `phrase` schema: `id, name (unique), display_name, notes,
  created_at, deleted_at`. Engine field removed from spec 0011's
  sketch.
- `model` schema gains `engine_phrase_key` alongside the existing
  `engine` and `phrase_id`.
- Sidecar manifests for filesystem imports carry `phrase_id` (or a
  `phrase_name` the scanner resolves to an id, creating a phrase row
  if the name is new).
- The UI's "phrase detail" view lists all models across engines with
  their per-engine metrics side-by-side. Cross-engine ranking is where
  Q7's thin normalized `metrics_json` envelope earns its keep.
