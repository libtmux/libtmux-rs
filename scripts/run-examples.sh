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

before=$(find "$dev_root" -mindepth 1 -maxdepth 1 -not -path "$run_dir" | sort)

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
run matrix --features plan,control-mode,blocking,test-support,query

after=$(find "$dev_root" -mindepth 1 -maxdepth 1 -not -path "$run_dir" | sort)
leaked=$(comm -13 <(printf '%s\n' "$before") <(printf '%s\n' "$after"))

status=0
if [ ${#failures[@]} -ne 0 ]; then
    printf '\nexamples exited nonzero: %s\n' "${failures[*]}" >&2
    status=1
fi
if [ -n "$leaked" ]; then
    printf '\nexamples left files in %s:\n%s\n' "$dev_root" "$leaked" >&2
    status=1
fi

if [ "$status" -eq 0 ]; then
    printf '\nevery example ran and left nothing behind\n'
fi
exit "$status"
