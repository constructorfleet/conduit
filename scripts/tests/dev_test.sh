#!/usr/bin/env bash
# Tests scripts/dev.sh.
#
# Every case runs the real script with --dry-run, so what is asserted is the
# environment the server and Vite would actually be handed rather than a
# restatement of the parsing. Nothing here starts a process or binds a port.
#
#   scripts/tests/dev_test.sh
set -uo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
readonly root
readonly script="${root}/scripts/dev.sh"

failures=0

# Runs dev.sh with a deliberately hostile environment: a shell that already
# exported the opposite authentication mode must not change what the script
# resolves, because that contradiction is what makes the server refuse to start.
run() {
    env CONDUIT_TOKENS=/leaked/from/the/shell CONDUIT_ALLOW_ANONYMOUS=1 \
        CONDUIT_BIND=0.0.0.0:1 CONDUIT_OPS_BIND=0.0.0.0:2 \
        "${script}" --dry-run "$@" 2>&1
}

pass() {
    printf 'ok   %s\n' "$1"
}

fail() {
    printf 'FAIL %s\n     %s\n' "$1" "$2" >&2
    failures=$((failures + 1))
}

# Asserts `dev.sh $args` succeeds and prints a line equal to `expected`, so a
# variable set to a longer value cannot satisfy a shorter pattern.
emits() {
    local name="$1" expected="$2"
    shift 2
    local output
    if ! output=$(run "$@"); then
        fail "${name}" "exited non-zero: ${output}"
        return
    fi
    if grep -Fqx -- "  ${expected}" <<<"${output}"; then
        pass "${name}"
    else
        fail "${name}" "no line '${expected}' in: ${output}"
    fi
}

# Asserts `dev.sh $args` is refused, and that the message names `reason` so the
# operator learns which flag was wrong rather than that something was.
refuses() {
    local name="$1" reason="$2"
    shift 2
    local output status
    output=$(run "$@")
    status=$?
    if [[ "${status}" -eq 0 ]]; then
        fail "${name}" "expected a refusal, got: ${output}"
        return
    fi
    if [[ "${output}" == *"${reason}"* ]]; then
        pass "${name}"
    else
        fail "${name}" "message did not mention '${reason}': ${output}"
    fi
}

# --- Defaults ----------------------------------------------------------------

# The whole point of the default: a laptop gets an open API without being asked
# to invent a token file first.
emits "the default serves the API anonymously" "CONDUIT_ALLOW_ANONYMOUS=1"
emits "the default sets no token file" "CONDUIT_TOKENS="
# Real providers are the default because echo providers cannot hear speech, and
# a development server that silently could not would be a confusing one.
emits "the default builds without the echo providers" "cargo run -p conduit-api"
emits "the default binds the API to loopback" "CONDUIT_BIND=127.0.0.1:8080"
emits "the default binds ops to loopback" "CONDUIT_OPS_BIND=127.0.0.1:9090"
emits "the default points the console proxy at the API" \
    "VITE_CONDUIT_API_TARGET=http://127.0.0.1:8080"
emits "the default points the Vox proxy at the local Vox service" \
    "VITE_CONDUIT_VOX_TARGET=http://127.0.0.1:8091"
emits "the default points the Memoria proxy at the local Memoria service" \
    "VITE_CONDUIT_MEMORIA_TARGET=http://127.0.0.1:8092"
emits "the default stores Vox prints in a writable local directory" \
    "SPEAKER_ID_DATA_DIR=${root}/output/dev/vox/data"
emits "the default stores Vox models in a writable local directory" \
    "SPEAKER_ID_MODEL_DIR=${root}/output/dev/vox/models"
emits "the default serves the console on Vite's port" \
    "npm run dev -- --port 5173 --strictPort --host 127.0.0.1"
emits "the default starts Vox on its own loopback port" \
    ".venv/bin/python3 -m uvicorn app:app --host 127.0.0.1 --port 8091"
emits "the default starts Memoria on its own loopback port" \
    ".venv/bin/python3 -m uvicorn app:app --host 127.0.0.1 --port 8092"

# --- Authentication ----------------------------------------------------------

emits "--anonymous is the default stated explicitly" "CONDUIT_ALLOW_ANONYMOUS=1" --anonymous

tokens=$(mktemp)
readonly tokens
trap 'rm -f "${tokens}"' EXIT
printf '{}\n' >"${tokens}"

emits "--tokens authenticates against the file" "CONDUIT_TOKENS=${tokens}" --tokens "${tokens}"
# Both variables set is an error the server refuses to start on, so the script
# must clear the one it did not choose even when the shell exported it.
emits "--tokens clears anonymous mode" "CONDUIT_ALLOW_ANONYMOUS=" --tokens "${tokens}"
emits "--anonymous clears an inherited token file" "CONDUIT_TOKENS=" \
    --tokens "${tokens}" --anonymous
