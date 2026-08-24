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

# tmux never unlinks its socket when the server exits, so whatever named one
# owns removing it. `TestServer` does; a test that hand-rolls a socket path
# does not, and the file it leaves is invisible until the root fills up. Pure
# filesystem, no toolchain, so it is part of `just check`.
#
# A run that has not finished is not this gate's business, or a suite in one
# terminal fails the gate in another: a fixture records the process that made
# it, and a socket a server still answers on is in use.
#
# `cargo test --all-targets` compiles an example and never runs it, so one that
# fails at runtime passes every other gate. This runs each against a server it
# owns and fails when one exits nonzero or leaves a socket behind.
#
# Run every example and check it cleaned up
[group: 'test']
examples:
    bash scripts/run-examples.sh

# Report anything the suite left in the fixture root
[group: 'test']
fixture-root:
    #!/usr/bin/env bash
    set -euo pipefail
    root=/tmp/libtmux-rs-test
    if [ ! -d "$root" ]; then
        echo "fixture root absent"
        exit 0
    fi
    left=()
    for entry in "$root"/* "$root"/.[!.]*; do
        [ -e "$entry" ] || continue
        owner="$entry/owner"
        if [ -f "$owner" ] && kill -0 "$(cat "$owner")" 2>/dev/null; then
            continue
        fi
        if [ -S "$entry" ] && tmux -S "$entry" list-sessions >/dev/null 2>&1; then
            continue
        fi
        left+=("${entry#"$root"/}")
    done
    if [ ${#left[@]} -eq 0 ]; then
        echo "fixture root clean"
        exit 0
    fi
    printf 'the fixture root still holds:\n' >&2
    printf '  %s\n' "${left[@]}" >&2
    printf '\ntmux does not unlink its socket when the server exits, so a test that\n' >&2
    printf 'names its own socket path has to remove it. libtmux::test::TestServer\n' >&2
    printf 'owns that lifecycle; prefer it over Server::builder().socket_path().\n' >&2
    exit 1

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

# A type with well-documented methods and no example of its own still leaves
# someone who arrived from a search with nothing to copy, and that gap is
# invisible unless it is counted.
#
# Report which crate-root types lack a runnable example
[group: 'docs']
example-coverage:
    cargo +nightly rustdoc -p libtmux --all-features \
        -- -Zunstable-options --output-format json > /dev/null
    python3 scripts/example-coverage.py target/doc/libtmux.json

# Needs nightly for rustdoc's JSON output, so it runs beside `api-check`
# rather than in `just check`.
#
# Fail when any crate-root type lacks a runnable example
[group: 'docs']
example-coverage-check:
    cargo +nightly rustdoc -p libtmux --all-features \
        -- -Zunstable-options --output-format json > /dev/null
    python3 scripts/example-coverage.py --require-all target/doc/libtmux.json

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
    # A fixed path in /tmp is one file shared between checkouts.
    current="$(mktemp -t libtmux-api-XXXXXX)"
    trap 'rm -f "$current"' EXIT
    python3 scripts/public-api.py target/doc/libtmux.json > "$current"
    if ! diff -u crates/libtmux/docs/public-api.txt "$current"; then
        printf '\npublic API changed. Run `just api` and commit the result.\n' >&2
        exit 1
    fi
    printf 'public API unchanged\n'

# Nightly and a sanitizer, so `fuzz/` is not a workspace member and this is not
# part of `just check`. The seeds matter: random bytes rarely produce a line
# beginning with `%`, so without them the control-mode target spends its whole
# budget proving that arbitrary input is text.
#
# cargo-fuzz builds for the triple it was itself compiled for, so an installed
# binary rather than a built one -- the installers ship musl -- defaults to a
# target that links libc statically, which a sanitizer cannot instrument.
# Naming the host's own triple makes that a property of this recipe.
#
# Fuzz one parser, seeded with the shapes it actually sees
[group: 'test']
fuzz target="control_line" seconds="60":
    cargo +nightly fuzz run --target "$(rustc +nightly -vV | sed -n 's/^host: //p')" \
        {{ target }} fuzz/corpus/{{ target }} fuzz/seeds/{{ target }} \
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

# The catalog is hand-maintained, so a format tmux gained -- or one nobody
# ever added -- is invisible: nothing fails, callers just cannot ask for the
# field. Checking needs no tmux source, so it is part of `just check`.
#
# parity.md is what this project calls its definition of done, so a row marked
# done that names a type nobody wrote is worse than one merely out of date: a
# reader cannot tell a delivered capability from a described one.
#
# Fail when a done parity row names a symbol that does not exist
[group: 'docs']
parity-claims:
    python3 scripts/check-parity-claims.py crates/libtmux/docs/parity.md crates

# Report formats tmux publishes that the catalog does not carry
[group: 'docs']
format-coverage-check:
    python3 scripts/format-coverage.py --check

# Rerecord the ledger against a tmux checkout
[group: 'release']
format-coverage source:
    python3 scripts/format-coverage.py {{ source }}

# rustdoc attaches prose to whatever item follows it and never checks that the
# two match, so a block inserted one line too high gives a type its
# neighbour's summary. Pure text, no toolchain, so it is part of `just check`.
#
# Report doc comments that were split across two items
[group: 'docs']
doc-blocks:
    python3 scripts/check-doc-blocks.py crates

# rustdoc supplies a doctest's `fn main`, so a block whose body is only a
# hidden function definition compiles it and then runs an empty main. Every
# assertion inside is dead, and nothing says so: it renders like any other
# example and `cargo test --doc` counts it as a pass. Pure text, no toolchain,
# so it is part of `just check`.
#
# Report doctests that compile a definition and execute nothing
[group: 'docs']
doctests-run:
    python3 scripts/check-doctests-run.py crates

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
check: fmt-check clippy test doctest examples fixture-root docs doc-blocks doctests-run parity-claims format-coverage-check features deny msrv package

[private]
_entr-warn:
    @echo "----------------------------------------------------------"
    @echo "     ! File watching functionality non-operational !      "
    @echo "                                                          "
    @echo "Install entr(1) to automatically run tasks on file change."
    @echo "See https://eradman.com/entrproject/                      "
    @echo "----------------------------------------------------------"
