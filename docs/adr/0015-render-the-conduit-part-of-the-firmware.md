# Render The Conduit Part Of The Firmware, Not The Board

Conduit renders the parts of an ESPHome configuration that describe **what a
device talks to and what it listens for**, and does not render the parts that
describe **what a device is made of**. The rendered output is a fragment
included by a hand-written board file, not a whole YAML document. Rendering is a
management endpoint, and it emits `!secret` references rather than values.

The reasoning below is in the order the constraints were found, because the
ordering is what decides the scope: reading the two existing board files against
each other settles question 2, and question 2 settles most of the rest.

## What the two YAMLs actually share

`conduit-sat1.yaml` (289 lines) and `conduit-voicepe.yaml` (208 lines) look like
the classic case for a generator — most of both files is the same shape. They are
not. Sorted and compared, 140 lines are identical out of 497, and a unified diff
puts 201 lines on one side or the other. The overlap is real but it is not the
bulk, and more importantly it is not where the interesting configuration is.

What differs is not incidental duplication. `sat1` carries an `spi:` bus, a
`satellite1:` component over that bus with a `cs_pin` and an `xmos_rst_pin`, a
`fusb302b:` USB-PD negotiator whose `on_power_ready` sets a global and runs a
script, two `audio_dac:` platforms proxied through a third, a three-stage speaker
chain (`i2s_audio` → `mixer` → `resampler`), and a `script:` that activates the
TAS2780 amplifier at a gain chosen from the negotiated PD voltage. `voicepe` has
none of that and does not want it. These are not variations on a parameter; they
are different hardware.

What is genuinely shared, and genuinely a Conduit concern, is two blocks at the
end of each file: `micro_wake_word:` and `conduit_voice:`. Between them they hold
the server address, the scheme, the pipeline name, the token, the utterance
cap, the wake debug destinations, and the list of wake models. That is the whole
of what an operator configures about *Conduit* on a device, and it is roughly 60
lines of the 497.

**So the unit of rendering is those two blocks.** Not the file.

## Decision one: render a fragment, not a document

Conduit renders a `!include`-able fragment containing `conduit_voice:` and
`micro_wake_word:`. The board file stays hand-written and includes it.

The alternative — rendering the whole document — means Conduit owns `cs_pin`,
`i2s_mclk_pin`, `gain_factor`, the PD voltage thresholds, and the amplifier
activation script. Owning those means a board profile format expressive enough to
describe an SPI bus, a duplex I2S bus in secondary mode, a DAC proxy, and a
conditional lambda. That format already exists and it is called ESPHome YAML.
Reinventing a worse version of it inside Conduit is the failure mode here, and it
is a one-way door: once the server emits whole documents, every board Conduit
does not model is a board Conduit cannot flash.

A fragment inverts that. A board Conduit has never heard of works by including
the fragment, because the fragment refers to the board only through IDs the board
declares — `microphone: sat1_mics`, `speaker: announcement_resampling_speaker`.
Those two IDs, plus a `gain_factor` and a mute switch ID, are the entire contract
between the rendered part and the hand-written part.

This also answers question 1, "what is the unit of rendering", by dissolving it.
There is no board profile to store, because the board file *is* the board
profile, checked in and hand-written, exactly as it is today. Conduit needs to
know four IDs, and those arrive as parameters on the render request rather than
as a stored hardware model.

## Decision two: the input is a pipeline name plus the wake definition it resolves

The render input is a pipeline name and the four board IDs. Everything else is
read from what already exists.

The wake models are the reason this is worth doing at all, and they are already
described server-side. `WakeVariant::MicroWakeWord` exists, with
`MicroWakeWordRuntime` distinguishing `where: device` from a Wyoming server, and
`phrases()` carrying the phrase list. Its documentation says outright that on a
satellite "the server never scores these; they are what an operator flashed, for
operator screens". That comment is a description of the gap this ADR closes: the
server holds a list of phrases whose only claim to truth is that somebody
remembered to hand-write the same list into a YAML file. `factory/wake.rs`'s
`DeviceWake` registers a detector that scores nothing, so a pipeline naming the
stage resolves — and nothing anywhere checks that the device it resolves for can
actually hear the phrase.

Rendering makes `phrases()` the source. A phrase in the definition becomes a
model in the rendered `micro_wake_word:` block, or the render fails saying no
model is known for it.

**What this does not do:** it does not make Conduit a model registry. The two
files reference models three different ways today — a full S3 URL, a full GitHub
release URL, and the bare name `hey_jarvis` that ESPHome resolves from its own
manifest. A phrase-to-model mapping has to live somewhere, and it goes in the
renderer as a table with an escape hatch for an explicit URL, because the
alternative is Conduit hosting model files, which is a different project.

## Decision three: rendering is a management endpoint

`GET /v1/devices/{device}/firmware`, behind a management token.

Not a CLI: the point of the feature is that a device's configuration follows from
its Conduit configuration, and a CLI puts a terminal between the two. Not a build
step for the same reason. Not a device-token route — emphatically. A device token
must not be able to read a rendered fragment, because the fragment contains the
`micro_wake_word` configuration of a pipeline and, more to the point, because
`auth.rs` already treats the device and management audiences as a hard boundary
and logs an attempt to cross it. `ManagementCaller` rejecting a device token with
a warning is the existing posture; this route inherits it rather than arguing
with it.

The device name in the path is the name from `CONDUIT_TOKENS`, which is the one
device identifier Conduit has that survives a restart — `DeviceId` does not, as
`README.md`'s known-gaps list still records. Keying on the token-file name rather
than the runtime id is what makes a rendered fragment reproducible.

## Decision four: no rendered secret is ever a rendered value

