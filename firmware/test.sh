#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
c++ -std=c++17 -Wall -Wextra -Werror -I "$root" \
  "$root/firmware/tests/conduit_voice_embedded_test.cpp" \
  -o /tmp/conduit-voice-embedded-test
/tmp/conduit-voice-embedded-test
"$root/firmware/tests/esphome_firmware_test.sh"
