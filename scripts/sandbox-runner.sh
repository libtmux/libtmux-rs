#!/bin/sh
# Cargo target runner (see .cargo/config.toml): runs one test, example or bench
# binary with no inherited tmux server and a private default socket directory,
# then stops any server the binary left there.
#
#   sandbox-runner.sh <binary> [args...]
#
# Exit status: the binary's own; 78 when the run is refused before the binary
# starts; 70 when the binary passed but left a server, or the directory could
# not be checked, or a server started outside the sandbox; 130 when interrupted
# and the binary passed. Messages name variables and sockets, never values.
set -u

refuse() {
    printf 'sandbox-runner: %s\n' "$*" >&2
    exit 78
}

[ "$#" -ge 1 ] || refuse "no binary to run"

helper=$(dirname "$0")/sandbox-outside.sh
[ -r "$helper" ] || refuse "scripts/sandbox-outside.sh is missing"
# shellcheck source=sandbox-outside.sh
. "$helper"
# Where tmux would have put a bare client's server had the sandbox not: under
# the TMUX_TMPDIR received, which under Cargo is its `[env]` fallback, and under
# /tmp, or the directory named for tests. Only the first is skipped there.
caller_tmpdir=${TMUX_TMPDIR-}
case $caller_tmpdir in
*/.cargo/tmux-sandbox) caller_tmpdir= ;;
esac
fallback_named=${LIBTMUX_SANDBOX_FALLBACK_ROOT:+1}
fallback_root=${LIBTMUX_SANDBOX_FALLBACK_ROOT:-/tmp}

# LIBTMUX_SANDBOX_TMUX is the one way back to a server, and it names it. The
# TMUX received is never the source, and TMUX_PANE stays unset.
named_tmux=${LIBTMUX_SANDBOX_TMUX-}
unset TMUX TMUX_PANE LIBTMUX_SOCKET LIBTMUX_SOCKET_PATH LIBTMUX_SANDBOX_TMUX \
    LIBTMUX_SANDBOX_FALLBACK_ROOT

# tmux appends tmux-<uid>/default, and sun_path holds 104 bytes on macOS and
# 108 on Linux.
limit=100
uid=$(id -u)
parent=${TMPDIR:-/tmp}
parent=${parent%/}
case $parent in
/*) ;;
*) refuse "TMPDIR is not an absolute path" ;;
esac
[ -d "$parent" ] || refuse "TMPDIR is not a directory"
# The path tmux binds is the resolved one: /var is /private/var on macOS.
resolved=$(cd "$parent" 2>/dev/null && pwd -P) || refuse "TMPDIR cannot be resolved"
need=$(($(printf '%s' "$resolved/ltrs.XXXXXX/tmux-$uid/default" | wc -c)))
[ "$need" -le "$limit" ] ||
    refuse "TMPDIR is too long for a tmux socket path; the limit is $limit bytes"
sandbox=$(mktemp -d "$parent/ltrs.XXXXXX" 2>/dev/null) ||
    refuse "cannot create a private TMUX_TMPDIR under TMPDIR"
chmod 700 "$sandbox" 2>/dev/null || {
    rm -rf "${sandbox:?}"
    refuse "cannot make the private TMUX_TMPDIR owner-only"
}
export TMUX_TMPDIR="$sandbox" LIBTMUX_TEST_SANDBOX="$sandbox"

if [ -n "$named_tmux" ]; then
    TMUX=$named_tmux
    export TMUX
    printf 'sandbox-runner: LIBTMUX_SANDBOX_TMUX is set: true; %s\n' \
        'TMUX comes from it and TMUX_PANE is unset' >&2
fi
unset named_tmux

if [ -n "$fallback_named" ]; then
    printf 'sandbox-runner: LIBTMUX_SANDBOX_FALLBACK_ROOT is set: true; %s\n' \
        'it names the fallback directory to watch in place of /tmp' >&2
fi

# Anything newer than this, outside the sandbox, was made after this point.
reference=$sandbox/.reference
outside_reference "$reference" || {
    rm -rf "${sandbox:?}"
    refuse "cannot write under the private TMUX_TMPDIR"
}

# Held until the binary exits, so an interrupt still reaches the sweep below.
trap 'interrupted=1' INT TERM HUP
"$@"
status=$?
trap - INT TERM HUP

outside_failed=
outside=$(outside_started "$reference" "$fallback_root" "$caller_tmpdir") || outside_failed=1

tmux_bin=${LIBTMUX_TEST_TMUX:-tmux}
command -v "$tmux_bin" >/dev/null 2>&1 || tmux_bin=tmux

# Every socket under the sandbox, links not followed: a test can start a server
# with -S anywhere in it. A socket file nothing answers on is not a server, and
# a protocol mismatch still means one listens. Prints "left" and "stuck" paths.
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
blind=
if command -v "$tmux_bin" >/dev/null 2>&1; then
    swept=$(sweep 2>/dev/null) || unscanned=1
    left=$(printf '%s\n' "$swept" | sed -n 's/^left / /p' | tr '\n' ',')
    stuck=$(printf '%s\n' "$swept" | sed -n 's/^stuck / /p' | tr '\n' ',')
elif [ -n "$(find "$sandbox" -type s 2>/dev/null | head -n 1)" ]; then
    # A socket nothing can be asked about may have a server behind it.
    blind=1
fi

# Nothing owns a directory that still holds a server, so it stays, and so does
# one that could not be searched or asked about.
leftover=
if [ -z "$stuck" ] && [ -z "$unscanned" ] && [ -z "$blind" ]; then
    case $sandbox in
    "$parent"/ltrs.??????) rm -rf "${sandbox:?}" 2>/dev/null ;;
    esac
    [ ! -e "$sandbox" ] || leftover=1
fi

# Read, never touched: nothing says the server is this run's to stop.
if [ -n "$outside_failed" ]; then
    printf 'sandbox-runner: could not check for servers outside the sandbox\n' >&2
    [ "$status" -ne 0 ] || status=70
elif [ -n "$outside" ]; then
    where=
    while read -r root; do
        if [ "$root" = "$fallback_root" ]; then
            where="$where the tmux fallback directory,"
        else
            where="$where the caller's TMUX_TMPDIR,"
        fi
    done <<OUTSIDE
$outside
OUTSIDE
    printf 'sandbox-runner: a tmux server started outside the sandbox, in%s; not touched\n' "${where%,}" >&2
    [ "$status" -ne 0 ] || status=70
fi

if [ -n "$unscanned" ]; then
    printf 'sandbox-runner: could not search the sandbox for sockets; the directory is kept\n' >&2
    [ "$status" -eq 0 ] && exit 70
elif [ -n "$blind" ]; then
    printf 'sandbox-runner: no tmux to ask about the sandbox sockets; the directory is kept\n' >&2
    [ "$status" -eq 0 ] && exit 70
elif [ -n "$stuck" ]; then
    printf 'sandbox-runner: could not stop the tmux server on socket(s):%s; the directory is kept\n' "${stuck%,}" >&2
    [ "$status" -eq 0 ] && exit 70
elif [ -n "$leftover" ]; then
    printf 'sandbox-runner: could not remove the sandbox directory; it is kept\n' >&2
    [ "$status" -eq 0 ] && exit 70
elif [ -n "$left" ]; then
    printf 'sandbox-runner: the binary left a tmux server on socket(s):%s; stopped\n' "${left%,}" >&2
    [ "$status" -eq 0 ] && exit 70
fi
[ -z "${interrupted-}" ] || [ "$status" -ne 0 ] || status=130
exit "$status"
