# Sat1 Firmware Target

Sat1 support must be implemented against Conduit's native conversation
WebSocket. Do not point Sat1 firmware at a Home Assistant Assist endpoint,
Tater native satellite endpoint, ESPHome voice-assistant endpoint, or UDP
wake-audio collector and call it Conduit support. Those are different
protocols.

Required behavior:

- Open `ws://<conduit-host>/v1/pipelines/<pipeline>/converse`.
- Capture microphone audio as 16 kHz mono signed 16-bit little-endian PCM.
- Send each captured audio block as a binary WebSocket frame.
- Send `CONDUIT_CONVERSE_END_JSON` after end-of-speech.
- Parse text notices with `firmware/common/conduit_converse.h`.
- Play binary frames received after `started` until `done` or `failed`.

Board work still needed:

- Sat1 microphone capture adapter.
- Sat1 speaker playback adapter.
- Wake/button trigger that starts one WebSocket turn.
- LED/display states for connecting, listening, thinking, speaking, failed.
