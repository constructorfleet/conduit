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
