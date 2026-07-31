# Sat1 Firmware Target

The shipping Satellite1 firmware is `firmware/esphome/conduit-sat1.yaml` plus
the `conduit_voice` ESPHome component. This directory is only a reference
scaffold: `conduit_sat1_config.h` defines board id/name macros for the plain-C
sketch in `firmware/common/` and is not compiled into any firmware image. See
[`../README.md`](../README.md) for why it is kept.

Sat1 support must be implemented against Conduit's native conversation
WebSocket. Do not point Sat1 firmware at a Home Assistant Assist endpoint,
Tater native satellite endpoint, ESPHome voice-assistant endpoint, or UDP
wake-audio collector and call it Conduit support. Those are different
protocols.

Required behavior:

- Open `ws://<conduit-host>/v1/pipelines/<pipeline>/converse`.
- Capture microphone audio as 16 kHz mono signed 16-bit little-endian PCM.
- Send each captured audio block as a binary WebSocket frame.
- Send the `{"type":"end"}` text frame after end-of-speech.
- Parse `started`, `done`, and `failed` text notices.
- Play binary frames received after `started` until `done` or `failed`.

`firmware/esphome/conduit-sat1.yaml` and
`firmware/esphome/components/conduit_voice/` implement all of the above.

Board work already done in `conduit-sat1.yaml`:

- Microphone capture via the `satellite1` microphone platform (`sat1_mics`),
  downmixed to 16 kHz mono by the `conduit_voice` component.
- Speaker playback via the `i2s_audio` speaker behind a mixer and a resampler
  (`announcement_resampling_speaker`), through the `satellite1` DAC proxy.
- Wake trigger: `micro_wake_word` `on_wake_word_detected` calls
  `conduit_voice.start`.
- Button trigger: the `btn_action` GPIO `on_multi_click` calls
  `conduit_voice.start`. Both triggers are gated on `master_mute_switch`.

Board work still needed:

- LED/display states for connecting, listening, thinking, speaking, failed.
  The YAML defines no `light:` block, and the vendored
  `firmware/esphome/components/satellite1/light/led_ring.cpp` is not referenced
  by any target, so the LED ring is currently dark.
