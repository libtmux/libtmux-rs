#!/usr/bin/env bash

# Run every shipped example to completion and prove it left nothing behind.
#
# `cargo test --all-targets` compiles an example and never runs it, so an
# example that fails at runtime passes every other gate. This runs each one
# against a server this script owns rather than whatever the reader happens to
# have, and fails when one exits nonzero or leaves a socket in the shared root.
#
# The table below is the other half of that. Naming the examples to run made
# adding one a silent no-op: the new file compiled, this gate never called it,
# and nothing said so. Three `tmux-mcp` examples sat unrun that way. So the
# table is checked against `cargo metadata` before anything runs, in both
# directions -- an example with no row fails, and a row naming no example
# fails, which is what catches a rename rather than only an addition.

set -euo pipefail

readonly dev_root=/tmp/libtmux-rs-dev

# crate | example | features | arguments | stdin driver | must print
#
# Blank lines and `#` comments are ignored; every other row must name an
# example that exists, and every example must have a row.
#
# The last column is what the example must print, and it answers two questions
# a leak check cannot.
#
# Containment: `inspect` and `find` resolve their server with
# `Server::from_env().or_else(|_| Server::new())`, so a `$TMUX` this script
# failed to set would send them to the reader's own default server -- where
# they would run perfectly, create nothing, and leave no socket behind to
# notice. Naming a session this script made can only come from this server.
#
# Demonstration: an example that runs and shows nothing passes an exit-code
# check. `find` searched for `sh` against a fixture running the login shell and
# matched nothing every time; `scratch` printed a line count and none of the
# session it built. Naming what each is for makes that a failure.
readonly table="
libtmux  | inspect  |                                               |    |               | examples
libtmux  | find     | query                                         | sh |               | in window
libtmux  | scratch  | test-support                                  |    |               | hello from tmux
libtmux  | sweep    | test-support                                  |    |               | fixture
libtmux  | watch    | control-mode,test-support                     |    |               | on the same socket
libtmux  | matrix   | plan,control-mode,blocking,test-support,query |    |               | every mode built the same thing: true
libtmux  | orchestrate | test-support                              |    |               | 3 of 3 jobs finished
libtmux  | recover  | test-support                                  |    |               | is_object_gone: false
tmux-mcp | budget   |                                               |    |               | of it output schemas
tmux-mcp | surface  |                                               |    |               | answers with:
tmux-mcp | readonly |                                               |    | mcp_handshake | serverInfo
"

rows() {
    printf '%s\n' "$table" | sed -e 's/#.*//' -e 's/[[:space:]]*|[[:space:]]*/|/g' \
        -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' | grep -v '^$'
}

# What the workspace actually ships, asked of Cargo rather than of the
# filesystem: a crate may place its examples anywhere, and a second crate
# growing one is exactly the case a path glob under `crates/libtmux` missed.
declared=$(rows | cut -d'|' -f1,2 | sort)
existing=$(cargo metadata --no-deps --format-version 1 \
    | python3 -c '
import json, sys
for pkg in json.load(sys.stdin)["packages"]:
    crate = pkg["name"]
    for target in pkg["targets"]:
        if "example" in target["kind"]:
            print(crate + "|" + target["name"])
' | sort)

unlisted=$(comm -13 <(printf '%s\n' "$declared") <(printf '%s\n' "$existing"))
missing=$(comm -23 <(printf '%s\n' "$declared") <(printf '%s\n' "$existing"))
if [ -n "$unlisted" ] || [ -n "$missing" ]; then
    printf 'the example table does not match the workspace:\n' >&2
    # shellcheck disable=SC2086 # a list, and meant to split
    if [ -n "$unlisted" ]; then printf '  shipped but never run: %s\n' $unlisted >&2; fi
    # shellcheck disable=SC2086 # a list, and meant to split
    if [ -n "$missing" ]; then printf '  named here but absent:  %s\n' $missing >&2; fi
    printf '\nAdd a row to `table` in this script, or remove the stale one.\n' >&2
    printf 'A row names the crate, the example, its features, its arguments,\n' >&2
    printf 'and optionally a function to write its stdin.\n' >&2
    exit 1
fi

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
# The second window runs `sh` rather than the login shell because `find`
# searches for it: a fixture whose panes run zsh makes that example print
# "nothing here is running sh" and demonstrate none of what it is for.
tmux -S "$socket" new-session -d -s examples
tmux -S "$socket" new-window -d -n second sh
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
        # A server still answering is someone's work, not a leak. `just
        # fixture-root` has asked this second question about the test root
        # since it was written; this root had only the first, so a hand spike
        # holding a bare socket -- which is what `scratch` and `orchestrate`
        # make, with no directory to put an owner file in -- was reported as
        # abandoned while a server was plainly attached to it.
        if [ -S "$entry" ] && tmux -S "$entry" list-sessions >/dev/null 2>&1; then
            continue
        fi
        printf '%s\n' "$entry"
    done
}

