# justfile for the libtmux Rust workspace
# https://just.systems/

set shell := ["bash", "-uc"]

# List all available commands
default:
    @just --list

# Run the whole test suite
[group: 'test']
test *args:
    cargo test --locked --workspace --all-targets --all-features {{ args }}

# Run documentation tests, including the examples on the front page
[group: 'test']
doctest:
    cargo test --locked --workspace --doc --all-features

# Run the suite on each crate's minimum supported Rust version
#
# Two floors, because they are two promises. The libraries support 1.85 and
# say so; tmux-mcp cannot, because rmcp and darling require 1.88. A crate
# tested only at the workspace floor can publish a rust-version it does not
# meet, which is what happened before this covered it.
[group: 'test']
msrv:
    rustup run 1.85.0 cargo test --locked \
        --package libtmux --package libtmux-macros --package tmux-workspace \
        --all-targets --all-features
    rustup run 1.85.0 cargo hack check --locked \
        --package libtmux --package libtmux-macros \
        --each-feature --all-targets
    rustup run 1.88.0 cargo test --locked \
        --package tmux-mcp --all-targets

# Test the script that points every agent CLI at a build of this server
#
# Needs pytest and tomlkit. -c and --confcutdir keep pytest from walking up
# into the Python project above this directory, whose config and conftest
# would otherwise be applied to a Rust workspace.
[group: 'test']
swap-test *args:
    python3 -m pytest scripts/test_mcp_swap.py -c /dev/null --confcutdir=. {{ args }}

# Build every pinned tmux release and run the suite against each
[group: 'test']
compat:
    bash scripts/test-tmux-format-compat.sh

# Regenerate the recorded public API surface
#
# Needs nightly for rustdoc's JSON output, so it is not part of `just check`.
[group: 'release']
api:
    cargo +nightly rustdoc -p libtmux --all-features \
        -- -Zunstable-options --output-format json
    python3 scripts/public-api.py target/doc/libtmux.json \
        > crates/libtmux/docs/public-api.txt

# There is no automated semver gate while every version is a prerelease, so
# this is what makes a change to the surface visible. It does not judge whether
# a change is allowed; it says one happened.
#
# Report any change to the public API surface
[group: 'release']
api-check:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo +nightly rustdoc -p libtmux --all-features \
        -- -Zunstable-options --output-format json > /dev/null
    python3 scripts/public-api.py target/doc/libtmux.json > /tmp/libtmux-api-now.txt
    if ! diff -u crates/libtmux/docs/public-api.txt /tmp/libtmux-api-now.txt; then
        printf '\npublic API changed. Run `just api` and commit the result.\n' >&2
        exit 1
    fi
    printf 'public API unchanged\n'

# Nightly and a sanitizer, so `fuzz/` is not a workspace member and this is not
# part of `just check`. The seeds matter: random bytes rarely produce a line
# beginning with `%`, so without them the control-mode target spends its whole
# budget proving that arbitrary input is text.
#
# Fuzz one parser, seeded with the shapes it actually sees
[group: 'test']
fuzz target="control_line" seconds="60":
    cargo +nightly fuzz run {{ target }} fuzz/corpus/{{ target }} fuzz/seeds/{{ target }} \
        -- -max_total_time={{ seconds }} -rss_limit_mb=4096

# List the fuzz targets
[group: 'test']
fuzz-list:
    cargo +nightly fuzz list

# Watch files and run tests on change (requires entr)
[group: 'test']
watch-test:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v entr > /dev/null; then
        fd -e rs -e toml . crates | entr -c just test
    else
        just test
        just _entr-warn
    fi

# Check formatting without changing anything
[group: 'lint']
fmt-check:
    cargo fmt --all -- --check

# Format the workspace
[group: 'lint']
fmt:
    cargo fmt --all

# Run clippy over every target and feature
[group: 'lint']
clippy:
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

# Check the crate builds with no features, and each feature alone
[group: 'lint']
features:
    cargo check --locked --package libtmux --all-targets --no-default-features
    cargo hack check --locked --workspace --each-feature --all-targets

# Check dependency licences, advisories, and sources
[group: 'lint']
deny:
    cargo deny check

# Watch files and run clippy on change (requires entr)
[group: 'lint']
watch-clippy:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v entr > /dev/null; then
        fd -e rs -e toml . crates | entr -c just clippy
    else
        just clippy
        just _entr-warn
    fi

# Build the API documentation
[group: 'docs']
docs:
    RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --all-features --no-deps

# Build documentation including dependencies, so intra-doc links resolve
[group: 'docs']
docs-full:
    cargo doc --locked --workspace --all-features

# Serve the built documentation on http://127.0.0.1:8971
[group: 'docs']
serve-docs port='8971': docs-full
    @echo "serving http://127.0.0.1:{{ port }}"
    python3 -m http.server {{ port }} --bind 127.0.0.1 --directory target/doc

# Run the hierarchy benchmark against real tmux
[group: 'bench']
bench *args:
    cargo bench --features test-support --bench hierarchy -- {{ args }}

# Build the crates that get published, and verify what they contain
#
# --allow-dirty so this stays runnable mid-change; `cargo publish` does its
# own clean-tree check when it matters.
[group: 'release']
package:
    cargo package --locked --allow-dirty \
        --package libtmux --package libtmux-macros --all-features
    bash scripts/check-package-contents.sh

# Regenerate the tmux option schema from tmux's own source
[group: 'release']
option-schema path:
    python3 scripts/generate-option-schema.py {{ path }}

# Run every gate CI runs
[group: 'check']
check: fmt-check clippy test doctest docs features deny msrv package

[private]
_entr-warn:
    @echo "----------------------------------------------------------"
    @echo "     ! File watching functionality non-operational !      "
    @echo "                                                          "
    @echo "Install entr(1) to automatically run tasks on file change."
    @echo "See https://eradman.com/entrproject/                      "
    @echo "----------------------------------------------------------"
