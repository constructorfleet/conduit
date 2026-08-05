#!/usr/bin/env bash
# Downloads the Silero VAD model `conduit-vad` scores.
#
# Fetched rather than vendored, on the same terms as the wake models: the
# repository does not carry another project's binaries, and the version this is
# pinned to is visible in one place.
#
# One file, unlike openWakeWord's three — Silero is a single model rather than a
# spectrogram, an embedder and a phrase classifier chained together.
#
#   scripts/fetch-vad-model.sh [destination]
set -euo pipefail

VERSION="v6.2.1"
BASE="https://github.com/snakers4/silero-vad/raw/${VERSION}/src/silero_vad/data"
DESTINATION="${1:-crates/conduit-vad/tests/models}"
# The 16 kHz-only export rather than the headline `silero_vad.onnx`. The default
# export dispatches on the sample rate with an ONNX `If`, and a graph whose
# branch condition a runtime cannot fold is a graph it cannot analyse — `tract`
# refuses to load it. This export has the rate baked in and so has no dispatch
# to fold, which is also why `conduit-vad` scores 16 kHz only.
MODEL="silero_vad_16k_op15.onnx"

mkdir -p "${DESTINATION}"
if [[ -s "${DESTINATION}/${MODEL}" ]]; then
    echo "have ${MODEL}"
else
    echo "fetching ${MODEL}"
    curl --fail --silent --show-error --location \
        --output "${DESTINATION}/${MODEL}" "${BASE}/${MODEL}"
fi

echo "Silero VAD ${VERSION} is in ${DESTINATION}"
