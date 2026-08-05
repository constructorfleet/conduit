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

refute() {
  pattern=$1
  file=$2
  if grep -Fq -- "$pattern" "$file"; then
    printf 'unwanted "%s" in %s\n' "$pattern" "$file" >&2
    return 1
  fi
}

# Every credential-bearing field, on every board, comes from `secrets.yaml`.
#
# Asserted by shape rather than by name because the name is what the old
# assertions checked: they grepped for the *key* `debug_wake_event_url` and for
# the literal `token: !secret conduit_token`, so Voice PE passing the wake
# webhook URL as a committed substitution satisfied both while Satellite1 took
# the same URL from secrets with a comment explaining why it had to. One rule,
# two boards, and the test could not see the disagreement.
#
# A Home Assistant webhook URL carries its token in the path, so the URL is the
# credential. Substitutions are committed; secrets are git-ignored.
for board in "$sat1" "$voicepe"; do
  for field in token debug_wake_event_url; do
    if ! grep -Eq "^  ${field}: !secret " "$board"; then
      printf '%s: %s must be "!secret ..." — substitutions are committed\n' \
        "$board" "$field" >&2
      exit 1
    fi
  done
  # And no credential-shaped key may be *defined* as a substitution, which is
  # how the Voice PE case arose: declared up top, interpolated below, reading
  # as ordinary configuration all the way down. Only the substitutions block is
  # scanned — `awk` stops at the next top-level key — because `!secret` uses
  # further down are the correct spelling and must not trip this.
  offender=$(
    awk '/^substitutions:/ { inside = 1; next }
         /^[a-z_]+:/ { inside = 0 }
         inside && /^  [a-z_]*(token|secret|password)[a-z_]*:/ { print }
         inside && /^  [a-z_]*_url:/ { print }' "$board"
  )
  if [ -n "$offender" ]; then
    printf '%s: credential-shaped substitution is committed:\n%s\n' \
      "$board" "$offender" >&2
    exit 1
  fi
done

require "futureproofhomes/satellite1-esphome" "$sat1"
require "592a9687206709046f475b5464941702beacb093" "$sat1"
require "microphone: sat1_mics" "$sat1"
require "speaker: announcement_resampling_speaker" "$sat1"
require "conduit_voice.start" "$sat1"
require "conduit_voice.interrupt" "$sat1"
require "conduit_voice.wake_debug_event" "$sat1"
require "debug_udp_host" "$sat1"
require "debug_wake_event_url" "$sat1"
require "token: !secret conduit_token" "$sat1"
require "max_utterance_ms" "$sat1"
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
require "conduit_voice.interrupt" "$voicepe"
require "conduit_voice.wake_debug_event" "$voicepe"
require "debug_udp_host" "$voicepe"
require "debug_wake_event_url" "$voicepe"
require "token: !secret conduit_token" "$voicepe"
require "max_utterance_ms" "$voicepe"

require "esp_websocket_client_send_bin" "$component/conduit_voice.cpp"
require "esp_websocket_client_send_text" "$component/conduit_voice.cpp"
require "config.headers" "$component/conduit_voice.cpp"
require "reconnect_timeout_ms = 1000" "$component/conduit_voice.cpp"
require "esp_http_client_perform" "$component/conduit_voice.cpp"
require "sendto" "$component/conduit_voice.cpp"
require "CONDUIT_VOICE_CONVERSE_END_JSON" "$component/conduit_voice.cpp"
# Interrupting must go over the wire. Dropping the socket instead would end the
# turn too, but the server could not tell it apart from a device that died.
require "CONDUIT_VOICE_CONVERSE_STOP_JSON" "$component/conduit_voice.cpp"
require "InterruptAction" "$component/__init__.py"
require "CONDUIT_VOICE_AUDIO_SAMPLE_RATE_HZ" "$component/conduit_voice.cpp"
require "CONDUIT_VOICE_WWD2_HEADER_BYTES" "$component/conduit_converse_embedded.h"
require "conduit_voice_wwd2_packet" "$component/conduit_converse_embedded.h"
require "conduit_voice_utterance_timeout_elapsed" "$component/conduit_converse_embedded.h"
require "conduit_voice_converse_path" "$component/conduit_converse_embedded.h"
# A failure the device cannot explain is a failure someone debugs by reading the
# server, so the parsed reason must reach the log.
require "notice.error" "$component/conduit_voice.cpp"
# The credential goes in a header, never the URI: the URI is logged on two
# failure paths here and recorded into the server's trace spans.
require "config.headers = this->build_headers_()" "$component/conduit_voice.cpp"
require "Authorization: Bearer " "$component/conduit_voice.cpp"
# `headers` is borrowed by the client, so the string has to outlive the config.
require "std::string headers_;" "$component/conduit_voice.h"
require "CONF_TOKEN" "$component/__init__.py"
require "set_token" "$component/__init__.py"
# A boot log is the first thing anyone pastes into an issue, so it may say
# whether a token is configured but never what it is.
refute "this->token_.c_str()" "$component/conduit_voice.cpp"
require "passive=True" "$component/__init__.py"
require "microphone.final_validate_microphone_source_schema" "$component/__init__.py"
require "esp32.add_idf_component(name=\"espressif/esp_websocket_client\"" "$component/__init__.py"
require "esp32.include_builtin_idf_component(\"esp_http_client\")" "$component/__init__.py"
