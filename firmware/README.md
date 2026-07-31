# Conduit Device Firmware

This directory contains Conduit-owned firmware integration notes and protocol
helpers for satellite devices.

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
- Handle `{"type":"done"}` or `{"type":"failed","error":"..."}` as terminal
  text frames.

The canonical Rust definitions live in
`crates/conduit-core/src/device.rs`.

The shipping firmware for both supported boards is the ESPHome build described
under [ESPHome Board Targets](#esphome-board-targets). Start there.

## Protocol Parity Is Not Enforced

Three hand-maintained copies of this wire contract exist:

- `crates/conduit-core/src/device.rs` (canonical, Rust),
- `esphome/components/conduit_voice/conduit_converse_embedded.h` (shipped
  firmware),
- `common/conduit_converse.h` (reference C header, see below).

They currently agree on all four message types (`end`, `started`, `done`,
`failed`), on binary-versus-text framing, and on 16 kHz mono signed 16-bit
little-endian PCM. Nothing in CI checks that they still agree: no test compares
the C/C++ constants against `device.rs`, and `firmware/tests/` only asserts
that specific symbols exist in the embedded header. A protocol change made in
`device.rs` alone will not fail any build. Update all three together, by hand,
until a real parity check exists.

## Reference Scaffold (Not Built, Not Flashed)

`common/`, `sat1/`, and `voicepe/` are a plain-C reference sketch of the wire
contract. They predate the ESPHome component and **no firmware build compiles
them**. Nothing under `esphome/` includes them; their only consumer is
`tests/conduit_converse_test.c`, which is why they are kept rather than
deleted.

Do not add board drivers here expecting them to ship. Real device behavior
lives in `esphome/components/conduit_voice/`.

`common/conduit_converse.h` provides, for reference:

- the required audio format constants,
- `CONDUIT_CONVERSE_END_JSON`,
- pipeline-name validation matching the API storage rules,
- `/v1/pipelines/{pipeline}/converse` path construction,
- parsing for `started`, `done`, and `failed` notices.

The shipped firmware does not use any of it. It uses its own copy,
`esphome/components/conduit_voice/conduit_converse_embedded.h`.

Run the firmware helper tests with:

```sh
./firmware/test.sh
```

## ESPHome Board Targets

Conduit provides ESPHome firmware targets for the two supported satellite
boards:

- `esphome/conduit-sat1.yaml` uses FutureProofHomes Satellite1 firmware from
  `futureproofhomes/satellite1-esphome` pinned to
  `592a9687206709046f475b5464941702beacb093`.
- `esphome/conduit-voicepe.yaml` uses Home Assistant Voice PE firmware from
  `esphome/home-assistant-voice-pe` pinned to
  `0579e7b9d8504264719c593474c85447253c9dc1`.

Both targets use the board hardware definitions from the named upstream
firmware sources without routing conversations through ESPHome's native
`voice_assistant`. Wake-word and action-button events call the local
`conduit_voice.start` action, which opens:

```text
ws://{conduit_server}/v1/pipelines/{conduit_pipeline}/converse
```

Set the substitutions before flashing:

- `conduit_server`: host and port for Conduit, for example `192.168.1.10:8080`.
- `conduit_pipeline`: the Conduit pipeline name.
- `conduit_scheme`: `ws` or `wss`.
- `wake_debug_assistant_id`: assistant id used for debug packets and wake
  events. Defaults to `conduit_pipeline`.
- `wake_debug_udp_host`: host running
  `~/src/wakeword/esphome-wakeword-debug/` ingest. Empty disables UDP debug
  audio.
- `wake_debug_udp_port`: UDP ingest port. Defaults to `6056`.
- `wake_debug_event_url`: debug ingest HTTP wake endpoint, for example
  `http://192.168.1.10:8000/wake_event`. Empty disables wake-event posting.

Both targets also read `wifi_ssid`, `wifi_password`, and `api_encryption_key`
from an ESPHome `secrets.yaml` you create next to the YAML. That file holds
credentials and is git-ignored, along with the `.esphome/` build directory.
Never commit either.

The local component streams microphone audio as binary WebSocket frames, sends
`{"type":"end"}` when stopped, parses Conduit text notices, and writes binary
reply frames to the board speaker.

LED and display feedback is not wired up on either target: neither YAML defines
a `light:` block, and the vendored `esphome/components/satellite1/light/`
`led_ring` platform is not referenced by any target. See the per-board notes in
`sat1/README.md` and `voicepe/README.md`.

Satellite1 also loads local `pcm5122` and `satellite1` component overlays from
`esphome/components/`. These are copied from the pinned FutureProofHomes ref
and patched only for ESPHome 2026.7's `GPIOPin::dump_summary(char *, size_t)`
signature so the firmware actually compiles with current ESPHome.

When `wake_debug_udp_host` is set, the local component also streams the same
16 kHz mono signed little-endian PCM that feeds wake-word detection to the
debug receiver using WWD2 UDP packets:

```text
magic=WWD2 assistant_id channels=1 bits=16 encoding=pcm_s16le sample_rate=16000
```

On wake-word detection, the YAML calls `conduit_voice.wake_debug_event` before
starting the Conduit conversation. That action posts to `wake_debug_event_url`
with `assistant_id={wake_debug_assistant_id}` so
`esphome-wakeword-debug` can align the wake event with the continuous WWD2
audio stream.
