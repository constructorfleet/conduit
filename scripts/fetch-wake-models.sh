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

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
# shellcheck source=lib/model-fetch.sh
source "${ROOT}/scripts/lib/model-fetch.sh"

VERSION="v0.5.1"
BASE="https://github.com/dscripka/openWakeWord/releases/download/${VERSION}"
DESTINATION="${1:-crates/conduit-wake/tests/models}"

mkdir -p "${DESTINATION}"

# The two every installation shares, plus one phrase to score against.
# Checksums verified against the v0.5.1 release; see scripts/lib/model-fetch.sh
# for why a mismatch is refused rather than cached.
fetch_model "${DESTINATION}" melspectrogram.onnx "${BASE}/melspectrogram.onnx" \
    ba2b0e0f8b7b875369a2c89cb13360ff53bac436f2895cced9f479fa65eb176f
fetch_model "${DESTINATION}" embedding_model.onnx "${BASE}/embedding_model.onnx" \
    70d164290c1d095d1d4ee149bc5e00543250a7316b59f31d056cff7bd3075c1f
fetch_model "${DESTINATION}" hey_jarvis_v0.1.onnx "${BASE}/hey_jarvis_v0.1.onnx" \
    94a13cfe60075b132f6a472e7e462e8123ee70861bc3fb58434a73712ee0d2cb

echo "openWakeWord ${VERSION} models are in ${DESTINATION}"
