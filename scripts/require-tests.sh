#!/usr/bin/env bash
# Run a test command and fail when it ran no test.
#
# A `cargo test` name filter that matches nothing exits 0 with
# "running 0 tests", so a renamed test would leave its lane green without
# running anything. Pass the whole command: scripts/require-tests.sh cargo test ...

set -uo pipefail

log="$(mktemp)"
trap 'rm -f "$log"' EXIT

"$@" 2>&1 | tee "$log"
status="${PIPESTATUS[0]}"
if [[ "$status" -ne 0 ]]; then
    exit "$status"
fi
if ! grep -Eq '^running [1-9][0-9]* tests?$' "$log"; then
    printf 'no test ran for: %s\n' "$*" >&2
    exit 1
fi
