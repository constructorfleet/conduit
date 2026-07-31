#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
sat1="$root/firmware/esphome/conduit-sat1.yaml"
voicepe="$root/firmware/esphome/conduit-voicepe.yaml"
component="$root/firmware/esphome/components/conduit_voice"
sat1_overlay="$root/firmware/esphome/components/SAT1_OVERLAY.md"

require() {
  pattern=$1
  file=$2
  if ! grep -Fq -- "$pattern" "$file"; then
    printf 'missing "%s" in %s\n' "$pattern" "$file" >&2
    return 1
  fi
}

require "futureproofhomes/satellite1-esphome" "$sat1"
require "592a9687206709046f475b5464941702beacb093" "$sat1"
require "microphone: sat1_mics" "$sat1"
require "speaker: announcement_resampling_speaker" "$sat1"
require "conduit_voice.start" "$sat1"
require "conduit_voice.wake_debug_event" "$sat1"
require "debug_udp_host" "$sat1"
require "debug_wake_event_url" "$sat1"
require "components:" "$sat1"
require "- pcm5122" "$sat1"
require "- satellite1" "$sat1"
require "ESPHome 2026.7 signature" "$sat1_overlay"
require "dump_summary(char *buffer, size_t len)" "$root/firmware/esphome/components/pcm5122/pcm_gpio.h"
require "dump_summary(char *buffer, size_t len)" "$root/firmware/esphome/components/satellite1/sat_gpio.h"

require "esphome/home-assistant-voice-pe" "$voicepe"
require "0579e7b9d8504264719c593474c85447253c9dc1" "$voicepe"
require "microphone: i2s_mics" "$voicepe"
require "speaker: announcement_resampling_speaker" "$voicepe"
require "conduit_voice.start" "$voicepe"
require "conduit_voice.wake_debug_event" "$voicepe"
require "debug_udp_host" "$voicepe"
require "debug_wake_event_url" "$voicepe"

require "esp_websocket_client_send_bin" "$component/conduit_voice.cpp"
require "esp_websocket_client_send_text" "$component/conduit_voice.cpp"
require "esp_http_client_perform" "$component/conduit_voice.cpp"
require "sendto" "$component/conduit_voice.cpp"
require "CONDUIT_VOICE_CONVERSE_END_JSON" "$component/conduit_voice.cpp"
require "CONDUIT_VOICE_AUDIO_SAMPLE_RATE_HZ" "$component/conduit_voice.cpp"
require "CONDUIT_VOICE_WWD2_HEADER_BYTES" "$component/conduit_converse_embedded.h"
require "conduit_voice_wwd2_packet" "$component/conduit_converse_embedded.h"
require "conduit_voice_converse_path" "$component/conduit_converse_embedded.h"
require "passive=True" "$component/__init__.py"
require "microphone.final_validate_microphone_source_schema" "$component/__init__.py"
require "esp32.add_idf_component(name=\"espressif/esp_websocket_client\"" "$component/__init__.py"
require "esp32.include_builtin_idf_component(\"esp_http_client\")" "$component/__init__.py"
