#!/usr/bin/env bash
# Keeps Cargo's dev-profile artifacts from growing without bound.
set -euo pipefail

readonly SELF="${0##*/}"

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
readonly ROOT

target_dir="${CARGO_TARGET_DIR:-${ROOT}/target}"
max_bytes="${CONDUIT_TARGET_MAX_BYTES:-53687091200}"

usage() {
    cat <<USAGE
${SELF} - prune oversized Cargo dev artifacts

Usage: scripts/prune-target.sh [options]

Options:
  --target-dir DIR    Cargo target directory (default: \$CARGO_TARGET_DIR or ./target).
  --max-bytes BYTES   Maximum allowed target/debug size before cleaning
                      (default: ${max_bytes}; 0 disables pruning).
  -h, --help          Show this help.
USAGE
}

die() {
    printf '%s: %s\n' "${SELF}" "$1" >&2
    exit 2
}

require_value() {
    if [[ $# -lt 2 ]]; then
        die "$1 needs a value"
    fi
}

require_unsigned_integer() {
    local flag="$1" value="$2"
    case "${value}" in
        '' | *[!0-9]*) die "${flag} needs a byte count, got '${value}'" ;;
    esac
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target-dir)
            require_value "$@"
            target_dir="$2"
            shift 2
            ;;
        --max-bytes)
            require_value "$@"
            require_unsigned_integer --max-bytes "$2"
            max_bytes="$2"
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            printf '%s: unknown option %s\n\n' "${SELF}" "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

require_unsigned_integer CONDUIT_TARGET_MAX_BYTES "${max_bytes}"

case "${target_dir}" in
    /*) ;;
    *) target_dir="${ROOT}/${target_dir}" ;;
esac

if [[ "${max_bytes}" -eq 0 ]]; then
    printf 'target pruning disabled by CONDUIT_TARGET_MAX_BYTES=0\n'
    exit 0
fi

debug_dir="${target_dir}/debug"
if [[ ! -d "${debug_dir}" ]]; then
    printf '%s does not exist; nothing to prune\n' "${debug_dir}"
    exit 0
fi

# `du -sk` is available on macOS and Linux. Its 1 KiB blocks avoid the
# incompatible `du -b`/`du -A` split between the two, because naturally this had
# to be mildly annoying too.
debug_kib=$(du -sk "${debug_dir}" | awk '{print $1}')
debug_bytes=$((debug_kib * 1024))

if [[ "${debug_bytes}" -le "${max_bytes}" ]]; then
    printf '%s is within budget (%s <= %s bytes)\n' "${debug_dir}" "${debug_bytes}" "${max_bytes}"
    exit 0
fi

printf '%s is over budget (%s > %s bytes); cleaning Cargo dev profile\n' \
    "${debug_dir}" "${debug_bytes}" "${max_bytes}"
"${CARGO:-cargo}" clean --profile dev --target-dir "${target_dir}"
