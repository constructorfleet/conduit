#!/usr/bin/env bash
# Tests scripts/lib/model-fetch.sh: the shared fetch-verify-retry logic behind
# fetch-vad-model.sh and fetch-wake-models.sh, without touching the network.
set -uo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
readonly root
readonly lib="${root}/scripts/lib/model-fetch.sh"

failures=0

pass() {
    printf 'ok   %s\n' "$1"
}

fail() {
    printf 'FAIL %s\n     %s\n' "$1" "$2" >&2
    failures=$((failures + 1))
}

# shellcheck source=../lib/model-fetch.sh
source "${lib}"

workspace=$(mktemp -d)
readonly workspace
trap 'rm -rf "${workspace}"' EXIT

GOOD_CONTENT="test content"
readonly GOOD_CONTENT
GOOD_SHA256=$(printf '%s\n' "${GOOD_CONTENT}" | shasum -a 256 | awk '{print $1}')
readonly GOOD_SHA256
BAD_CONTENT="wrong content"
readonly BAD_CONTENT

fake_curl() {
    local name="$1" body="$2"
    local path="${workspace}/${name}"
    cat >"${path}" <<SH
#!/usr/bin/env bash
${body}
SH
    chmod +x "${path}"
    printf '%s\n' "${path}"
}

# 1. A fresh download that succeeds on the first attempt is verified and kept.
dest="${workspace}/case1"
mkdir -p "${dest}"
log="${workspace}/case1.log"
curl_ok=$(fake_curl case1-curl "
printf '%s\n' \"\$*\" >>'${log}'
while [[ \$# -gt 0 ]]; do
    if [[ \"\$1\" == --output ]]; then
        printf '${GOOD_CONTENT}\n' > \"\$2\"
        break
    fi
    shift
done
")
if CURL="${curl_ok}" fetch_model "${dest}" model.onnx "https://example.invalid/model.onnx" "${GOOD_SHA256}"; then
    if [[ "$(cat "${dest}/model.onnx")" == "${GOOD_CONTENT}" ]]; then
        pass "a fresh download that verifies is kept"
    else
        fail "a fresh download that verifies is kept" "unexpected content: $(cat "${dest}/model.onnx")"
    fi
else
    fail "a fresh download that verifies is kept" "fetch_model exited non-zero"
fi

# 2. A cached file that already matches the checksum is not re-downloaded.
dest="${workspace}/case2"
mkdir -p "${dest}"
printf '%s\n' "${GOOD_CONTENT}" >"${dest}/model.onnx"
log="${workspace}/case2.log"
curl_should_not_run=$(fake_curl case2-curl "
printf 'called\n' >>'${log}'
")
if CURL="${curl_should_not_run}" fetch_model "${dest}" model.onnx "https://example.invalid/model.onnx" "${GOOD_SHA256}"; then
    if [[ -s "${log}" ]]; then
        fail "a cached file matching the checksum is not re-fetched" "curl was called"
    else
        pass "a cached file matching the checksum is not re-fetched"
    fi
else
    fail "a cached file matching the checksum is not re-fetched" "fetch_model exited non-zero"
fi

# 3. A cached file that does NOT match the checksum is re-fetched, not trusted.
dest="${workspace}/case3"
mkdir -p "${dest}"
printf '%s\n' "${BAD_CONTENT}" >"${dest}/model.onnx"
log="${workspace}/case3.log"
curl_fixes_it=$(fake_curl case3-curl "
printf 'called\n' >>'${log}'
while [[ \$# -gt 0 ]]; do
    if [[ \"\$1\" == --output ]]; then
        printf '${GOOD_CONTENT}\n' > \"\$2\"
        break
    fi
    shift
done
")
if CURL="${curl_fixes_it}" fetch_model "${dest}" model.onnx "https://example.invalid/model.onnx" "${GOOD_SHA256}"; then
    if [[ -s "${log}" ]] && [[ "$(cat "${dest}/model.onnx")" == "${GOOD_CONTENT}" ]]; then
        pass "a cached file that fails the checksum is re-fetched rather than trusted"
    else
        fail "a cached file that fails the checksum is re-fetched rather than trusted" \
            "log: $(cat "${log}" 2>/dev/null), content: $(cat "${dest}/model.onnx" 2>/dev/null)"
    fi
else
    fail "a cached file that fails the checksum is re-fetched rather than trusted" "fetch_model exited non-zero"
fi

# 4. A transient failure on the first attempt is retried and can still succeed.
dest="${workspace}/case4"
mkdir -p "${dest}"
counter="${workspace}/case4.count"
printf '0\n' >"${counter}"
curl_flaky=$(fake_curl case4-curl "
n=\$(cat '${counter}')
n=\$((n + 1))
printf '%s\n' \"\$n\" >'${counter}'
if [[ \"\$n\" -lt 2 ]]; then
    echo 'transient failure' >&2
    exit 7
fi
while [[ \$# -gt 0 ]]; do
    if [[ \"\$1\" == --output ]]; then
        printf '${GOOD_CONTENT}\n' > \"\$2\"
        break
    fi
    shift
done
")
if MODEL_FETCH_ATTEMPTS=3 CURL="${curl_flaky}" \
    fetch_model "${dest}" model.onnx "https://example.invalid/model.onnx" "${GOOD_SHA256}"; then
    if [[ "$(cat "${dest}/model.onnx")" == "${GOOD_CONTENT}" ]]; then
        pass "a transient download failure is retried rather than failing immediately"
    else
        fail "a transient download failure is retried rather than failing immediately" \
            "unexpected content: $(cat "${dest}/model.onnx" 2>/dev/null)"
    fi
else
    fail "a transient download failure is retried rather than failing immediately" "fetch_model exited non-zero"
fi

# 5. A mismatch that persists after retrying fails loudly and leaves no file.
dest="${workspace}/case5"
mkdir -p "${dest}"
log="${workspace}/case5.log"
curl_always_wrong=$(fake_curl case5-curl "
printf 'called\n' >>'${log}'
while [[ \$# -gt 0 ]]; do
    if [[ \"\$1\" == --output ]]; then
        printf '${BAD_CONTENT}\n' > \"\$2\"
        break
    fi
    shift
done
")
if output=$(MODEL_FETCH_ATTEMPTS=3 CURL="${curl_always_wrong}" \
    fetch_model "${dest}" model.onnx "https://example.invalid/model.onnx" "${GOOD_SHA256}" 2>&1); then
    fail "a persistent checksum mismatch fails loudly and names the model" "fetch_model exited zero: ${output}"
else
    attempts=$(wc -l <"${log}" | tr -d ' ')
    if [[ -e "${dest}/model.onnx" ]]; then
        fail "a persistent checksum mismatch fails loudly and names the model" "corrupt file left behind"
    elif [[ "${attempts}" != "3" ]]; then
        fail "a persistent checksum mismatch fails loudly and names the model" "expected 3 attempts, curl ran ${attempts} times"
    elif [[ "${output}" != *"model.onnx"* ]]; then
        fail "a persistent checksum mismatch fails loudly and names the model" "message does not name the model: ${output}"
    else
        pass "a persistent checksum mismatch fails loudly and names the model"
    fi
fi

if [[ "${failures}" -gt 0 ]]; then
    printf '\n%d failing\n' "${failures}" >&2
    exit 1
fi
printf '\nall model-fetch tests passed\n'
