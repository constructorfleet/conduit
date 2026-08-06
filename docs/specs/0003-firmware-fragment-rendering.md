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
- Hosting wake models. The phrase-to-model table is a mapping, not a registry.
- **Compiling firmware, or serving a compiled image.** ADR-0019 delegates both
  to an ESPHome instance the operator already runs, because a compiled `.bin`
  has the device token substituted into it and serving one would invert
  ADR-0015's secrets posture. Conduit's artifact stays text.
- Validating the fragment against a real ESPHome installation. The renderer
  validates its own inputs; ESPHome remains the authority on YAML it accepts.
- First adoption of a device with no firmware. ESPHome's own install flow owns
  that; track E links to it.

---

## How This Is Broken Up

Four tracks. Each builds, passes the gates, and is worth committing alone.

| # | Track | Delivers |
| --- | --- | --- |
| A | Phrase-to-model resolution in `conduit-provider` | The mapping, with an explicit-URL escape hatch, testable without an endpoint |
| B | The renderer and its endpoint | `GET /v1/devices/{device}/firmware` |
| C | Board files include the fragment | The duplication is gone for real |
| D | Console affordance | An operator can fetch a fragment without curl |
| E | Hand-off to an ESPHome instance | The fragment reaches a device, per ADR-0019 |

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
The contract addition is typed in `frontend/src/contracts/`, per ADR-0006. After
B because the endpoint is useful to an operator with an ESPHome dashboard before
a console button exists.

**This download stays after track E ships.** ADR-0019 makes it the fallback: when
the configured ESPHome instance is unreachable or its API has moved, the page
degrades to "here is your fragment, apply it yourself" rather than to a dead
button.

## Track E — Hand-off to an ESPHome instance

Implements [ADR-0019](../adr/0019-flashing-through-an-esphome-instance-conduit-does-not-own.md).
Conduit uploads the fragment to an ESPHome dashboard the operator already runs
and links to that dashboard's install and OTA affordances. Conduit does not
compile, does not store an image, and does not serve a binary.

**Configuration.** One new setting: the ESPHome dashboard base URL, and whatever
credential that instance requires. Read from the environment like every other
config, per the existing `config.rs` pattern.

**The base URL is an SSRF surface** — an operator-supplied address the server
dials — and gets the treatment that needs: scheme restricted to `http`/`https`,
parsed rather than string-concatenated, and a failure to connect reported as a
failure rather than retried in a way that scans. The credential for that
instance is a secret Conduit holds, so it is never logged and never returned in
a response, the same rule `auth.rs` already follows for tokens.

**What is uploaded is only the fragment.** The board file is uploaded once by
hand or already lives in the ESPHome config directory; reconfiguring a device
rewrites one small file. This is why ADR-0015's fragment decision is load-bearing
for flashing rather than merely compatible with it.

**What the console shows.** Upload, then a link out to the ESPHome instance for
the build and install. Not an embedded `<esp-web-install-button>`: ESP Web Tools
flashes a compiled `.bin` over WebSerial, which is the artifact ADR-0019 declines
to produce. The device's own dashboard page already has that button, correctly,
because that instance did the compile.

Tests: an unreachable instance surfaces an actionable error and the download
fallback still works; a rejected scheme is refused before any request is made;
the configured credential appears in no response body and no log line; the
upload sends the fragment and not the board file.

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
- **A rendered fragment nobody applies.** Tracks A–D depend on an operator with
  an ESPHome dashboard applying the output by hand. Track E closes that, but
  until it lands those tracks buy only the phrase/model consistency check —
  which is still the motivating case, and is why track A is severable.
- **ESPHome's dashboard API is not a versioned third-party contract** and can
  change between releases. Weaker coupling than ADR-0015 decision five, but real;
  the track D fallback is what keeps a broken upload from being a dead page.
