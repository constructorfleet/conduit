#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cc -std=c99 -Wall -Wextra -Werror -I "$root" \
  "$root/firmware/tests/conduit_converse_test.c" \
  -o /tmp/conduit-converse-test
/tmp/conduit-converse-test
c++ -std=c++17 -Wall -Wextra -Werror -I "$root" \
  "$root/firmware/tests/conduit_voice_embedded_test.cpp" \
  -o /tmp/conduit-voice-embedded-test
/tmp/conduit-voice-embedded-test
"$root/firmware/tests/esphome_firmware_test.sh"
