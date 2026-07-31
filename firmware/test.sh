#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
c++ -std=c++17 -Wall -Wextra -Werror -I "$root" \
  "$root/firmware/tests/conduit_voice_embedded_test.cpp" \
  -o /tmp/conduit-voice-embedded-test
/tmp/conduit-voice-embedded-test
# The fixture is generated from the canonical Rust definitions and checked in,
# so this runs without a Rust build. `cargo test -p conduit-api` is what fails
# if the two have drifted.
c++ -std=c++17 -Wall -Wextra -Werror -I "$root" \
  "$root/firmware/tests/conduit_notice_fixture_test.cpp" \
  -o /tmp/conduit-notice-fixture-test
/tmp/conduit-notice-fixture-test "$root"
"$root/firmware/tests/esphome_firmware_test.sh"
