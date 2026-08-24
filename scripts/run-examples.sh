#!/usr/bin/env bash

# Run every shipped example to completion and prove it left nothing behind.
#
# `cargo test --all-targets` compiles an example and never runs it, so an
# example that fails at runtime passes every other gate. This runs each one
# against a server this script owns rather than whatever the reader happens to
# have, and fails when one exits nonzero or leaves a socket in the shared root.

set -euo pipefail

readonly dev_root=/tmp/libtmux-rs-dev
readonly manifest="crates/libtmux/Cargo.toml"

mkdir -p "$dev_root"
run_dir=$(mktemp -d "$dev_root/examples.XXXXXXXX")
readonly run_dir
readonly socket="$run_dir/server.sock"

# Who to ask about, later. A run still going owns its directory; one whose
# owner is gone left it behind. `just fixture-root` decides the same way.
printf '%s\n' "$$" > "$run_dir/owner"

cleanup() {
    tmux -S "$socket" kill-server >/dev/null 2>&1 || true
    rm -rf "$run_dir"
}
trap cleanup EXIT

# `inspect` and `find` read `$TMUX`, which is how a process inside a pane finds
# the server it belongs to. Pointing that at a socket this script owns is what
# keeps them off the reader's own server while still exercising the path they
# document.
tmux -S "$socket" new-session -d -s examples
tmux -S "$socket" new-window -d -n second
server_pid=$(tmux -S "$socket" display-message -p '#{pid}')
export TMUX="$socket,$server_pid,0"

# Whatever nobody is still using. A directory belonging to a run in progress is
# not a leak even though it is new, and one belonging to a run that is gone is
# a leak even though it is old, so this asks who owns a thing rather than what
# it is called. Excluding by name got the first case right and made the second
# invisible: a run killed hard enough to skip its own trap left a live daemon
# that this gate, `just fixture-root` and `examples/sweep` all called clean.
abandoned() {
    local entry owner
    for entry in "$dev_root"/* "$dev_root"/.[!.]*; do
        [ -e "$entry" ] || continue
        owner="$entry/owner"
        if [ -f "$owner" ] && kill -0 "$(cat "$owner")" 2>/dev/null; then
            continue
        fi
        printf '%s\n' "$entry"
    done
}

failures=()
run() {
    local name=$1
    shift
    printf '\n=== example %s\n' "$name"
    if ! cargo run --quiet --manifest-path "$manifest" --example "$name" "$@"; then
        failures+=("$name")
    fi
}

run inspect
run find --features query -- sh
run scratch --features test-support
run sweep --features test-support
run watch --features control-mode,test-support
run matrix --features plan,control-mode,blocking,test-support,query

leaked=$(abandoned)

status=0
if [ ${#failures[@]} -ne 0 ]; then
    printf '\nexamples exited nonzero: %s\n' "${failures[*]}" >&2
    status=1
fi
if [ -n "$leaked" ]; then
    printf '\n%s holds what nobody is using:\n%s\n' "$dev_root" "$leaked" >&2
    printf '\ntmux does not unlink its socket when the server exits, so whatever\n' >&2
    printf 'named one owns removing it. A daemon may still be answering on one of\n' >&2
    printf 'these, holding a pseudo-terminal per pane; check with `tmux -S <path>\n' >&2
    printf 'kill-server` before removing the directory.\n' >&2
    status=1
fi

if [ "$status" -eq 0 ]; then
    printf '\nevery example ran and left nothing behind\n'
fi
exit "$status"
