#!/usr/bin/env bash
# Runs the API server, Conduit Vox, and the Operator Console together for local
# development.
#
# Two processes is the honest shape of the stack, but starting them by hand
# means remembering which port the Vite proxy expects and which authentication
# mode the server refuses to start without. This is that pair, started once,
# and stopped together: killing the script kills both, so there is no orphaned
# server holding 8080 the next time.
#
# The default is the mode a laptop wants — an open API, and real providers from
# whatever Provider Definitions are saved — because that is the loop worth
# making frictionless. Everything else is a flag.
#
#   scripts/dev.sh                          # anonymous API, Vox, real providers
#   scripts/dev.sh --tokens secrets/tokens.json
#   scripts/dev.sh --echo                   # no speech engine or model server
#   scripts/dev.sh --api-port 8081 --ui-port 5174
#
# Nothing here reads .env: compose does that, and a shell that exported
# CONDUIT_TOKENS while this script asked for anonymous would make the server
# refuse to start on a contradiction it did not choose. Authentication is set
# explicitly below; every other CONDUIT_* variable in your environment passes
# through untouched.
set -euo pipefail

readonly SELF="${0##*/}"

# Where the pieces live, so the script works from any directory.
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
readonly ROOT

# Defaults. `8080` and `5173` are what the Vite proxy and Vite itself already
# assume, so overriding one port must not mean overriding both by hand.
api_port=8080
ops_port=9090
vox_port=8091
ui_port=5173
# Empty means anonymous; a path means authenticate against that token file.
tokens=""
# Whether to compile the in-memory echo providers in.
echo_providers=0
# Print the resolved configuration and exit, which is also what the tests read.
dry_run=0

usage() {
    cat <<USAGE
${SELF} — run the Conduit API, Vox, and Operator Console together

Usage: scripts/dev.sh [options]

Options:
  --anonymous          Serve the API with no authentication (default).
  --tokens FILE        Authenticate against FILE instead of serving openly.
  --echo               Build with the echo providers, which transcribe audio as
                       UTF-8 text, so a pipeline can hold a conversation with no
                       speech engine or model server. Off by default: real
                       providers come from saved Provider Definitions.
  --api-port PORT      Service API port (default ${api_port}).
  --ops-port PORT      Ops API port for /health, /ready, /metrics (default ${ops_port}).
  --vox-port PORT      Conduit Vox port (default ${vox_port}).
  --ui-port PORT       Operator Console port (default ${ui_port}).
  --dry-run            Print what would run, start nothing.
  -h, --help           Show this help.

All three processes bind loopback only. Ctrl-C stops the trio.
USAGE
}

die() {
    printf '%s: %s\n' "${SELF}" "$1" >&2
    exit 2
}

# Rejects anything that is not a port, because a typo that reached `cargo` would
# surface as a parse error from a SocketAddr rather than as the flag it was.
require_port() {
    local flag="$1" value="$2"
    case "${value}" in
        '' | *[!0-9]*) die "${flag} needs a port number, got '${value}'" ;;
    esac
    if [[ "${value}" -lt 1 || "${value}" -gt 65535 ]]; then
        die "${flag} needs a port between 1 and 65535, got '${value}'"
    fi
}

# Every option that takes a value must be given one; `--api-port --echo` would
# otherwise silently consume the next flag.
require_value() {
    if [[ $# -lt 2 ]]; then
        die "$1 needs a value"
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --anonymous)
            tokens=""
            shift
            ;;
        --tokens)
            require_value "$@"
            tokens="$2"
            shift 2
            ;;
        --echo | --dev-providers)
            echo_providers=1
            shift
            ;;
        --api-port)
            require_value "$@"
            require_port --api-port "$2"
            api_port="$2"
            shift 2
            ;;
        --ops-port)
            require_value "$@"
            require_port --ops-port "$2"
            ops_port="$2"
            shift 2
            ;;
        --ui-port)
            require_value "$@"
            require_port --ui-port "$2"
            ui_port="$2"
            shift 2
            ;;
        --vox-port)
            require_value "$@"
            require_port --vox-port "$2"
            vox_port="$2"
            shift 2
            ;;
        --dry-run)
            dry_run=1
            shift
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

# Distinct ports, or one listener wins and the other dies on bind with an error
# that names an address rather than the flag that collided.
if [[ "${api_port}" == "${ops_port}" || "${api_port}" == "${vox_port}" || "${api_port}" == "${ui_port}" || "${ops_port}" == "${vox_port}" || "${ops_port}" == "${ui_port}" || "${vox_port}" == "${ui_port}" ]]; then
    die "--api-port, --ops-port, --vox-port, and --ui-port must differ (got ${api_port}, ${ops_port}, ${vox_port}, ${ui_port})"
