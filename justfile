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

# `--all-features` alone is not enough, and the powerset does not cover the
# difference: `cargo hack --all-targets` builds libs, bins, tests, benches and
# examples, and compiles no doctest at all. So a doctest reaching for an item
# behind a feature it does not declare is invisible to every gate -- which is
# how two of them survived, one since the tree was imported.
#
# Minimal core and facade builds, then each client's default features with its
# fixture, cover the configurations a contributor uses. A doctest needing the
# fixture enables `test-support`; other optional features remain off.
#
# Run documentation tests, including the examples on the front page
[group: 'test']
doctest:
    cargo test --locked --workspace --doc --all-features
    cargo test --locked --package libtmux-core --package libtmux-blocking --package libtmux --doc --no-default-features
    cargo test --locked --package libtmux-tokio --doc --features test-support
    cargo test --locked --package libtmux --doc --features test-support

# Each doctest is a process of its own, and rustdoc gives it the sandbox only
# through the environment it inherits. A canary doctest asserts that, and a
# second run proves the assertion can fail.
#
# Check that a doctest runs inside the tmux sandbox
[group: 'test']
doctest-sandbox:
    uv run scripts/sandbox/check_doctest_sandbox.py

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
# Verify complete API programs and their native attachment targets
api-examples:
    uv run scripts/check-api-examples/check_api_examples.py

# Run every example and check it cleaned up
[group: 'test']
examples:
    uv run scripts/run-examples/run_examples.py

# tmux never unlinks its socket when the server exits, so whatever named one
# owns removing it. `TestServer` does; a test that hand-rolls a socket path
# does not, and the file it leaves is invisible until the root fills up. Pure
# filesystem, no toolchain, so it is part of `just check`. It decides by who
# owns a fixture, not by its name or age, as scripts/fixture-root explains.
#
# Report anything the suite left in the fixture root
[group: 'test']
fixture-root:
    uv run scripts/fixture-root/fixture_root.py

# Run the suite on each crate's minimum supported Rust version
#
# Two floors, because they are two promises. The libraries support 1.85 and
# say so; tmux-mcp cannot, because rmcp and darling require 1.88. A crate
# tested only at the workspace floor can publish a rust-version it does not
# meet, which is what happened before this covered it.
[doc("Run the suite on each crate's minimum supported Rust version")]
[group: 'test']
msrv:
    rustup run 1.85.0 cargo test --locked \
        --package libtmux-core --package libtmux-blocking --package libtmux-tokio \
        --package libtmux --package libtmux-macros --package tmux-workspace \
        --package mcp-swap \
        --all-targets --all-features
    rustup run 1.85.0 cargo hack check --locked \
        --package libtmux-core --package libtmux-blocking --package libtmux-tokio \
        --package libtmux --package libtmux-macros \
        --each-feature --all-targets
    rustup run 1.88.0 cargo test --locked \
        --package tmux-mcp --all-targets

# The compile-fail cases run only on the toolchain `rust-toolchain.toml` pins,
# because their `.stderr` files quote rustc's own wording. A toolchain bump
# rewords some of them; this rewrites every mismatch for review.
#
# Rewrite the derive's compile-fail expectations for the pinned toolchain
[group: 'test']
macros-ui-bless:
    TRYBUILD=overwrite cargo test --locked --package libtmux-macros --test derive

# Test the native tool that points agent CLIs at a build of this server
[doc('Test the native tool that points agent CLIs at a build of this server')]
[group: 'test']
swap-test *args:
    cargo test --locked --package mcp-swap --all-targets {{ args }}

# `LIBTMUX_COMPAT_LANES` names the lanes to build and test, such as "3.3a 3.8",
# and may name a tmux tag, branch or commit the manifest does not hold. Unset,
# every lane in .github/ci-matrix.txt runs.
#
# Build the pinned tmux releases and run the suite against each
[group: 'test']
compat:
    uv run scripts/tmux-format-compat/tmux_format_compat.py

# A type with well-documented methods and no example of its own still leaves
# someone who arrived from a search with nothing to copy, and that gap is
# invisible unless it is counted.
#
# Report which crate-root types lack a runnable example
[group: 'docs']
example-coverage: (_example-coverage "libtmux-core") (_example-coverage "libtmux-blocking") (_example-coverage "libtmux-tokio") (_example-coverage "libtmux")

# Needs nightly for rustdoc's JSON output, so it runs beside `api-check`
# rather than in `just check`.
#
# Fail when any crate-root type lacks a runnable example
[group: 'docs']
example-coverage-check: (_example-coverage "libtmux-core" "--require-all") (_example-coverage "libtmux-blocking" "--require-all") (_example-coverage "libtmux-tokio" "--require-all") (_example-coverage "libtmux" "--require-all")

