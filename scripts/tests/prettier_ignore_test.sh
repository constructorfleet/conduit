#!/usr/bin/env bash
# Tests that the root .prettierignore keeps hand-written markdown outside the
# frontend out of Prettier, and nothing else. Needs the frontend's node_modules
# (`npm ci` in frontend/) because that is where Prettier is pinned.
set -uo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
readonly root
readonly prettier="${root}/frontend/node_modules/.bin/prettier"

if [[ ! -x "${prettier}" ]]; then
    printf 'FAIL prettier is not installed; run `npm ci` in %s/frontend first\n' "${root}" >&2
    exit 1
fi

failures=0

pass() {
    printf 'ok   %s\n' "$1"
}

fail() {
    printf 'FAIL %s\n     %s\n' "$1" "$2" >&2
    failures=$((failures + 1))
}

cleanup() {
    rm -f "${root}/.prettier-ignore-test."*
}
trap cleanup EXIT

check() {
    (cd "${root}" && "${prettier}" --check "$@" 2>&1)
}

if output=$(check CHANGELOG.md README.md docs/api.md); then
    pass "root and docs markdown are ignored by prettier"
else
    fail "root and docs markdown are ignored by prettier" "exited non-zero: ${output}"
fi

# A deliberately unformatted markdown file at the root proves the ignore, not
# the file's current state, is what makes the check pass.
unformatted_md=".prettier-ignore-test.md"
printf '*   badly   spaced\n\n\n\n* list\n' >"${root}/${unformatted_md}"
if output=$(check "${unformatted_md}"); then
    pass "an unformatted root markdown file is still ignored"
else
    fail "an unformatted root markdown file is still ignored" "exited non-zero: ${output}"
fi

# The ignore must be narrow: an unformatted non-markdown file at the root is
# still reported.
unformatted_json=".prettier-ignore-test.json"
printf '{"a":1,\n\n"b":   2}\n' >"${root}/${unformatted_json}"
if output=$(check "${unformatted_json}"); then
    fail "a non-markdown root file is still checked" "prettier accepted an unformatted file: ${output}"
else
    pass "a non-markdown root file is still checked"
fi

# Markdown under frontend/ is the frontend's business (its own .prettierignore
# and `npm run format`), so the root ignore must not swallow it.
unformatted_frontend_md="frontend/.prettier-ignore-test.md"
printf '*   badly   spaced\n\n\n\n* list\n' >"${root}/${unformatted_frontend_md}"
if output=$(check "${unformatted_frontend_md}"); then
    fail "frontend markdown is not ignored by the root ignore" "prettier accepted an unformatted file: ${output}"
else
    pass "frontend markdown is not ignored by the root ignore"
fi
rm -f "${root}/${unformatted_frontend_md}"

if ((failures > 0)); then
    printf '%d failure(s)\n' "${failures}" >&2
    exit 1
fi