# `readonly` is a server: it runs until its client hangs up, and hanging up
# before it has initialised is the one case it reports as an error, because a
# client that never spoke never started. So the gate has to be that client.
# Waiting for the reply rather than sleeping a fixed span keeps this from
# asserting how fast the machine is; the cap only stops a wedge from hanging
# the gate.
#
# The request is written before the polling starts, and that ordering is what
# makes the cap safe rather than merely generous: the line is already in the
# pipe, so a poll that gives up still leaves a server that reads it, answers,
# sees EOF and exits clean. Measured against a 40-second wait on cargo's build
# lock -- which falls inside this window, where a cold build does not -- the
# example still exited 0 with its full reply after 39 seconds. Polling before
# writing would remove that and leave the number looking unchanged.
mcp_handshake() {
    local out=$1 waited=0
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"run-examples","version":"0"}}}'
    while ! grep -q '"result"' "$out" 2>/dev/null; do
        if [ "$waited" -ge 600 ]; then break; fi
        sleep 0.05
        waited=$((waited + 1))
    done
    return 0
}

# Built before any of them runs, so the run phase is the example's own cost.
# A driven example measures how long its server takes to answer, and a cold
# build inside that window reads as a server that never did.
printf 'building %s examples\n' "$(rows | wc -l | tr -d ' ')"
while IFS='|' read -r crate name features args driver expect; do
    opts=(--quiet --manifest-path "crates/$crate/Cargo.toml" --example "$name")
    if [ -n "$features" ]; then opts+=(--features "$features"); fi
    cargo build "${opts[@]}"
done < <(rows)

failures=()
while IFS='|' read -r crate name features args driver expect; do
    printf '\n=== example %s/%s\n' "$crate" "$name"
    opts=(--quiet --manifest-path "crates/$crate/Cargo.toml" --example "$name")
    if [ -n "$features" ]; then opts+=(--features "$features"); fi
    # shellcheck disable=SC2086 # arguments are a field, and are meant to split
    if [ -z "$driver" ]; then
        out="$run_dir/$name.out"
        if cargo run "${opts[@]}" -- $args </dev/null | tee "$out"; then
            if [ -n "$expect" ] && ! grep -qF "$expect" "$out"; then
                printf 'did not print %s\n' "$expect" >&2
                failures+=("$crate/$name")
            fi
        else
            failures+=("$crate/$name")
        fi
    else
        out="$run_dir/$name.out"
        : > "$out"
        if "$driver" "$out" | cargo run "${opts[@]}" -- $args > "$out"; then
            printf 'answered %s bytes and shut down cleanly\n' "$(wc -c < "$out" | tr -d ' ')"
            if [ -n "$expect" ] && ! grep -qF "$expect" "$out"; then
                printf 'did not print %s\n' "$expect" >&2
                failures+=("$crate/$name")
            fi
        else
            failures+=("$crate/$name")
            sed -n '1,5p' "$out" >&2
        fi
    fi
done < <(rows)

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
