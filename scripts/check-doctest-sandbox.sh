#!/usr/bin/env bash
# Run the doctest canary through the runner, through `[env]` alone, and with the
# sandbox broken one way at a time; each broken run must fail, name what is
# wrong and print no value. The ambient environment names a server throughout.

set -euo pipefail

cd "$(dirname "$0")/.."

canary=(cargo test --locked --package libtmux --doc SandboxCanary)

# The host triple's runner outranks the `cfg(unix)` one in the config file.
# Overriding the `cfg(unix)` entry itself does not always replace it.
triple=$(rustc -vV | sed -n 's/^host: //p')
swap=(--config "target.\"$triple\".runner=['env']")

# shellcheck source=sandbox-outside.sh
. scripts/sandbox-outside.sh

log=
elsewhere=
cleanup() {
    status=$?
    if [ -n "$elsewhere" ]; then
        tmux -S "$elsewhere/server.sock" kill-server >/dev/null 2>&1 || true
        tmux -S "$elsewhere/tmpdir/tmux-$(id -u)/default" kill-server >/dev/null 2>&1 || true
        # Read, never touched: only the servers this script started are stopped.
        outside=$(outside_started "$elsewhere/reference" /tmp "${TMUX_TMPDIR-}") || {
            printf 'could not check for servers outside the sandbox\n' >&2
            status=70
        }
        if [ -n "$outside" ]; then
            printf 'a tmux server started outside the sandbox during this check; not touched\n' >&2
            status=70
        fi
        rm -rf "${elsewhere:?}"
    fi
    [ -z "$log" ] || rm -f "${log:?}"
    exit "$status"
}
trap cleanup EXIT

log=$(mktemp)
elsewhere=$(mktemp -d)
# A TMUX_TMPDIR that exists but is not the sandbox. One that does not exist
# would send a bare client to /tmp/tmux-<uid>, the developer's own sockets.
mkdir "$elsewhere/tmpdir"
outside_reference "$elsewhere/reference" || {
    printf 'cannot write the reference file for the outside check\n' >&2
    exit 1
}

# A tmux whose servers listen on a socket of its own, so a bare client does
# not land on the default socket.
cat >"$elsewhere/tmux" <<WRAPPER
#!/bin/sh
exec tmux -S "$elsewhere/server.sock" "\$@"
WRAPPER
chmod +x "$elsewhere/tmux"

ambient=(
    "TMUX=/nonexistent/decoy.sock,1,0" TMUX_PANE=%9 LIBTMUX_SOCKET=decoy-name
    LIBTMUX_SOCKET_PATH=/nonexistent/decoy-path.sock
)
env "${ambient[@]}" LIBTMUX_DOCTEST_EXPECTS=runner \
    scripts/require-tests.sh "${canary[@]}"
env "${ambient[@]}" LIBTMUX_DOCTEST_EXPECTS="env" \
    scripts/require-tests.sh "${canary[@]}" "${swap[@]}"

runner_args=("${swap[@]}")

# expect_failure <text the failure must contain> <label> [VAR=value ...] -- <cargo args>
expect_failure() {
    local expected=$1 label=$2
    shift 2
    local faults=()
    while [ "$1" != -- ]; do
        faults+=("$1")
        shift
    done
    shift
    if env ${faults[@]+"${faults[@]}"} "${canary[@]}" \
        ${runner_args[@]+"${runner_args[@]}"} "$@" >"$log" 2>&1; then
        printf 'the doctest canary passed with %s\n' "$label" >&2
        exit 1
    fi
    if ! grep -qF "$expected" "$log" ||
        ! grep -q '^test result: FAILED. 0 passed; 1 failed' "$log"; then
        printf 'the doctest canary did not fail on %s:\n' "$label" >&2
        cat "$log" >&2
        exit 1
    fi
    if grep -q 'decoy' "$log"; then
        printf 'the doctest canary printed the value it was given for %s\n' "$label" >&2
        exit 1
    fi
}

for variable in TMUX TMUX_PANE LIBTMUX_SOCKET LIBTMUX_SOCKET_PATH; do
    expect_failure "$variable is set" "$variable naming a server" -- \
        --config "env.$variable.value=\"/nonexistent/decoy.sock,1,0\""
done
expect_failure 'TMUX_TMPDIR is not LIBTMUX_TEST_SANDBOX' \
    'a TMUX_TMPDIR that is not the sandbox' -- \
    --config "env.TMUX_TMPDIR.value=\"$elsewhere/tmpdir\"" \
    --config 'env.TMUX_TMPDIR.relative=false'
expect_failure "missed the sandbox's default socket" \
    'a tmux that lands elsewhere' "LIBTMUX_TEST_TMUX=$elsewhere/tmux" --
expect_failure 'the runner did not reach this doctest' \
    'a doctest the runner did not reach' LIBTMUX_DOCTEST_EXPECTS=runner --
runner_args=()
expect_failure 'and not the runner should have reached this doctest' \
    'a doctest the runner did reach, where [env] was expected' \
    LIBTMUX_DOCTEST_EXPECTS="env" --

printf 'the doctest canary passes in the sandbox and fails when it is broken\n'
