#!/usr/bin/env bash
# Downloads the openWakeWord models the `conduit-wake` tests score against.
#
# The models are upstream release artifacts, so they are fetched rather than
# vendored: the repository does not carry another project's binaries, and the
# version this is pinned to is visible in one place. Without them the
# detection tests skip; CI runs this first so they do not.
#
#   scripts/fetch-wake-models.sh [destination]
set -euo pipefail

VERSION="v0.5.1"
BASE="https://github.com/dscripka/openWakeWord/releases/download/${VERSION}"
DESTINATION="${1:-crates/conduit-wake/tests/models}"

# The two every installation shares, plus one phrase to score against.
MODELS=(melspectrogram.onnx embedding_model.onnx hey_jarvis_v0.1.onnx)

mkdir -p "${DESTINATION}"
for model in "${MODELS[@]}"; do
    if [[ -s "${DESTINATION}/${model}" ]]; then
        echo "have ${model}"
        continue
    fi
    echo "fetching ${model}"
    curl --fail --silent --show-error --location \
        --output "${DESTINATION}/${model}" "${BASE}/${model}"
done

echo "openWakeWord ${VERSION} models are in ${DESTINATION}"
