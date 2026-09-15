#!/usr/bin/env bash
# Shared fetch-verify-retry logic for pinned model downloads.
#
# Both fetch-vad-model.sh and fetch-wake-models.sh pull third-party release
# artifacts and must not trust bytes they didn't check: a truncated download or
# a re-cut upstream file loads fine and then scores wrongly, which looks like a
# quiet room rather than a failure. The checksum is the guard; the retry is
# what makes it safe to keep asking after a dropped connection instead of
# failing a pull request over a single lost packet.
#
# Meant to be sourced, not executed.

verify_model_checksum() {
    local path="$1" sha256="$2"
    if command -v shasum >/dev/null 2>&1; then
        echo "${sha256}  ${path}" | shasum -a 256 --check --status
    elif command -v sha256sum >/dev/null 2>&1; then
        echo "${sha256}  ${path}" | sha256sum --check --status
    else
        echo "no shasum or sha256sum: cannot verify ${path}" >&2
        return 1
    fi
}

# fetch_model DESTINATION MODEL URL SHA256
#
# Leaves DESTINATION/MODEL in place only if its bytes match SHA256. A cached
# file that doesn't match is deleted and re-fetched rather than trusted. Each
# attempt's curl call retries transient HTTP/connection failures on its own
# (--retry); MODEL_FETCH_ATTEMPTS (default 3) is a second retry layer above
# that: it re-runs the whole download if curl still fails after exhausting
# its own retries, and it also catches what curl's retry can't see — a
# download that succeeds but whose bytes don't match.
fetch_model() {
    local dest="$1" model="$2" url="$3" sha256="$4"
    local path="${dest}/${model}"
    local attempts="${MODEL_FETCH_ATTEMPTS:-3}"

    if [[ -s "${path}" ]] && verify_model_checksum "${path}" "${sha256}"; then
        echo "have ${model}"
        return 0
    fi

    local attempt=1
    while ((attempt <= attempts)); do
        echo "fetching ${model} (attempt ${attempt}/${attempts})"
        rm -f "${path}"
        if "${CURL:-curl}" --fail --silent --show-error --location \
            --retry 5 --retry-delay 2 --retry-all-errors \
            --output "${path}" "${url}"; then
            if verify_model_checksum "${path}" "${sha256}"; then
                return 0
            fi
            echo "${model} does not match the pinned checksum (attempt ${attempt}/${attempts})" >&2
        else
            echo "${model} download failed (attempt ${attempt}/${attempts})" >&2
        fi
        rm -f "${path}"
        attempt=$((attempt + 1))
    done

    echo "${model} could not be fetched and verified after ${attempts} attempts; refusing it" >&2
    return 1
}
