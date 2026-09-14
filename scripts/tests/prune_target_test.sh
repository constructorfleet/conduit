#!/usr/bin/env bash
# Tests scripts/prune-target.sh without touching the real Cargo target dir.
set -uo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
readonly root
readonly script="${root}/scripts/prune-target.sh"

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

fake_cargo="${workspace}/cargo"
cat >"${fake_cargo}" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${FAKE_CARGO_LOG}"
SH
chmod +x "${fake_cargo}"

run_prune() {
    local target_dir="$1" max_bytes="$2" log="$3"
    FAKE_CARGO_LOG="${log}" CARGO="${fake_cargo}" \
        "${script}" --target-dir "${target_dir}" --max-bytes "${max_bytes}"
}

tiny_target="${workspace}/tiny-target"
mkdir -p "${tiny_target}/debug"
printf 'tiny\n' >"${tiny_target}/debug/libconduit.rlib"
tiny_log="${workspace}/tiny.log"
if output=$(run_prune "${tiny_target}" 1000000 "${tiny_log}" 2>&1); then
    if [[ -s "${tiny_log}" ]]; then
        fail "a target under the budget is left alone" "cargo was called: $(cat "${tiny_log}")"
    elif [[ "${output}" == *"within budget"* ]]; then
        pass "a target under the budget is left alone"
    else
        fail "a target under the budget is left alone" "missing budget message: ${output}"
    fi
else
    fail "a target under the budget is left alone" "exited non-zero: ${output}"
fi

large_target="${workspace}/large-target"
mkdir -p "${large_target}/debug"
printf 'larger than the deliberately miserable budget\n' >"${large_target}/debug/libconduit.rlib"
large_log="${workspace}/large.log"
if output=$(run_prune "${large_target}" 8 "${large_log}" 2>&1); then
    expected="clean --profile dev --target-dir ${large_target}"
    if grep -Fqx -- "${expected}" "${large_log}"; then
        pass "an oversized debug target cleans the dev profile"
    else
        fail "an oversized debug target cleans the dev profile" "cargo log was: $(cat "${large_log}" 2>/dev/null)"
    fi
else
    fail "an oversized debug target cleans the dev profile" "exited non-zero: ${output}"
fi

missing_target="${workspace}/missing-target"
missing_log="${workspace}/missing.log"
if output=$(run_prune "${missing_target}" 8 "${missing_log}" 2>&1); then
    if [[ -s "${missing_log}" ]]; then
        fail "a missing debug target is a no-op" "cargo was called: $(cat "${missing_log}")"
    elif [[ "${output}" == *"does not exist"* ]]; then
        pass "a missing debug target is a no-op"
    else
        fail "a missing debug target is a no-op" "missing no-op message: ${output}"
    fi
else
    fail "a missing debug target is a no-op" "exited non-zero: ${output}"
fi

if [[ "${failures}" -gt 0 ]]; then
    printf '\n%d failing\n' "${failures}" >&2
    exit 1
fi
printf '\nall prune-target tests passed\n'