[private]
_runtime-json crate:
    cargo +nightly rustdoc --locked --package {{ crate }} --all-features --lib \
        -- -Zunstable-options --output-format json --document-hidden-items

[private]
_example-coverage crate strict="": (_runtime-json crate)
    uv run scripts/example-coverage/example_coverage.py {{ strict }} \
        target/doc/{{ replace(crate, "-", "_") }}.json

# Which format names each listing asks tmux for.
#
# The supplement arrays behind it are macro-generated, so the answer is in the
# source and cannot be read out of it. `each_listing_records_the_fields_it_requests`
# is both the recorder and the gate: it fails under `just test` when the set
# drifts, so nothing extra runs in `just check`.
[doc('Which format names each listing asks tmux for')]
[group: 'release']
list-profiles:
    LIBTMUX_RERECORD=1 uv run scripts/require-tests/require_tests.py \
        cargo test --locked --package libtmux-tokio --all-features \
        --lib each_listing_records_the_fields_it_requests

# Regenerate the recorded public API surface
#
# Needs nightly for rustdoc's JSON output, so it is not part of `just check`.
[doc('Regenerate the recorded public API surface')]
[group: 'release']
api: (_api-record "libtmux-core") (_api-record "libtmux-blocking") (_api-record "libtmux-tokio") (_api-record "libtmux")

[private]
_api-record crate: (_runtime-json crate)
    uv run scripts/public-api/public_api.py target/doc/{{ replace(crate, "-", "_") }}.json \
        > crates/{{ crate }}/docs/public-api.txt

# No automated semver gate runs yet, and a 0.1.N release may break the API, so
# this is what makes a change to the surface visible. It does not judge whether
# a change is allowed; it says one happened.
#
# Report any change to the public API surface
[group: 'release']
api-check: (_api-check "libtmux-core") (_api-check "libtmux-blocking") (_api-check "libtmux-tokio") (_api-check "libtmux")

[private]
_api-check crate: (_runtime-json crate)
    uv run scripts/public-api/public_api.py \
        --check crates/{{ crate }}/docs/public-api.txt \
        target/doc/{{ replace(crate, "-", "_") }}.json

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

# The support scripts under scripts/ and the pseudo-terminal fixtures of the
# tmux-workspace tests are typed Python. Format, lint and type-check them, then
# run the script tests, which include the supervisor cases and the checks that
# hold the CI workflows to the plan.
#
# Format, lint, type check and test the support scripts
[group: 'lint']
scripts:
    uv run --project scripts --locked ruff format --check --config scripts/pyproject.toml scripts crates/tmux-workspace/tests/fixtures
    uv run --project scripts --locked ruff check --config scripts/pyproject.toml scripts crates/tmux-workspace/tests/fixtures
    uv run --project scripts --locked ty check --project scripts scripts crates/tmux-workspace/tests/fixtures
    uv run --project scripts --locked pytest scripts

# Run clippy over every target and feature
[group: 'lint']
clippy:
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

# `cargo check` warns and exits zero, so running a configuration is not the
# same as gating it: `just clippy` and `just docs` deny warnings but each run
# one configuration -- `--all-features` -- and a warning that appears only when
# a feature is *off* is not in it. Running clippy here rather than `check` is
# what makes the combinations count, and it carries the four denied lints
# across every one of them rather than across the single build clippy ran.
#
# Lint every pair of features, denying what `check` would only mention
[group: 'lint']
features:
    cargo clippy --locked --package libtmux-core --package libtmux-blocking \
        --package libtmux-tokio --package libtmux --all-targets --no-default-features -- -D warnings
    cargo hack clippy --locked --workspace --feature-powerset --depth 2 --all-targets -- -D warnings

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
#
# Every feature, as docs.rs builds it, then each runtime crate with its defaults
# and with none, and tmux-workspace as a library dependent builds it, without the
# command: a warning that only a partial build raises would otherwise reach
# every such reader and no gate.
[group: 'docs']
docs:
    RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --all-features --no-deps
    RUSTDOCFLAGS='-D warnings' cargo doc --locked --package libtmux-core --package libtmux-blocking \
        --package libtmux-tokio --package libtmux --no-deps
    RUSTDOCFLAGS='-D warnings' cargo doc --locked --package libtmux-core --package libtmux-blocking \
        --package libtmux-tokio --package libtmux --no-default-features --no-deps
    RUSTDOCFLAGS='-D warnings' cargo doc --locked --package tmux-workspace --no-default-features --no-deps

