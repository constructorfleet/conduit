# VoicePE Firmware Target

VoicePE support must be implemented against Conduit's native conversation
WebSocket. Do not reuse a Home Assistant Assist, Tater native satellite,
ESPHome voice-assistant, or UDP wake-audio firmware path unless it has been
rewired to this exact Conduit protocol.

Required behavior:

- Open `ws://<conduit-host>/v1/pipelines/<pipeline>/converse`.
- Capture microphone audio as 16 kHz mono signed 16-bit little-endian PCM.
- Send each captured audio block as a binary WebSocket frame.
- Send `CONDUIT_CONVERSE_END_JSON` after end-of-speech.
- Parse text notices with `firmware/common/conduit_converse.h`.
- Play binary frames received after `started` until `done` or `failed`.

Board work still needed:

- VoicePE microphone capture adapter.
- VoicePE speaker playback adapter.
- Wake/button trigger that starts one WebSocket turn.
- LED states for connecting, listening, thinking, speaking, failed.