emits "the last authentication flag wins" "CONDUIT_TOKENS=${tokens}" \
    --anonymous --tokens "${tokens}"
# Caught before the compile, because a typo found three minutes later is a typo
# found too late.
refuses "a missing token file is refused" "does not exist" --tokens /no/such/tokens.json

# --- Providers ---------------------------------------------------------------

emits "--echo builds the echo providers in" \
    "cargo run -p conduit-api --features dev-providers" --echo
emits "--dev-providers is a synonym for --echo" \
    "cargo run -p conduit-api --features dev-providers" --dev-providers

# --- Ports -------------------------------------------------------------------

emits "--api-port moves the service listener" "CONDUIT_BIND=127.0.0.1:8081" --api-port 8081
# The reason the console proxy target is derived rather than a separate flag:
# moving the API and forgetting the proxy would leave the UI talking to nothing.
emits "--api-port follows through to the console proxy" \
    "VITE_CONDUIT_API_TARGET=http://127.0.0.1:8081" --api-port 8081
emits "--ops-port moves the ops listener" "CONDUIT_OPS_BIND=127.0.0.1:9191" --ops-port 9191
emits "--ui-port moves the console" "npm run dev -- --port 5174 --strictPort --host 127.0.0.1" --ui-port 5174
emits "--vox-port moves the Vox listener" \
    "VITE_CONDUIT_VOX_TARGET=http://127.0.0.1:8191" --vox-port 8191
emits "--memoria-port moves the Memoria listener" \
    "VITE_CONDUIT_MEMORIA_TARGET=http://127.0.0.1:8291" --memoria-port 8291

refuses "a non-numeric port is refused" "--api-port" --api-port http://8080
refuses "port zero is refused" "--ui-port" --ui-port 0
refuses "a port above the range is refused" "--ops-port" --ops-port 70000
# Otherwise one listener wins the bind and the other dies naming an address
# rather than the flags that collided.
refuses "colliding ports are refused" "must differ" --api-port 9090
refuses "vox port collisions are refused" "must differ" --vox-port 8080
refuses "a flag consumed as a value is refused" "needs a value" --api-port

# --- Ports already in use ----------------------------------------------------

# Something has to actually hold a port for the check to have anything to find.
# It writes the port to a file rather than to stdout, because a helper that
# inherits the pipe of a command substitution keeps it open, and the substitution
# would then block until the helper exited rather than until it had bound.
readonly port_file="${TMPDIR:-/tmp}/conduit-dev-test-port.$$"
# Port 0 asks the kernel for a free one, so this cannot flake against a machine
# where whatever fixed number was hardcoded happened to be taken.
python3 -c '
import socket, sys, time
listener = socket.socket()
listener.bind(("127.0.0.1", 0))
listener.listen(1)
open(sys.argv[1], "w").write(str(listener.getsockname()[1]))
time.sleep(120)
' "${port_file}" &
occupier=$!
trap 'kill "${occupier}" 2>/dev/null; rm -f "${tokens}" "${port_file}"' EXIT

occupied=""
for _ in $(seq 1 50); do
    if [[ -s "${port_file}" ]]; then
        occupied=$(cat "${port_file}")
        break
    fi
    sleep 0.1
done

if [[ -n "${occupied}" ]]; then
    # `--dry-run` resolves configuration and starts nothing, so the occupancy
    # check must not run there: printing what would happen collides with nothing.
    emits "--dry-run ignores an occupied port" "CONDUIT_BIND=127.0.0.1:${occupied}" \
        --api-port "${occupied}"
    # Without --dry-run the same port is refused, and the message says what holds
    # it: on a developer machine that is usually a tunnel or a previous run.
    output=$(env CONDUIT_TOKENS="" CONDUIT_ALLOW_ANONYMOUS=1 \
        "${script}" --api-port "${occupied}" 2>&1)
    if [[ "${output}" == *"already in use"* ]]; then
        pass "an occupied port is refused before the build"
    else
        fail "an occupied port is refused before the build" "message was: ${output}"
    fi
else
    fail "an occupied port is refused before the build" "could not reserve a port to occupy"
fi

# --- Usage -------------------------------------------------------------------

refuses "an unknown option is refused" "unknown option" --strict
if "${script}" --help | grep -Fq -- "--echo"; then
    pass "--help documents the flags"
else
    fail "--help documents the flags" "--echo is not in the help output"
fi

if [[ "${failures}" -gt 0 ]]; then
    printf '\n%d failing\n' "${failures}" >&2
    exit 1
fi
printf '\nall dev.sh tests passed\n'
