# Conduit Device Firmware

This directory contains the ESPHome firmware Conduit ships for satellite
devices, plus the notes needed to build and flash it.

Conduit does not speak Home Assistant Assist, Tater native satellite, ESPHome
voice-assistant, or wake-audio UDP protocols. A Conduit firmware target must
use Conduit's conversation WebSocket directly:

```text
GET /v1/pipelines/{pipeline}/converse
```

Wire contract:

- Send captured audio as binary WebSocket frames.
- Audio is 16 kHz mono signed 16-bit little-endian PCM.
- Send `{"type":"end"}` as a text frame when the utterance is complete.
- Handle `{"type":"started","conversation":"..."}` before reply audio.
- Play binary WebSocket frames from the server as reply audio.
- Send `{"type":"stop"}` to cut a reply short. Valid at any point in the turn,
  including after `end` and during playback — which is when it matters. Prefer
  it over closing the socket: the server records a `stop` as an interruption and
  a closed socket as a device that vanished, and an operator needs to tell those
  apart.
- Handle `{"type":"done"}` or `{"type":"failed","error":"..."}` as terminal
  text frames.

The canonical Rust definitions live in
`crates/conduit-core/src/device.rs`.

The shipping firmware for both supported boards is the ESPHome build described
under [ESPHome Board Targets](#esphome-board-targets). Start there.

## How Protocol Parity Is Enforced

Two copies of this wire contract exist:

- `crates/conduit-core/src/device.rs` (canonical, Rust),
- `esphome/components/conduit_voice/conduit_converse_embedded.h` (shipped
  firmware).

The header is hand-written rather than generated, because its parser is not
something `serde` can emit. Two checks stand in for generation, and CI runs
both.

**Constants.** `crates/conduit-api/tests/protocol_parity.rs` reads the shipped
header and compares its end-of-utterance frame, its stop frame, the converse
path, the sample rate, the channel count, and the sample width against what the
Rust definitions serialize and what the API's route declares. This catches a
rename or a changed value. A drifted command frame is worth catching in
particular: the server ignores a control message it cannot parse, so a device
would go on asking for something that silently never happens.

**Behaviour, which is the check that matters.** The same test serializes every
canonical notice into `tests/notices.fixture`, and
`tests/conduit_notice_fixture_test.cpp` runs those exact bytes through the real
firmware parser and asserts the decoded type and fields. Agreeing on spelling
is not the same as being able to read what the other side writes: the fixture
includes a `failed` frame whose error text contains a quote, a backslash, and
the literal `"type":"` pattern, because that field is filled from a provider's
error message and so an upstream server's wording decides what a satellite
parses.

The fixture is checked in, so the firmware suite runs without a Rust build
first. The Rust test regenerates it and fails when the result differs, which is
what stops it from being hand-edited. To update it after a deliberate protocol
change:

```sh
CONDUIT_REGENERATE_FIXTURES=1 cargo test -p conduit-api --test protocol_parity
```

The binary WWD2 wake-audio packet format in the same header is outside the
conversation protocol and has no parity check; it is covered only by
`tests/conduit_voice_embedded_test.cpp`.

Run the firmware helper tests with:

```sh
./firmware/test.sh
```

## ESPHome Board Targets

Conduit provides ESPHome firmware targets for the two supported satellite
boards:

- `esphome/conduit-sat1.yaml`, with `esphome/conduit-sat1.conduit.yaml`, uses
  FutureProofHomes Satellite1 firmware from
  `futureproofhomes/satellite1-esphome` pinned to
  `592a9687206709046f475b5464941702beacb093`.
- `esphome/conduit-voicepe.yaml`, with `esphome/conduit-voicepe.conduit.yaml`,
  uses Home Assistant Voice PE firmware from
  `esphome/home-assistant-voice-pe` pinned to
  `0579e7b9d8504264719c593474c85447253c9dc1`.

Both targets use the board hardware definitions from the named upstream
firmware sources without routing conversations through ESPHome's native
`voice_assistant`. Wake-word and action-button events call the local
`conduit_voice.start` action, which opens:

```text
ws://{server}/v1/pipelines/{pipeline}/converse
```

where `server` and `pipeline` are the ones rendered into the fragment.

Each board file is now two files. The hand-written one owns the hardware; the
Conduit half — the `conduit_voice:` and `micro_wake_word:` blocks — is rendered
by the server and merged in as a package:

```yaml
packages:
  conduit: !include conduit-sat1.conduit.yaml
```

Per [ADR-0015](../docs/adr/0015-render-the-conduit-part-of-the-firmware.md), the
board file is the board profile and is never generated; the fragment names only
ids the board file declares, and nothing about what the board is made of.

The console's **Firmware** section does this with the board ids as fields: fill
them in, read the rendered YAML, and save it under the name the board file
includes. Re-render whenever the pipeline, the server address, or the flashed
phrases change. By hand, that is:

```sh
curl -H "Authorization: Bearer $CONDUIT_MANAGEMENT_TOKEN" \
  "http://192.168.1.10:8080/v1/devices/kitchen/firmware?\
pipeline=kitchen&microphone=sat1_mics&speaker=announcement_resampling_speaker\
&mute_switch=master_mute_switch&gain_factor=6&server=192.168.1.10:8080" \
  > firmware/esphome/conduit-sat1.conduit.yaml
```

The board ids are parameters because only the board file knows them, and none
has a default: a default microphone id would render something that compiles
cleanly against somebody else's board. `scheme` defaults to `ws`;
`max_utterance_ms` to `8000`; `debug_udp_port` to `6056`. See
[the API reference](../docs/api.md) for the full parameter list.

`wss` and an `https` wake-debug endpoint verify the server against the ESP-IDF
root certificate bundle, which both targets enable through
`CONFIG_MBEDTLS_CERTIFICATE_BUNDLE`. A server whose certificate is signed by a
private CA is not in that bundle and will be refused; use `ws` behind a trusted
network, or add the CA to the build.

The two committed fragments are checked in so this suite has something to grep
and a reviewer can read the output. They are not hand-edited —
`cargo test -p conduit-api --test firmware_fragments` regenerates them and fails
when the two differ. Set `CONDUIT_REGENERATE_FIXTURES=1` to update them.

Still set as substitutions, because they are properties of the device rather
than of the pipeline:

- `name` and `friendly_name`: what the device calls itself.

Both targets read `wifi_ssid`, `wifi_password`, `api_encryption_key`,
`conduit_token`, and `wake_debug_event_url` from an ESPHome `secrets.yaml` you
create next to the YAML. That file holds credentials and is git-ignored, along
with the `.esphome/` build directory. Never commit either.

`wake_debug_event_url` is the debug ingest HTTP wake endpoint, for example
`http://192.168.1.10:8000/wake_event`; set it to `""` to disable wake-event
posting. It is a secret rather than a substitution because a Home Assistant
webhook URL carries its token in the path, which makes the whole URL a
credential. Voice PE used to take it as a substitution while Satellite1 took it
from `secrets.yaml`, which was one rule applied to one of two boards. The
renderer emits it as a `!secret` reference for both, which is what makes a
rendered fragment safe to commit.

`conduit_token` is the device token Conduit authenticates the satellite with. It
must match a `devices` entry in the server's token file, and each satellite
should have its own — a token names one device, which is how the event stream
can be filtered by satellite and how a leaked token can be revoked without
reflashing the rest of the house. Generate it rather than choosing it:

```sh
openssl rand -hex 32
```

The component sends it as an `Authorization: Bearer` header on the upgrade
request, never in the URL: the URL is logged on two device failure paths and is
recorded into the server's trace spans, so a token in it would end up in device
logs and in whatever collector those spans are exported to. The token never
appears in the device's own logs either; `dump_config` reports only whether one
is set.

The option is optional, for a server started with `CONDUIT_ALLOW_ANONYMOUS`.
Against any other server, omitting it means the upgrade is refused with 401.

The local component streams microphone audio as binary WebSocket frames, sends
`{"type":"end"}` when stopped or when `max_utterance_ms` elapses,
parses Conduit text notices, and writes binary reply frames to the board
speaker. It exposes three actions and one condition:

| Action | What it does |
| --- | --- |
| `conduit_voice.start` | Opens the socket and starts streaming a bounded microphone utterance |
| `conduit_voice.stop` | Ends the utterance and lets the reply play out |
| `conduit_voice.interrupt` | Sends `{"type":"stop"}`, silences the speaker, and ends the turn |
| `conduit_voice.is_running` (condition) | Whether a turn is in progress |

`stop` and `interrupt` differ in what happens to the reply: `stop` says "I have
finished speaking, answer me", while `interrupt` says "stop talking". Only
`interrupt` silences the local speaker, because audio already handed to it would
otherwise keep playing after the server had cancelled the turn.

Satellite1 also loads local `pcm5122` and `satellite1` component overlays from
`esphome/components/`. These are copied from the pinned FutureProofHomes ref
and patched only for ESPHome 2026.7's `GPIOPin::dump_summary(char *, size_t)`
signature so the firmware actually compiles with current ESPHome.

### Satellite1

- Microphone capture via the `satellite1` microphone platform (`sat1_mics`),
  downmixed to 16 kHz mono by the `conduit_voice` component.
- Speaker playback via the `i2s_audio` speaker behind a mixer and a resampler
  (`announcement_resampling_speaker`), through the `satellite1` DAC proxy.
- Wake trigger: `micro_wake_word` `on_wake_word_detected` calls
  `conduit_voice.start`; the component sends `end` automatically after
  `max_utterance_ms`.
- Button trigger: the `btn_action` GPIO `on_multi_click` calls
  `conduit_voice.start`, or `conduit_voice.interrupt` if a turn is already
  running — a press during a reply cuts it off. Starting is gated on
  `master_mute_switch`; interrupting is not, because muting must not trap someone
  in a reply they cannot stop.

Still needed: LED and display states for connecting, listening, thinking,
speaking, and failed. The YAML defines no `light:` block, and the vendored
`esphome/components/satellite1/light/led_ring.cpp` is not referenced by any
target, so the LED ring is dark.

### Voice PE

- Microphone capture via the `i2s_audio` microphone platform (`i2s_mics`) at
  16 kHz, downmixed to mono by the `conduit_voice` component.
- Speaker playback via the `i2s_audio` speaker behind a resampler
  (`announcement_resampling_speaker`), through the `aic3204` DAC.
- Wake trigger: `micro_wake_word` `on_wake_word_detected` calls
  `conduit_voice.start`; the component sends `end` automatically after
  `max_utterance_ms`.
- Button trigger: the `center_button` GPIO `on_multi_click` calls
  `conduit_voice.start`, or `conduit_voice.interrupt` if a turn is already
  running — a press during a reply cuts it off. Starting is gated on
  `master_mute_switch`, which also tracks the hardware mute slider; interrupting
  is not, because muting must not trap someone in a reply they cannot stop.

Still needed: LED states for connecting, listening, thinking, speaking, and
failed. The YAML defines no `light:` block, so the LED ring is dark.

When `debug_udp_host` is set, the local component also streams the same
16 kHz mono signed little-endian PCM that feeds wake-word detection to the
debug receiver using WWD2 UDP packets:

```text
magic=WWD2 assistant_id channels=1 bits=16 encoding=pcm_s16le sample_rate=16000
```

On wake-word detection, the YAML calls `conduit_voice.wake_debug_event` before
starting the Conduit conversation. That action posts to `wake_debug_event_url`
with the fragment's `debug_assistant_id` so
`esphome-wakeword-debug` can align the wake event with the continuous WWD2
audio stream.