Every secret in the rendered fragment is emitted as `!secret name`, never as the
secret itself. The endpoint reads no token from storage in order to render one.

This is not a precaution, it is the existing rule written down. Both board files
carry the comment "From secrets.yaml, not a substitution: substitutions are
committed", and `conduit_voice/__init__.py::_validate_token` rejects a token
containing CR or LF *because the token is interpolated into a raw
CRLF-terminated header block* — a validator that exists because the injection was
real. A renderer that emitted `token: <value>` would produce a file whose whole
purpose is to be committed to a firmware repository, containing a credential.
Emitting `token: !secret conduit_token` produces a file that is safe to commit
and useless to an attacker who has it.

**Found while auditing this, fixed alongside this ADR, and the same class of
mistake the renderer would industrialize:** `conduit-voicepe.yaml` passed the
wake debug webhook URL as a substitution, while `conduit-sat1.yaml` took the same
URL from `!secret` with a comment saying explicitly "the URL carries a webhook
token, and substitutions are committed". Two files, one rule, one of them
breaking it — and a Home Assistant webhook URL carries its token in the path, so
the URL *is* the credential.

The test could not see it. `firmware/tests/esphome_firmware_test.sh` grepped both
files for the *key* `debug_wake_event_url` and for the literal `token: !secret
conduit_token`, so it caught a missing token secret and the substitution
satisfied it. Both boards now take the URL from secrets, and the suite asserts by
shape instead: every credential-bearing field must be spelled `!secret`, and no
credential-shaped key may be defined in the substitutions block at all.

The renderer's value here is partly that this stops being possible. One code path
emitting `!secret` for every credential-bearing field cannot disagree with itself
across two boards.

Every other rendered field is an interpolation into a config format and is
validated as such. The component's own schema is the model —
`_validate_pipeline` bounds length and restricts to `[A-Za-z0-9-_]`,
`_validate_token` rejects CR/LF and surrounding whitespace — and the renderer
validates before emitting rather than trusting ESPHome to catch it, because a
value that reaches a device is one that got past both.

## Decision five: the renderer and the component version together, and that is the point

The rendered fragment targets `conduit_voice`'s config schema. They ship in the
same repository, in the same release, and neither is independently upgradable.

Stated plainly because it is a real cost: an operator cannot pin an old
`conduit_voice` and take a new server, or vice versa, once they render. In
exchange, a protocol change becomes a deletion rather than a negotiation — which
is the mechanism by which this unblocks #105. The barge-in constraint there is
`conduit_voice.cpp:288`, where `handle_microphone_data_` returns early unless
`state_ == STREAMING`, while `:279` sets `REPLYING` on the first reply chunk: a
device stops sending audio the moment the assistant starts speaking. Changing
that today means a firmware change coordinated by hand with a server change
across every flashed device. Under rendering, server and fragment move together
and the coordination is the release.

`crates/conduit-api/tests/protocol_parity.rs` already binds the firmware's
protocol constants to the Rust definitions. That test is the precedent for this
decision, not an argument against it: the two halves are already coupled and
already tested as coupled. This makes the coupling honest.

## What happens to the two existing YAMLs

They stay, hand-written, and become **both examples and board profiles** — the
two are the same thing under decision one. Each loses its `conduit_voice:` and
`micro_wake_word:` blocks to an `!include` of the rendered fragment, and keeps
everything else verbatim.

They are not deleted and not generated. A working `sat1` file is the only
complete statement of how that board's XMOS chip, PD negotiation and DAC proxy
fit together, and the firmware suite greps it for exactly those facts.

## Consequences

- A pipeline's wake phrases and a device's flashed models can no longer disagree
  silently. Today nothing connects them.
- Conduit gains a phrase-to-model table it must maintain, with an explicit-URL
  escape hatch. This is the ongoing cost of the decision.
- Rendered YAML still has to reach a device, and Conduit has no relationship with
  an ESPHome instance. This ADR deliberately does not solve that: an endpoint
  that emits a correct fragment is useful to an operator with an ESPHome dashboard
  today, and OTA delivery is a separate decision with its own trust questions.
  Question 4 is answered "not yet, and on purpose".
  **Since superseded by [ADR-0019](0019-flashing-through-an-esphome-instance-conduit-does-not-own.md)**,
  which answers question 4 by delegating compilation and flashing to an ESPHome
  instance the operator already runs. It does not disturb the fragment decision
  below — it depends on it, because a fragment is what keeps the upload to one
  small file.
- The `micro_wake_word` `gain_factor` differs per board (6 on sat1, 4 on voicepe)
  and is a microphone property, not a pipeline property, so it is a render
  parameter rather than something read from a definition.
- `firmware/tests/esphome_firmware_test.sh` greps the board files for keys that
  will move into the fragment. Those assertions move with them when it does. The
  shape-based secret assertions added with this ADR should not: they must apply to
  whatever file the credential ends up in, rendered or hand-written.

## Alternatives rejected

**Render the whole document.** Requires Conduit to model SPI buses, I2S modes,
DAC proxies and conditional lambdas. Reinvents ESPHome YAML, and makes every
unmodelled board unflashable. Rejected under decision one.

**Store board profiles like provider definitions.** Follows from rendering whole
documents and dies with it. A board profile stored in Conduit is a second,
worse copy of a file that is already checked in and already tested.

**A CLI or build-step renderer.** Cheaper, and forfeits the reason to build it:
configuration that still requires someone at a terminal has not moved.

**Render only `conduit_voice:` and leave `micro_wake_word:` hand-written.** The
smallest possible version, and it drops the motivating case. On-device wake word
being configured rather than hand-edited *is* the `micro_wake_word:` block.
