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
- Handle `{"type":"done"}` or `{"type":"failed","error":"..."}` as terminal
  text frames.

The canonical Rust definitions live in
`crates/conduit-core/src/device.rs`.

The shipping firmware for both supported boards is the ESPHome build described
under [ESPHome Board Targets](#esphome-board-targets). Start there.

## Protocol Parity Is Not Enforced

Two hand-maintained copies of this wire contract exist:

- `crates/conduit-core/src/device.rs` (canonical, Rust),
- `esphome/components/conduit_voice/conduit_converse_embedded.h` (shipped
  firmware).

They currently agree on all four message types (`end`, `started`, `done`,
`failed`), on binary-versus-text framing, and on 16 kHz mono signed 16-bit
little-endian PCM. Nothing in CI checks that they still agree: no test compares
the embedded constants against `device.rs`, and `firmware/tests/` only asserts
that specific symbols exist in the embedded header. A protocol change made in
`device.rs` alone will not fail any build. Update both together, by hand, until
a real parity check exists.

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
  `conduit_voice.start`.
- Button trigger: the `btn_action` GPIO `on_multi_click` calls
  `conduit_voice.start`. Both triggers are gated on `master_mute_switch`.

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
  `conduit_voice.start`.
- Button trigger: the `center_button` GPIO `on_multi_click` calls
  `conduit_voice.start`. Both triggers are gated on `master_mute_switch`,
  which also tracks the hardware mute slider.

Still needed: LED states for connecting, listening, thinking, speaking, and
failed. The YAML defines no `light:` block, so the LED ring is dark.

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
