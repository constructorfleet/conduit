# Firmware Fragment Rendering

Implementation spec for
[ADR-0015](../adr/0015-render-the-conduit-part-of-the-firmware.md).

Adds `GET /v1/devices/{device}/firmware`, which renders the `conduit_voice:` and
`micro_wake_word:` blocks for one device as an `!include`-able ESPHome fragment,
and converts the two checked-in board files to include what they currently
inline.

Nothing about board hardware is rendered, and no rendered field ever carries a
secret value. Both of those are ADR-0015 decisions, not choices left to the
implementation.

---

## Goals

1. A pipeline's wake phrases and a device's flashed wake models can no longer
   disagree silently. Today `WakeVariant::MicroWakeWord`'s `phrases()` and the
   `models:` list in a board YAML are two hand-maintained copies of one fact.
2. An operator reconfigures a device's Conduit settings without editing YAML —
   they change a pipeline or a definition and re-render.
3. Every credential-bearing field in rendered output is spelled `!secret name`,
   emitted by one code path that cannot disagree with itself across boards.
4. The two board files keep working, hand-written, as both examples and board
   profiles.

## Non-Goals

- Rendering board hardware: `spi:`, `i2s_audio:`, `audio_dac:`, GPIO pins,
  the PD-negotiation script, `gain_factor` as anything but a parameter.
  Rejected in ADR-0015 decision one.
- Storing board profiles server-side. The board file is the board profile.
- Delivering a rendered fragment to a device. ADR-0015 answers question 4 with
  "not yet, and on purpose": Conduit has no relationship with an ESPHome
  instance, and OTA delivery carries its own trust questions. The operator
  saves the response and runs their own ESPHome build.
- Hosting wake models. The phrase-to-model table is a mapping, not a registry.
- Compiling or validating the fragment against a real ESPHome installation.
  The renderer validates its own inputs; ESPHome remains the authority on YAML
  it accepts.

---

## How This Is Broken Up

Four tracks. Each builds, passes the gates, and is worth committing alone.

| # | Track | Delivers |
| --- | --- | --- |
| A | Phrase-to-model resolution in `conduit-provider` | The mapping, with an explicit-URL escape hatch, testable without an endpoint |
| B | The renderer and its endpoint | `GET /v1/devices/{device}/firmware` |
| C | Board files include the fragment | The duplication is gone for real |
| D | Console affordance | An operator can fetch a fragment without curl |

Track A first because it is the only part with a decision left in it and it is
pure library code. Track C is deliberately last: until B renders output that
matches what the boards inline today, converting them is a regression waiting
for a flash.

---

## Track A — Phrase-to-model resolution

**Where:** `crates/conduit-provider/src/storage/wake.rs`, beside
`MicroWakeWordRuntime`, which is the type that already distinguishes
`where: device` from a Wyoming server.

A phrase in a device-runtime `MicroWakeWord` definition has to become a
`models:` entry. The three spellings in use across the two boards are the whole
requirement:

- a bare name ESPHome resolves from its own manifest — `hey_jarvis`
- a GitHub release URL — `okay_nabu_20241226.3/okay_nabu.json`
- an S3 URL — the `fph-firmware-assets` bucket

So the model reference is a two-variant type: a manifest name, or an explicit
URL. A phrase resolves through a table of known phrases to manifest names, and a
definition may override any phrase with an explicit URL. **A phrase with neither
a table entry nor an override is an error at render time, not a silently
omitted model** — a device flashed without the model for a phrase the server
believes it detects is exactly the failure this feature exists to prevent.

The table lives in the renderer's crate as data, per ADR-0015: "Conduit gains a
phrase-to-model table it must maintain, with an explicit-URL escape hatch. This
is the ongoing cost of the decision."

**Open decision for review, flagged rather than assumed:** the override has to
be stored somewhere, and the natural home is a field on the
`MicroWakeWord` variant — `models: Option<HashMap<String, String>>` keyed by
phrase. That is a wire-format change to a stored definition. It is additive and
`#[serde(default)]`, so existing stored definitions parse unchanged, but it does
touch `WireWakeVariant` and the frontend contract. The alternative — the
override arrives as a query parameter on the render request — keeps storage
untouched at the cost of making a device's models not reproducible from server
state, which defeats the point. **Recommendation: the stored field.** Confirm
before implementing, since ADR-0011/0013 govern that shape.

Tests: a phrase in the table resolves to its manifest name; an overridden phrase
resolves to its URL and ignores the table; an unknown phrase with no override is
an error naming the phrase; a Wyoming-runtime definition resolves no models at
all, because its phrases are scored on a server and belong in no firmware.

## Track B — The renderer and its endpoint

**Where:** a new `crates/conduit-api/src/firmware.rs`, with the route added in
`lib.rs` beside the pipeline routes.

`GET /v1/devices/{device}/firmware`, extracting `ManagementCaller`. Per
ADR-0015: not a device-token route, emphatically — `auth.rs` already treats the
audiences as a hard boundary and logs an attempt to cross it, and this route
inherits that posture rather than arguing with it. Extracting `ManagementCaller`
is what enforces it; no per-handler check is needed or wanted.

**The `device` in the path is the name from the `CONDUIT_TOKENS` file**, not a
`DeviceId`, because `DeviceId::new()` is minted per process and does not survive
a restart. `Tokens` indexes by token today and has no name lookup, so this track
adds one — a way to ask `Access` whether a device of a given name is declared,
and what pipelines it may open. Note the two cases that need deciding in code
rather than in prose:

- **An anonymous server** (`CONDUIT_ALLOW_ANONYMOUS`) has one shared device
  identity named `anonymous` and no declared names. Rendering for an arbitrary
  name would be rendering for a device that does not exist.
  **Recommendation: render for `anonymous` and 404 every other name**, which
  keeps a development server usable without inventing devices.