fi

# Checked here rather than left to the server: a missing token file after a
# three-minute compile is a worse way to learn about a typo.
if [[ -n "${tokens}" && ! -f "${tokens}" ]]; then
    die "token file '${tokens}' does not exist"
fi

# The server treats an empty value as unset for both, so each mode can state
# both variables and never present it with the contradiction of two.
if [[ -n "${tokens}" ]]; then
    auth_summary="token file ${tokens}"
    export CONDUIT_TOKENS="${tokens}"
    export CONDUIT_ALLOW_ANONYMOUS=""
else
    auth_summary="anonymous (no authentication)"
    export CONDUIT_TOKENS=""
    export CONDUIT_ALLOW_ANONYMOUS=1
fi

# Carries the package as well as the features so the array is never empty:
# under `set -u`, bash 3.2 — which is the bash macOS ships — treats expanding an
# empty array as an unbound variable and aborts.
cargo_args=(-p conduit-api)
if [[ "${echo_providers}" -eq 1 ]]; then
    provider_summary="echo providers (audio is treated as UTF-8 text)"
    cargo_args+=(--features dev-providers)
else
    provider_summary="real providers from saved Provider Definitions"
fi

# Loopback, not the server's own `0.0.0.0` default: a development server should
# not be reachable from the network because someone started a script.
export CONDUIT_BIND="127.0.0.1:${api_port}"
export CONDUIT_OPS_BIND="127.0.0.1:${ops_port}"
# What the Vite proxy forwards /v1 to, so the browser bundle needs no API origin.
export VITE_CONDUIT_API_TARGET="http://127.0.0.1:${api_port}"
# What the Vite proxy forwards /vox to during local development. The production
# bundle still talks to Conduit at the same path; only Vite learns the direct
# upstream so the embedded UI works before a link exists.
export VITE_CONDUIT_VOX_TARGET="http://127.0.0.1:${vox_port}"
vox_dev_root="${ROOT}/output/dev/vox"
readonly vox_dev_root
# Vox defaults to container paths. On a host shell those are a great way to
# find out which directories do not exist, or worse, do and are unwritable.
export SPEAKER_ID_DATA_DIR="${SPEAKER_ID_DATA_DIR:-${vox_dev_root}/data}"
export SPEAKER_ID_MODEL_DIR="${SPEAKER_ID_MODEL_DIR:-${vox_dev_root}/models}"

cat <<SUMMARY
conduit dev
  api            http://127.0.0.1:${api_port}
  ops            http://127.0.0.1:${ops_port}
  vox            http://127.0.0.1:${vox_port}
  console        http://127.0.0.1:${ui_port}
  access         ${auth_summary}
  providers      ${provider_summary}
SUMMARY

# The resolved environment, not a prose summary of it: what the server reads is
# the thing worth showing, and it is what the tests assert on.
if [[ "${dry_run}" -eq 1 ]]; then
    cat <<RESOLVED
  CONDUIT_BIND=${CONDUIT_BIND}
  CONDUIT_OPS_BIND=${CONDUIT_OPS_BIND}
  CONDUIT_TOKENS=${CONDUIT_TOKENS}
  CONDUIT_ALLOW_ANONYMOUS=${CONDUIT_ALLOW_ANONYMOUS}
  VITE_CONDUIT_API_TARGET=${VITE_CONDUIT_API_TARGET}
  VITE_CONDUIT_VOX_TARGET=${VITE_CONDUIT_VOX_TARGET}
  SPEAKER_ID_DATA_DIR=${SPEAKER_ID_DATA_DIR}
  SPEAKER_ID_MODEL_DIR=${SPEAKER_ID_MODEL_DIR}
  cargo run ${cargo_args[*]}
  .venv/bin/python -m uvicorn app:app --host 127.0.0.1 --port ${vox_port}
  npm run dev -- --port ${ui_port} --strictPort --host 127.0.0.1
RESOLVED
    exit 0
fi

if ! command -v cargo >/dev/null 2>&1; then
    die "cargo is not on PATH"
fi
if ! command -v npm >/dev/null 2>&1; then
    die "npm is not on PATH"
fi
if ! command -v python3 >/dev/null 2>&1; then
    die "python3 is not on PATH"
fi

# Checked before the compile for the same reason the token file is: a bare
# `AddrInUse` after a three-minute build names an address rather than what is
# already holding it, and on a developer machine the answer is usually a tunnel
# or a previous run. Skipped when `lsof` is missing rather than treated as free.
if command -v lsof >/dev/null 2>&1; then
    for port_pair in "api:${api_port}" "ops:${ops_port}" "vox:${vox_port}" "console:${ui_port}"; do
        label="${port_pair%%:*}"
        port="${port_pair##*:}"
        if holder=$(lsof -nP -sTCP:LISTEN -iTCP:"${port}" 2>/dev/null | awk 'NR == 2 {print $1 " (pid " $2 ")"}') \
            && [[ -n "${holder}" ]]; then
            die "the ${label} port ${port} is already in use by ${holder}; stop it or pass --${label/console/ui}-port"
        fi
    done
