#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

dependencies=$(cargo tree --locked -p libtmux --all-features --edges all --target all --prefix none)
if [[ $(printf '%s\n' "$dependencies" | wc -l) -ne 1 ]]; then
    printf 'The core gained dependencies:\n%s\n' "$dependencies" >&2
    exit 1
fi

cargo fmt --all --check
CLIPPY_CONF_DIR="$PWD" cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
cargo run --locked -q -p split-demo --bin offline
cargo run --locked -q -p split-demo --features live --bin live
