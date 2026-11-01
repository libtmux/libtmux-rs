#!/bin/sh
# Cargo target runner (see .cargo/config.toml): runs one test, example or bench
# binary with no inherited tmux server and a private default socket directory,
# then stops any server the binary left there.
#
#   sandbox-runner.sh <binary> [args...]
#
# TMUX, TMUX_PANE, LIBTMUX_SOCKET and LIBTMUX_SOCKET_PATH are removed. The one
# way back to a server is LIBTMUX_SANDBOX_TMUX: when it is set and not empty,
# the binary gets its value as TMUX, so a caller names the server it wants
# instead of letting an inherited TMUX through. TMUX_PANE stays removed.
#
# Exit status: the binary's own; 78 when the run is refused before the binary
# starts; 70 when the binary passed but left a server running. Messages name
# variables and sockets, never values.
set -u

refuse() {
    printf 'sandbox-runner: %s\n' "$*" >&2
    exit 78
}

[ "$#" -ge 1 ] || refuse "no binary to run"

# The value is read before TMUX is removed and unset before the binary starts,
# so nothing the binary runs sees it. The ambient TMUX is never the source.
named_tmux=${LIBTMUX_SANDBOX_TMUX-}
unset TMUX TMUX_PANE LIBTMUX_SOCKET LIBTMUX_SOCKET_PATH LIBTMUX_SANDBOX_TMUX
if [ -n "$named_tmux" ]; then
    TMUX=$named_tmux
    export TMUX
    printf 'sandbox-runner: LIBTMUX_SANDBOX_TMUX is set: true; %s\n' \
        'TMUX comes from it and TMUX_PANE is unset' >&2
fi
unset named_tmux

# tmux appends tmux-<uid>/default, and sun_path holds 104 bytes on macOS and
# 108 on Linux.
limit=100
uid=$(id -u)
parent=${TMPDIR:-/tmp}
parent=${parent%/}
[ -d "$parent" ] || refuse "TMPDIR is not a directory"
need=$(($(printf '%s' "$parent/ltrs.XXXXXX/tmux-$uid/default" | wc -c)))
[ "$need" -le "$limit" ] ||
    refuse "TMPDIR is too long for a tmux socket path; the limit is $limit bytes"
sandbox=$(mktemp -d "$parent/ltrs.XXXXXX") ||
    refuse "cannot create a private TMUX_TMPDIR under TMPDIR"
chmod 700 "$sandbox"
export TMUX_TMPDIR="$sandbox" LIBTMUX_TEST_SANDBOX="$sandbox"

# Held until the binary exits, so an interrupt still reaches the sweep below.
trap 'interrupted=1' INT TERM HUP
"$@"
status=$?
trap - INT TERM HUP

tmux_bin=${LIBTMUX_TEST_TMUX:-tmux}
command -v "$tmux_bin" >/dev/null 2>&1 || tmux_bin=tmux

# Every socket anywhere under the sandbox, links not followed, since a test can
# start a server with -S on a path of its own. A socket file nothing answers on
# is not a server: tmux never unlinks it. A mismatched release answers with a
# protocol error, which still means something is listening. Prints "left <path>"
# for each server found and "stuck <path>" for each that survives kill-server.
sweep() {
    find "$sandbox" -type s -exec sh -c '
        tmux_bin=$1 sandbox=$2
        shift 2
        answers() {
            output=$("$tmux_bin" -N -S "$1" list-sessions 2>&1) && return 0
            case $output in *"protocol version mismatch"*) return 0 ;; esac
            return 1
        }
        for socket; do
            answers "$socket" || continue
            name=${socket#"$sandbox"/}
            printf "left %s\n" "$name"
            "$tmux_bin" -S "$socket" kill-server >/dev/null 2>&1
            tries=0
            while answers "$socket" && [ "$tries" -lt 30 ]; do
                sleep 0.1 2>/dev/null || sleep 1
                tries=$((tries + 1))
            done
            ! answers "$socket" || printf "stuck %s\n" "$name"
        done
        :' sh "$tmux_bin" "$sandbox" {} +
}

left=
stuck=
unscanned=
if command -v "$tmux_bin" >/dev/null 2>&1; then
    swept=$(sweep) || unscanned=1
    left=$(printf '%s\n' "$swept" | sed -n 's/^left / /p' | tr '\n' ',')
    stuck=$(printf '%s\n' "$swept" | sed -n 's/^stuck / /p' | tr '\n' ',')
fi

# Nothing owns a directory that still holds a server, so it stays, and so does
# one that could not be searched.
if [ -z "$stuck" ] && [ -z "$unscanned" ]; then
    case $sandbox in
    "$parent"/ltrs.??????) rm -rf "${sandbox:?}" ;;
    esac
fi

if [ -n "$unscanned" ]; then
    printf 'sandbox-runner: could not search the sandbox for sockets; the directory is kept\n' >&2
    [ "$status" -eq 0 ] && exit 70
elif [ -n "$stuck" ]; then
    printf 'sandbox-runner: could not stop the tmux server on socket(s):%s; the directory is kept\n' "${stuck%,}" >&2
    [ "$status" -eq 0 ] && exit 70
elif [ -n "$left" ]; then
    printf 'sandbox-runner: the binary left a tmux server on socket(s):%s; stopped\n' "${left%,}" >&2
    [ "$status" -eq 0 ] && exit 70
fi
[ -z "${interrupted-}" ] || [ "$status" -ne 0 ] || status=130
exit "$status"