fi

# Frontend dependencies before anything long-running: `vite: not found` after
# the backend is already listening reads as a broken script rather than a
# missing `npm ci`.
if [[ ! -d "${ROOT}/frontend/node_modules" ]]; then
    printf '\ninstalling frontend dependencies\n'
    (cd "${ROOT}/frontend" && npm ci)
fi

vox_dir="${ROOT}/services/vox"
readonly vox_dir
vox_venv="${vox_dir}/.venv"
readonly vox_venv
vox_python="${vox_venv}/bin/python"
readonly vox_python

# Vox's UI and health routes only need the base requirements, so the default
# development script installs those rather than every engine's model stack.
if [[ ! -x "${vox_python}" ]]; then
    printf '\ncreating the Vox virtualenv\n'
    (cd "${vox_dir}" && python3 -m venv .venv)
fi
if ! "${vox_python}" -c "import fastapi, httpx, numpy, soundfile, uvicorn" >/dev/null 2>&1; then
    printf '\ninstalling Vox dependencies\n'
    (cd "${vox_dir}" && "${vox_venv}/bin/pip" install -r requirements.txt)
fi

mkdir -p "${SPEAKER_ID_DATA_DIR}" "${SPEAKER_ID_MODEL_DIR}"

# Compiled before either process starts, so a compile error is a compile error
# and not a console proxying to a port nothing ever opened.
printf '\nbuilding conduit-api\n'
(cd "${ROOT}" && cargo build "${cargo_args[@]}")

api_pid=""
vox_pid=""
ui_pid=""

# `cargo run` and `npm run dev` are wrappers: the processes that actually hold
# the ports are their children. Signalling only the wrapper reaps the wrapper and
# orphans the listener, so the next run dies on a port already in use.
#
# Children before parents, so nothing is left reparented to init and still
# listening after its wrapper is gone.
# shellcheck disable=SC2329  # called from `stop`, via a command substitution.
descendants() {
    local pid="$1" child
    for child in $(pgrep -P "${pid}" 2>/dev/null); do
        descendants "${child}"
    done
    printf '%s\n' "${pid}"
}

# One exit path for every way this ends — Ctrl-C, a child dying, a failed
# start — because a half-stopped pair leaves a listener holding the port.
# shellcheck disable=SC2329  # invoked by the trap below.
stop() {
    trap - EXIT INT TERM
    local pid victim
    for pid in "${ui_pid}" "${vox_pid}" "${api_pid}"; do
        [[ -n "${pid}" ]] || continue
        for victim in $(descendants "${pid}"); do
            kill "${victim}" 2>/dev/null || true
        done
        wait "${pid}" 2>/dev/null || true
    done
}
trap stop EXIT INT TERM

printf '\nstarting conduit-api\n'
(cd "${ROOT}" && exec cargo run "${cargo_args[@]}") &
api_pid=$!

printf 'starting Conduit Vox\n'
(cd "${vox_dir}" && exec "${vox_python}" -m uvicorn app:app --host 127.0.0.1 --port "${vox_port}") &
vox_pid=$!

# `--host 127.0.0.1` because Vite otherwise resolves `localhost` to IPv6 only on
# macOS, and the console would refuse the loopback address this script prints.
printf 'starting the operator console\n\n'
(cd "${ROOT}/frontend" && exec npm run dev -- \
    --port "${ui_port}" --strictPort --host 127.0.0.1) &
ui_pid=$!

# Polled rather than `wait -n`, which needs bash 4.3 and so is absent from the
# bash macOS ships. Either process exiting takes the other down: a console
# proxying to a dead server is a worse debugging experience than a clean stop.
while kill -0 "${api_pid}" 2>/dev/null && kill -0 "${vox_pid}" 2>/dev/null && kill -0 "${ui_pid}" 2>/dev/null; do
    sleep 1
done

if ! kill -0 "${api_pid}" 2>/dev/null; then
    printf '\n%s: conduit-api exited; stopping Vox and the operator console\n' "${SELF}" >&2
elif ! kill -0 "${vox_pid}" 2>/dev/null; then
    printf '\n%s: Conduit Vox exited; stopping conduit-api and the operator console\n' "${SELF}" >&2
else
    printf '\n%s: the operator console exited; stopping conduit-api and Vox\n' "${SELF}" >&2
fi
exit 1
