#!/usr/bin/env bash
# Shared retry-and-checksum fetch used by scripts/fetch-wake-models.sh and
# scripts/fetch-vad-model.sh. Meant to be sourced, not executed.
#
# A transient failure reaching a third-party release CDN has nothing to do
# with the change under review, so curl retries with backoff rather than
# failing the job on the first dropped packet. But a retry is only safe
# because every result — cached or freshly fetched — is checked against a
# pinned SHA-256 first: without that guard a retry could re-download the same
# corrupt bytes and report success, and a corrupt model loads and scores
# wrongly rather than failing loudly.

# fetch_verified <destination-file> <url> <sha256> [retries]
#
# Leaves a verified file at <destination-file>. A cached file that already
# matches is left alone and reported; anything else is (re)fetched, verified,
# and on a mismatch retried exactly once before failing and naming the file.
#
# [retries] (default 5) controls only curl's `--retry` count for transient
# HTTP/connection failures on a single download attempt. It has no effect on
# the checksum-mismatch retry above, which is a separate, fixed one-time retry
# regardless of this value.
fetch_verified() {
    local destination="$1" url="$2" sha256="$3" retries="${4:-5}"
    local name attempt
    name="$(basename "${destination}")"

    if [[ -s "${destination}" ]] && _fetch_verified_checksum_ok "${destination}" "${sha256}"; then
        echo "have ${name}"
        return 0
    fi

    for attempt in 1 2; do
        echo "fetching ${name}"
        curl --fail --silent --show-error --location \
            --retry "${retries}" --retry-all-errors \
            --output "${destination}" "${url}"

        if _fetch_verified_checksum_ok "${destination}" "${sha256}"; then
            return 0
        fi

        if [[ "${attempt}" -eq 1 ]]; then
            echo "${name} did not match the pinned checksum; retrying" >&2
        fi
        rm -f "${destination}"
    done

    echo "${name} does not match the pinned checksum; refusing it" >&2
    return 1
}

_fetch_verified_checksum_ok() {
    local file="$1" sha256="$2"
    if command -v shasum >/dev/null 2>&1; then
        echo "${sha256}  ${file}" | shasum -a 256 --check --status
    elif command -v sha256sum >/dev/null 2>&1; then
        echo "${sha256}  ${file}" | sha256sum --check --status
    else
        echo "no shasum or sha256sum: cannot verify $(basename "${file}")" >&2
        return 1
    fi
}
