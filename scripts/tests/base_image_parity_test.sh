#!/usr/bin/env bash
# Tests that the torch-based service images and the tags Publish builds from
# name the same base image.
#
# services/vox and services/wyoming-asr default `BASE_IMAGE` in their
# Dockerfiles, and .github/workflows/publish.yml passes its own `base:` for the
# published CPU tag. Three copies of one value drift: #280 was exactly that,
# the root image moving to trixie while these stayed on bookworm. A local build
# and the published image disagreeing about the distribution is the kind of
# difference that only shows up at runtime.
#
# The GPU tags deliberately use a CUDA base and are not compared here.
set -uo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
readonly root

failures=0

pass() {
    printf 'ok   %s\n' "$1"
}

fail() {
    printf 'FAIL %s\n     %s\n' "$1" "$2" >&2
    failures=$((failures + 1))
}

# Default of `ARG BASE_IMAGE=...` in a service Dockerfile.
dockerfile_base() {
    sed -ne 's/^ARG BASE_IMAGE=\(.*\)$/\1/p' "$1" | head -1
}

vox=$(dockerfile_base "${root}/services/vox/Dockerfile")
asr=$(dockerfile_base "${root}/services/wyoming-asr/Dockerfile")
# The CPU row of the publish matrix; the CUDA row is meant to differ.
publish=$(sed -ne 's/^ *base: \(python:.*\)$/\1/p' "${root}/.github/workflows/publish.yml" | head -1)

if [[ -z "${vox}" || -z "${asr}" || -z "${publish}" ]]; then
    fail "every base image is readable" \
        "vox='${vox}' wyoming-asr='${asr}' publish='${publish}'"
else
    pass "every base image is readable"

    if [[ "${vox}" == "${asr}" ]]; then
        pass "vox and wyoming-asr share a base image"
    else
        fail "vox and wyoming-asr share a base image" \
            "vox builds on ${vox} but wyoming-asr builds on ${asr}"
    fi

    if [[ "${vox}" == "${publish}" ]]; then
        pass "the published CPU tag uses the Dockerfile's base image"
    else
        fail "the published CPU tag uses the Dockerfile's base image" \
            "the Dockerfile defaults to ${vox} but Publish builds from ${publish}"
    fi
fi

if (( failures > 0 )); then
    printf '\n%d check(s) failed\n' "${failures}" >&2
    exit 1
fi
