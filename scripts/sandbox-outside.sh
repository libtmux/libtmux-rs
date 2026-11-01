# shellcheck shell=sh
# Sourced by the sandbox runner and the doctest check. Looks at the default
# sockets a bare tmux client would reach outside the sandbox; never connects to
# one, signals a server or removes a file.

# outside_reference <file>: make the reference, and wait until the clock is past it.
outside_reference() {
    touch "$1" 2>/dev/null || return 1
    outside_tries=0
    while [ "$outside_tries" -lt 100 ]; do
        touch "$1.tick" 2>/dev/null || return 1
        [ -z "$(find "$1.tick" -newer "$1" 2>/dev/null)" ] || return 0
        outside_tries=$((outside_tries + 1))
        sleep 0.01 2>/dev/null || sleep 1
    done
    return 1
}

# outside_started <reference> <root>...: print each root whose default socket
# is newer than the reference, whoever made it. Returns 2 without a reference.
outside_started() {
    [ -e "$1" ] || return 2
    outside_reference=$1
    shift
    outside_uid=$(id -u)
    for outside_root in "$@"; do
        [ -n "$outside_root" ] || continue
        outside_socket=${outside_root%/}/tmux-$outside_uid/default
        [ -S "$outside_socket" ] || continue
        if [ -n "$(find "$outside_socket" -newer "$outside_reference" 2>/dev/null)" ]; then
            printf '%s\n' "$outside_root"
        fi
    done
}
