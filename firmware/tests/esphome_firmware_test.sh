#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
sat1="$root/firmware/esphome/conduit-sat1.yaml"
voicepe="$root/firmware/esphome/conduit-voicepe.yaml"
component="$root/firmware/esphome/components/conduit_voice"

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

require "esphome/home-assistant-voice-pe" "$voicepe"
require "0579e7b9d8504264719c593474c85447253c9dc1" "$voicepe"
require "microphone: i2s_mics" "$voicepe"
require "speaker: announcement_resampling_speaker" "$voicepe"
require "conduit_voice.start" "$voicepe"

require "esp_websocket_client_send_bin" "$component/conduit_voice.cpp"
require "esp_websocket_client_send_text" "$component/conduit_voice.cpp"
require "CONDUIT_VOICE_CONVERSE_END_JSON" "$component/conduit_voice.cpp"
require "CONDUIT_VOICE_AUDIO_SAMPLE_RATE_HZ" "$component/conduit_voice.cpp"
require "conduit_voice_converse_path" "$component/conduit_converse_embedded.h"
require "microphone.final_validate_microphone_source_schema" "$component/__init__.py"
require "esp32.add_idf_component(name=\"espressif/esp_websocket_client\"" "$component/__init__.py"
