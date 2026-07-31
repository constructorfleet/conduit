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
`crates/conduit-core/src/device.rs`. The files under `sat1/` and `voicepe/`
are board integration targets for this protocol, not wrappers around any other
assistant protocol.

## Shared Helpers

`common/conduit_converse.h` provides:

- the required audio format constants,
- `CONDUIT_CONVERSE_END_JSON`,
- pipeline-name validation matching the API storage rules,
- `/v1/pipelines/{pipeline}/converse` path construction,
- parsing for `started`, `done`, and `failed` notices.

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

The local component streams microphone audio as binary WebSocket frames, sends
`{"type":"end"}` when stopped, parses Conduit text notices, and writes binary
reply frames to the board speaker.

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
