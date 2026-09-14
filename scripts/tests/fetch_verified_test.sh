#!/usr/bin/env bash
# Tests scripts/lib/fetch-verified.sh, the retry-and-checksum logic shared by
# scripts/fetch-wake-models.sh and scripts/fetch-vad-model.sh.
#
# curl is replaced with a fake on PATH that logs its invocations and writes
# scripted content, so every case runs the real fetch_verified function
# without touching the network.
#
#   scripts/tests/fetch_verified_test.sh
set -uo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
readonly root

# shellcheck source=../lib/fetch-verified.sh
source "${root}/scripts/lib/fetch-verified.sh"

failures=0

pass() {
    printf 'ok   %s\n' "$1"
}

fail() {
    printf 'FAIL %s\n     %s\n' "$1" "$2" >&2
    failures=$((failures + 1))
}

workspace=$(mktemp -d)
readonly workspace
trap 'rm -rf "${workspace}"' EXIT

good_content="the actual model bytes"
bad_content="an html error page, or a truncated download"
good_sha256=$(printf '%s' "${good_content}" | shasum -a 256 | cut -d' ' -f1)
readonly good_content bad_content good_sha256

fake_bin="${workspace}/bin"
mkdir -p "${fake_bin}"
export PATH="${fake_bin}:${PATH}"

# The fake curl writes CURL_CONTENTS (one line per call, consumed in order) to
# whatever --output path it was given, and logs its full argument list so a
# test can assert both what was fetched and how many times.
cat >"${fake_bin}/curl" <<'SH'
#!/usr/bin/env bash
echo "$*" >>"${CURL_LOG}"
output=""
while [[ $# -gt 0 ]]; do
    if [[ "$1" == "--output" ]]; then
        output="$2"
        break
    fi
    shift
done
line=$(head -n 1 "${CURL_CONTENTS}")
sed -i.bak '1d' "${CURL_CONTENTS}" && rm -f "${CURL_CONTENTS}.bak"
if [[ "${line}" == "FAIL" ]]; then
    echo "curl: fake transient failure" >&2
    exit 1
fi
printf '%s' "${line}" >"${output}"
SH
chmod +x "${fake_bin}/curl"

queue() {
    : >"${workspace}/contents"
    for line in "$@"; do
        printf '%s\n' "${line}" >>"${workspace}/contents"
    done
}

# --- Fresh fetch --------------------------------------------------------------

dest="${workspace}/fresh.onnx"
export CURL_CONTENTS="${workspace}/contents"
export CURL_LOG="${workspace}/curl.log"
: >"${CURL_LOG}"
queue "${good_content}"
if fetch_verified "${dest}" "https://example.invalid/fresh.onnx" "${good_sha256}" >/tmp/out 2>&1; then
    if [[ "$(cat "${dest}")" == "${good_content}" ]] && grep -q -- "--retry" "${CURL_LOG}"; then
        pass "a fresh download that matches the checksum is kept"
    else
        fail "a fresh download that matches the checksum is kept" "dest=$(cat "${dest}" 2>/dev/null) log=$(cat "${CURL_LOG}")"
    fi
else
    fail "a fresh download that matches the checksum is kept" "exited non-zero: $(cat /tmp/out)"
fi

# --- Cache hit -----------------------------------------------------------------

dest="${workspace}/cached.onnx"
printf '%s' "${good_content}" >"${dest}"
: >"${CURL_LOG}"
queue "FAIL"
if fetch_verified "${dest}" "https://example.invalid/cached.onnx" "${good_sha256}" >/tmp/out 2>&1; then
    if [[ ! -s "${CURL_LOG}" ]]; then
        pass "a cached file matching the checksum is not re-fetched"
    else
        fail "a cached file matching the checksum is not re-fetched" "curl was called: $(cat "${CURL_LOG}")"
    fi
else
    fail "a cached file matching the checksum is not re-fetched" "exited non-zero: $(cat /tmp/out)"
fi

# --- Corrupt cache ---------------------------------------------------------------

dest="${workspace}/corrupt.onnx"
printf '%s' "${bad_content}" >"${dest}"
: >"${CURL_LOG}"
queue "${good_content}"
if fetch_verified "${dest}" "https://example.invalid/corrupt.onnx" "${good_sha256}" >/tmp/out 2>&1; then
    if [[ "$(cat "${dest}")" == "${good_content}" ]]; then
        pass "a cached file that does not match the checksum is re-fetched"
    else
        fail "a cached file that does not match the checksum is re-fetched" "dest was: $(cat "${dest}")"
    fi
else
    fail "a cached file that does not match the checksum is re-fetched" "exited non-zero: $(cat /tmp/out)"
fi

# --- Mismatch retried once, then succeeds ------------------------------------

dest="${workspace}/flaky.onnx"
: >"${CURL_LOG}"
queue "${bad_content}" "${good_content}"
if fetch_verified "${dest}" "https://example.invalid/flaky.onnx" "${good_sha256}" >/tmp/out 2>&1; then
    calls=$(wc -l <"${CURL_LOG}")
    if [[ "$(cat "${dest}")" == "${good_content}" && "${calls}" -eq 2 ]]; then
        pass "a checksum mismatch is retried and can still succeed"
    else
        fail "a checksum mismatch is retried and can still succeed" "dest=$(cat "${dest}") calls=${calls}"
    fi
else
    fail "a checksum mismatch is retried and can still succeed" "exited non-zero: $(cat /tmp/out)"
fi

# --- Mismatch that survives the retry fails loudly, naming the model ---------

dest="${workspace}/bad-model.onnx"
: >"${CURL_LOG}"
queue "${bad_content}" "${bad_content}"
output=$(fetch_verified "${dest}" "https://example.invalid/bad-model.onnx" "${good_sha256}" 2>&1)
status=$?
if [[ "${status}" -ne 0 && "${output}" == *"bad-model.onnx"* && ! -e "${dest}" ]]; then
    pass "a mismatch that survives the retry fails and names the model"
else
    fail "a mismatch that survives the retry fails and names the model" \
        "status=${status} exists=$([[ -e "${dest}" ]] && echo yes || echo no) output=${output}"
fi

if [[ "${failures}" -gt 0 ]]; then
    printf '\n%d failing\n' "${failures}" >&2
    exit 1
fi
printf '\nall fetch-verified tests passed\n'
