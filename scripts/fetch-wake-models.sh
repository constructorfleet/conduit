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

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
# shellcheck source=lib/fetch-verified.sh
source "${root}/scripts/lib/fetch-verified.sh"

VERSION="v0.5.1"
BASE="https://github.com/dscripka/openWakeWord/releases/download/${VERSION}"
DESTINATION="${1:-crates/conduit-wake/tests/models}"

# The two every installation shares, plus one phrase to score against. Pinned
# checksums are the guard against a truncated download or an HTML error page
# that `--fail` did not catch: either loads as an ONNX file and then scores
# wrongly, and a wake detector that reports nothing looks like a quiet room.
#
# Parallel arrays rather than an associative array: the macOS-shipped bash
# (3.2) has no `declare -A`, and this needs to run there as well as in CI.
MODELS=(melspectrogram.onnx embedding_model.onnx hey_jarvis_v0.1.onnx)
SHA256S=(
    ba2b0e0f8b7b875369a2c89cb13360ff53bac436f2895cced9f479fa65eb176f
    70d164290c1d095d1d4ee149bc5e00543250a7316b59f31d056cff7bd3075c1f
    94a13cfe60075b132f6a472e7e462e8123ee70861bc3fb58434a73712ee0d2cb
)

mkdir -p "${DESTINATION}"
for i in "${!MODELS[@]}"; do
    model="${MODELS[${i}]}"
    fetch_verified "${DESTINATION}/${model}" "${BASE}/${model}" "${SHA256S[${i}]}"
done

echo "openWakeWord ${VERSION} models are in ${DESTINATION}"
