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

# v5.1.2 rather than the newest tag, deliberately. The v6.2.1 file of the same
# name loads and runs and reports about 0.001 for every window, including real
# speech — a detector that trims away every word while looking like it works.
# Whether that export wants a different calling convention or is simply broken
# was not worth finding out when a known-good version is one line away.
VERSION="v5.1.2"
BASE="https://github.com/snakers4/silero-vad/raw/${VERSION}/src/silero_vad/data"
DESTINATION="${1:-crates/conduit-vad/tests/models}"
MODEL="silero_vad.onnx"
# The checksum is the point of this script rather than a nicety: the failure
# above is invisible at load time, so the guard has to be on the bytes. A model
# that does not match is not scored.
SHA256="2623a2953f6ff3d2c1e61740c6cdb7168133479b267dfef114a4a3cc5bdd788f"

verify() {
    if command -v shasum >/dev/null 2>&1; then
        echo "${SHA256}  $1" | shasum -a 256 --check --status
    elif command -v sha256sum >/dev/null 2>&1; then
        echo "${SHA256}  $1" | sha256sum --check --status
    else
        echo "no shasum or sha256sum: cannot verify ${MODEL}" >&2
        return 1
    fi
}

mkdir -p "${DESTINATION}"
if [[ -s "${DESTINATION}/${MODEL}" ]] && verify "${DESTINATION}/${MODEL}"; then
    echo "have ${MODEL}"
else
    echo "fetching ${MODEL}"
    curl --fail --silent --show-error --location \
        --output "${DESTINATION}/${MODEL}" "${BASE}/${MODEL}"
    if ! verify "${DESTINATION}/${MODEL}"; then
        echo "${MODEL} does not match the pinned checksum; refusing it" >&2
        rm -f "${DESTINATION}/${MODEL}"
        exit 1
    fi
fi

echo "Silero VAD ${VERSION} is in ${DESTINATION}"