- **A device token scoped to pipelines** (`Device::pipelines`) must not render a
  fragment naming a pipeline it may not open. The render request carries a
  pipeline name; if the device is scoped and the pipeline is not in its list,
  that is a 422, not a silently rendered fragment the device will be refused
  when it connects.

**Request parameters.** The pipeline name and the four board IDs ADR-0015
identifies as the whole contract between rendered and hand-written parts, plus
the two the boards differ on:

| Parameter | Source | Why not read from a definition |
| --- | --- | --- |
| `pipeline` | required | Which pipeline this device converses with |
| `microphone` | required | A board ID, declared by the board file |
| `speaker` | required | A board ID |
| `mute_switch` | required | A board ID, used by `on_wake_word_detected` |
| `gain_factor` | required | A microphone property, 6 on sat1 and 4 on voicepe — ADR-0015 consequences |
| `server`, `scheme`, `max_utterance_ms` | required / defaulted | What the device dials; not a property of the pipeline graph |

Every one of these is interpolated into a config format, so **every one is
validated before emission**, mirroring the component's own schema, which is the
authority: `_validate_pipeline` bounds length at 128 and restricts to
`[A-Za-z0-9-_]`; IDs get the same treatment because an ESPHome ID has the same
shape; `scheme` is `ws` or `wss` and nothing else; `gain_factor` and
`max_utterance_ms` are bounded integers. A rejected parameter is a 422 naming
the field. ADR-0015: "the renderer validates before emitting rather than
trusting ESPHome to catch it, because a value that reaches a device is one that
got past both."

**Secrets.** `token:` and `debug_wake_event_url:` are emitted as
`!secret conduit_token` and `!secret wake_debug_event_url` — the names the board
files already use. The handler reads no token from storage in order to render
one; there is no code path from a stored credential to rendered output, which
is the property worth testing directly.

**Resolution.** The pipeline's `NodeKind::WakeWord` node names a provider
definition; that definition's `WakeVariant::MicroWakeWord` with
`MicroWakeWordRuntime::Device` supplies the phrases, which track A turns into
models. A pipeline whose wake stage is a Wyoming runtime renders a
`conduit_voice:` block and **no** `micro_wake_word:` block, because the device
detects nothing — and that is a correct fragment, not an error. A pipeline with
no wake stage at all is the same case.

**Response.** `text/yaml`, the fragment as a body. Not JSON-wrapped: the
artifact is a file an operator saves next to their board file, and a YAML
document inside a JSON string is a worse version of that.

Errors: 404 for an unknown device name or unknown pipeline; 422 for an invalid
parameter, a pipeline the device may not open, or a phrase with no known model;
503 when the store is unavailable, via the existing `store_failure`.

Tests: authentication, including that a device token is refused with the logged
warning; the happy path for each board's parameters, asserted against the blocks
those boards inline today; every validation rejection; the Wyoming and no-wake
cases emitting no `micro_wake_word:`; a scoped device refused a pipeline outside
its list; **and a test asserting no rendered output contains a token value**,
which is the security property rather than a spot-check of one field.

## Track C — Board files include the fragment

Each board file loses its `conduit_voice:` and `micro_wake_word:` blocks to an
`!include` and keeps everything else verbatim. ADR-0015: they are not deleted
and not generated; a working `sat1` file is the only complete statement of how
that board's XMOS chip, PD negotiation and DAC proxy fit together.

`firmware/tests/esphome_firmware_test.sh` greps the board files for keys that
move. Those assertions move with them, per the ADR's consequences — **with one
exception it calls out explicitly**: the shape-based secret assertions must
apply to whatever file the credential ends up in, rendered or hand-written. In
practice that means the loop over credential-bearing fields runs against the
committed fragment too, and the substitutions-block scan stays on the board
files, where a substitutions block still exists.

A rendered fragment is committed for each board so the firmware suite has
something to grep and a reviewer can see the output. That fragment is a fixture:
a test regenerates it from the renderer and fails if it drifts, which is the
same mechanism `protocol_parity.rs` already uses for `notices.fixture`.

## Track D — Console affordance

A device's page offers its fragment for download, with the board IDs as fields.
The contract addition is typed in `frontend/src/contracts/`, per ADR-0006. Last
because the endpoint is useful to an operator with an ESPHome dashboard before a
console button exists.

---

## Resolved Decisions

**The fragment is one file, not two.** Both blocks in one include, rather than a
`conduit_voice` fragment and a `micro_wake_word` fragment. They share the
`conduit` ID — `on_wake_word_detected` calls `conduit_voice.start` on it — so
splitting them produces two files that are only valid together.

**No `vad:` key is rendered conditionally.** Both boards pass a bare `vad:`
under `micro_wake_word:`, and it is board-independent, so it is emitted
unconditionally. Input-path VAD as a pipeline stage (ADR-0014) is a server
concern and does not appear in firmware.

**`debug_*` fields are rendered.** They are in both boards' `conduit_voice:`
blocks, they are Conduit configuration rather than board configuration, and
`debug_wake_event_url` is one of the two credentials whose `!secret` spelling
this feature is meant to make un-disagreeable.

## Risks

- **The fragment and the component version together, with no compatibility
  seam.** ADR-0015 decision five accepts this deliberately: an operator cannot
  pin an old `conduit_voice` and take a new server. Worth a line in the firmware
  README so it is discovered before a flash rather than after.
- **A rendered fragment nobody applies.** The endpoint's value depends on an
  operator with an ESPHome dashboard. Question 4 is answered "not yet" on
  purpose, but if nobody applies the output, tracks A–C bought only the
  phrase/model consistency check — which is still the motivating case, and is
  why track A is severable.
