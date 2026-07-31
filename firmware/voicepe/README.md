# VoicePE Firmware Target

The shipping Voice PE firmware is `firmware/esphome/conduit-voicepe.yaml` plus
the `conduit_voice` ESPHome component. This directory is only a reference
scaffold: `conduit_voicepe_config.h` defines board id/name macros for the
plain-C sketch in `firmware/common/` and is not compiled into any firmware
image. See [`../README.md`](../README.md) for why it is kept.

VoicePE support must be implemented against Conduit's native conversation
WebSocket. Do not reuse a Home Assistant Assist, Tater native satellite,
ESPHome voice-assistant, or UDP wake-audio firmware path unless it has been
rewired to this exact Conduit protocol.

Required behavior:

- Open `ws://<conduit-host>/v1/pipelines/<pipeline>/converse`.
- Capture microphone audio as 16 kHz mono signed 16-bit little-endian PCM.
- Send each captured audio block as a binary WebSocket frame.
- Send the `{"type":"end"}` text frame after end-of-speech.
- Parse `started`, `done`, and `failed` text notices.
- Play binary frames received after `started` until `done` or `failed`.

`firmware/esphome/conduit-voicepe.yaml` and
`firmware/esphome/components/conduit_voice/` implement all of the above.

Board work already done in `conduit-voicepe.yaml`:

- Microphone capture via the `i2s_audio` microphone platform (`i2s_mics`) at
  16 kHz, downmixed to mono by the `conduit_voice` component.
- Speaker playback via the `i2s_audio` speaker behind a resampler
  (`announcement_resampling_speaker`), through the `aic3204` DAC.
- Wake trigger: `micro_wake_word` `on_wake_word_detected` calls
  `conduit_voice.start`.
- Button trigger: the `center_button` GPIO `on_multi_click` calls
  `conduit_voice.start`. Both triggers are gated on `master_mute_switch`,
  which also tracks the hardware mute slider.

Board work still needed:

- LED states for connecting, listening, thinking, speaking, failed. The YAML
  defines no `light:` block, so the LED ring is currently dark.