# Each published crate ships a changelog of its own, so a reader of one crate
# does not have to read the history of the other crates.
#
# Fail when a published crate has no changelog, does not ship it or does not
# link it
[group: 'docs']
changelogs:
    uv run scripts/check-changelogs/check_changelogs.py

# The catalog is hand-maintained, so a format tmux gained -- or one nobody
# ever added -- is invisible: nothing fails, callers just cannot ask for the
# field. Checking needs no tmux source, so it is part of `just check`.
#
# Report formats tmux publishes that the catalog does not carry
[group: 'docs']
format-coverage-check:
    uv run scripts/format-coverage.py --check

# Rerecord the ledger against a tmux checkout
[group: 'release']
format-coverage source:
    uv run scripts/format-coverage.py {{ source }}

# rustdoc attaches prose to whatever item follows it and never checks that the
# two match, so a block inserted one line too high gives a type its
# neighbour's summary. Pure text, no toolchain, so it is part of `just check`.
#
# Report doc comments that were split across two items
[group: 'docs']
doc-blocks:
    uv run scripts/check-doc-blocks/check_doc_blocks.py crates

# A block of program output pasted into prose is a copy, and a copy drifts:
# the example grows a column, the prose keeps rendering, and the reader is
# shown a table no command produces. This runs the example each block names
# and compares every column but the clock.
[group: 'docs']
example-tables:
    uv run scripts/check-example-tables/check_example_tables.py README.md

# A Contents list is a copy of a document's headings, and a copy drifts: a
# renamed section leaves the list rendering a link to an anchor that is gone.
#
# Report Contents lists that disagree with their document's sections
[group: 'docs']
doc-contents:
    uv run scripts/check-contents/check_contents.py crates/libtmux/docs/design.md \
        crates/libtmux-tokio/docs/design.md crates/libtmux-tokio/docs/findings.md

# rustdoc supplies a doctest's `fn main`, so a block whose body is only a
# hidden function definition compiles it and then runs an empty main. Every
# assertion inside is dead, and nothing says so: it renders like any other
# example and `cargo test --doc` counts it as a pass. Pure text, no toolchain,
# so it is part of `just check`.
#
# Report doctests that compile a definition and execute nothing
[group: 'docs']
doctests-run:
    uv run scripts/check-doctests-run/check_doctests_run.py crates

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
    cargo bench --package libtmux-tokio --features test-support --bench hierarchy -- {{ args }}

# Run the pane-wait benchmark against real tmux, both wait paths
[group: 'bench']
bench-waits *args:
    cargo bench --package libtmux-tokio --features test-support,control-mode --bench waits -- {{ args }}

# Compare the last benchmark run with the one before it; fails past 2x
[group: 'bench']
bench-compare *args:
    uv run scripts/bench-regressions/bench_regressions.py target/criterion {{ args }}

# Build the crates that get published, and verify what they contain
#
# --allow-dirty so this stays runnable mid-change; `cargo publish` does its
# own clean-tree check when it matters.
#
# Cargo treats staged registry versions as immutable. A fresh target keeps
# same-version edits from reusing source extracted by an earlier run.
[doc('Build the crates that get published, and verify what they contain')]
[group: 'release']
package:
    uv run scripts/check-package-contents/check_package_contents.py --build

# Regenerate the tmux option schema from tmux's own source
[group: 'release']
option-schema path:
    uv run scripts/generate-option-schema.py {{ path }}

# Run every gate CI runs
[group: 'check']
check: fmt-check scripts clippy test swap-test doctest doctest-sandbox api-examples examples example-tables fixture-root docs doc-blocks doc-contents changelogs doctests-run format-coverage-check features deny msrv package

[private]
_entr-warn:
    @echo "----------------------------------------------------------"
    @echo "     ! File watching functionality non-operational !      "
    @echo "                                                          "
    @echo "Install entr(1) to automatically run tasks on file change."
    @echo "See https://eradman.com/entrproject/                      "
    @echo "----------------------------------------------------------"


# Remove ignored build artifacts and caches. Untracked files and .env stay.
[group: 'clean']
clean:
    git clean -fdX -e '!.env' -e '!.env.*'

# Preview what clean will delete.
[group: 'clean']
clean-dry:
    git clean -ndX -e '!.env' -e '!.env.*'

# Remove ignored and untracked files. .env and .env.* stay.
# git clean -x ignores .gitignore and only preserves -e patterns, so .env is named here rather than negated.
[group: 'clean']
[confirm("This will delete all untracked and ignored files. Continue?")]
nuke:
    git clean -fdx -e '.env' -e '.env.*'
