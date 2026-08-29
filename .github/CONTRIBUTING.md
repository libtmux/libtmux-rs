# Contributing

This repository is a Cargo workspace of four published crates, and the gates
below are what a change has to pass. It was extracted from the Python libtmux
repository; its `uv`, `pytest`, `ruff`, and `mypy` conventions do not apply to
the Rust crates. The `mcp_swap.py` maintenance utility is the exception: `uv`
supplies its dependencies, and `pytest` tests it under `just check`.

For how the prose reads — README, changelog, release notes, commit messages,
rustdoc, and source comments — see [`WRITING.md`](WRITING.md).

## Getting set up

You need tmux 3.2a or newer on `PATH` and a Unix target. Native Windows is
unsupported because tmux is unavailable there; WSL works.

`rust-toolchain.toml` pins the toolchain, so rustup installs it on the first
cargo command. The gate also needs the two MSRV floors, `uv`, and three Cargo
tools:

```console
$ rustup toolchain install 1.85.0 1.88.0
```

```console
$ cargo install just cargo-hack cargo-deny
```

Install [`uv`](https://docs.astral.sh/uv/getting-started/installation/)
separately; Cargo does not manage the Python test environment.

Nightly is needed only for `just api-check`, `just example-coverage-check`,
and `just fuzz`, none of which are part of the local gate.

`just` with no argument lists every recipe. Cargo is authoritative; the
justfile only groups Cargo commands.

## Building

```console
$ cargo build --workspace --all-features
```

Four crates, all published:

| Crate | What it is |
| --- | --- |
| `crates/libtmux` | The async tmux client and object model |
| `crates/libtmux-macros` | `#[derive(Filterable)]`, for downstream structs |
| `crates/tmux-mcp` | A Model Context Protocol server over tmux |
| `crates/tmux-workspace` | A tmuxp-style YAML builder |

`tmux-mcp` carries its own `version` and `rust-version` rather than inheriting
the workspace's: it moves at its own pace, and `rmcp` and `darling` require a
newer compiler than the libraries promise. `fuzz/` is excluded from the
workspace because it needs nightly and a sanitizer.

`tmux-mcp` and `tmux-workspace` exist to use `libtmux` from outside it, and
being genuinely published is part of that job — a crate built only inside its
own workspace never proves its dependency requirements resolve.
`tmux-workspace` could not be published at all until `libtmux` shipped the
`plan` feature it asks for, which is not visible from inside the tree. When
either needs a workaround, that is a finding about `libtmux`.

## Running the tests

```console
$ just test
```

Tests run against real tmux. There are no mocks; a test that would need one is
usually asking for a design change. `libtmux::test::TestServer`, behind the
`test-support` feature, gives each test an isolated socket and deterministic
cleanup. Tests are functions, not methods on a struct.

Every test runs against whatever `tmux` resolves to, unless
`LIBTMUX_TEST_TMUX` names one:

```console
$ LIBTMUX_TEST_TMUX=/path/to/tmux-3.5a just test
```

Fixture deadlines bound a tmux that starts with a core to spare. On a machine
running several times its cores in work -- a shared runner, or a laptop with
another suite on it -- five seconds stops bounding startup and starts deciding
results, and tests that wait on a fixture fail in a set that moves between
runs. `LIBTMUX_TEST_TIMEOUT_SCALE` widens every fixture deadline by a factor,
read once and never below `1`:

```console
$ LIBTMUX_TEST_TIMEOUT_SCALE=4 just test
```

Unset, deadlines are what they always were. It is not a fix for a test that
waits on the wrong thing: it moves the ceiling, so a hung fixture takes that
much longer to say so.

Prefer that over prepending to `PATH`. `TestServer` used to read only `PATH`,
so the variable steered the format-compatibility tests and nothing else, and a
run pinned that way passed against whichever tmux `PATH` held. A pass about
the wrong binary reports nothing and looks exactly like a pass about the right
one. `just compat` builds each pinned release and runs the whole suite against
it, so a test that depends on behaviour a release does not have needs a
`libtmux::since::*` gate rather than an assumption about the machine.

Doctests are a separate target and are part of the gate:

```console
$ just doctest
```

Four READMEs are compiled along with them, so a Rust example on any of these
pages is a test rather than prose:

- `crates/libtmux/README.md`, by `#![doc = include_str!(…)]` at the top of
  `crates/libtmux/src/lib.rs`: that README is the crate documentation.
- `README.md`, by a `#[cfg(doctest)] #[doc = include_str!(…)]` item further
  down the same file, so the front page is tested without appearing in the
  rendered documentation.
- `crates/tmux-workspace/README.md`, by the same top-of-crate include in
  `crates/tmux-workspace/src/lib.rs`.
- `crates/libtmux-macros/README.md`, by `MacrosReadme` in
  `crates/tmux-workspace/src/lib.rs`, reached through a symlink. A proc-macro
  crate cannot doctest a README that derives through it, and the example needs
  `libtmux` under its own name, which is only true from a third crate.
  `crates/libtmux/tests/macros_readme.rs` compiles the same example again as
  an ordinary test.

`crates/tmux-mcp/README.md` is the page not covered; its blocks are prose.

Nothing checks that a page stays wired. Deleting one of those `include_str!`
items stops testing that README and every gate stays green, so treat the item
as load-bearing, and wire a new README in when adding one.

### Where a test goes

- **Beside the code**, in an inline `#[cfg(test)] mod tests`, when it reaches
  an unexported identifier.
- **In the crate's `tests/` directory** otherwise. Each file there is its own
  binary, named for the surface it covers — `control.rs`, `query.rs`,
  `mutations.rs`.
- **Named `real_tmux_compat_…`** when it pins behavior or wording against real
  tmux releases rather than against our own expectations. These are the tests
  the compatibility lane exists to run, and the prefix is how they are found.
  Version-specific behavior belongs in one of these, not in a comment.

A passing gate is evidence only once it has been shown capable of failing.
Pair a new test with a deliberate break that proves it bites.

### The fixture root

**This workspace owns `/tmp/libtmux-rs-test/` and nothing outside it.** More
than one libtmux lives on a developer's machine, and the Python suites put
their sockets in `/tmp` too. Sharing the root makes "whose leftover is this"
unanswerable, and a stray reaper or a socket collision then reads as a bug in
whatever ran next: one such session ended with 3,023 of 4,096 pseudo-terminals
held and `fork` failing with `No space left on device` in an unrelated build.

- Fixtures go under `/tmp/libtmux-rs-test/`. `TestServer` puts them there; do
  not pass a socket path outside it.
- Throwaway sockets for hand spikes go under `/tmp/libtmux-rs-dev/`, so they
  are as obviously ours as the fixtures are and can be swept without thinking.
- `reap_abandoned_servers` only ever looks inside the fixture root, so it
  cannot reach another workspace's server however abandoned that server looks.
  Keep it that way.
- Socket paths are bounded by `sun_path` at about 108 bytes, which is why the
  root is short. A scratch directory deep under `$TMPDIR` fails to bind with
  "File name too long".
- **tmux does not unlink its socket when the server exits**, so whatever named
  the socket owns removing it. `TestServer` does; a test that reaches for
  `Server::builder().socket_path(...)` and starts a server there does not, and
  the file it leaves is invisible until the root fills up. `just fixture-root`
  fails when anything is left behind, and runs as part of `just check`.

## Flaky, or broken?

Tests drive a real tmux server, so they are load-sensitive in a way a mocked
suite is not. A single failure in a full-suite run warrants re-running that
test in isolation before blaming the change:

```console
$ cargo test --locked -p <crate> --test <file> --all-features <test_name>
```

Three signs it is the machine rather than the code: the failure moves between
runs, the failure reads as a timeout, an empty listing, or `no server running`
rather than a wrong value, and the file that failed is not one the change
touched.

That is not permission to shrug. Several have turned out to be real defects in
the test, and two shapes account for most of them:

- **Typing into a pane that cannot read yet.** tmux hands back a pane the
  moment it forks, long before the shell in it starts, and keys sent before
  then are swallowed. `typing_fixture` waits for the prompt; a test that types
  without it passes on an idle machine and not on a loaded one.
- **A fixed sleep standing in for a wait.** A duration chosen to cover a
  latency that has no upper bound is a guess, and load is what collects on it.
  Wait for the observable instead — `Pane::wait_for_text` and
  `Pane::wait_for_quiet` are in the library and need no feature, and
  `tmux-mcp` exposes the same shapes as tools. A wait must assert the outcome
  it got, since one that runs to its deadline still returns successfully.

A test that only passes on an idle machine is a broken test, and the fix
belongs in the test rather than in a rerun.

### Asking tmux what it does

Establish a behaviour with the command the code sends, not with a convenient
one. `display-message` carries `CMD_CLIENT_CANFAIL`, so it resolves a target
it cannot find by falling back rather than refusing: `display-message -t
<session>:<vacated index>` answers about some other window, while
`select-window` with the same target exits 1. A probe built on it reports a
behaviour the real command does not have.

The same flag makes the reverse true. An absent target expands every format
empty and exits zero, so a probe that counts an empty answer as absence cannot
tell it from a command that failed.

Two investigations here have been decided by that difference, in both
directions, so the rule is worth the extra minute: send what the code sends.

## Checks that must pass

```console
$ just check
```

That runs, in order: `fmt-check`, `clippy`, `test`, `swap-test`,
`compat-supervisor-test`, `doctest`, `examples`, `fixture-root`, `docs`,
`doc-blocks`, `parity-claims`, `format-coverage-check`, `features`, `deny`,
`msrv`, `package`.
Clippy runs with `-D warnings`, `docs` with `RUSTDOCFLAGS='-D warnings'`, and
every cargo invocation passes `--locked`, so a change that moves `Cargo.lock`
fails until the lockfile is committed.

`compat-supervisor-test` exercises the Linux pidfd containment used by the
slow compatibility lane. It skips on other Unix targets; CI runs it on Linux.

Four gates are **not** in `just check` and run only in CI:

| Gate | Why it is separate |
| --- | --- |
| `just api-check` | Needs nightly rustdoc JSON; the gate stays on stable |
| `just example-coverage-check` | Same, and rides the same nightly build |
| `just compat` | Builds five tmux releases from source; 90 minutes |
| `just fuzz <target>` | Needs nightly and a sanitizer; runs weekly |

CI also runs the suite on macOS, but only on `master` or manual dispatch: a
macOS runner bills at ten times a Linux one and the lints are
platform-independent. On a pull request, `tests on macOS` and `fuzz parsers`
report as skipping. That is the design, not a failure.

`just parity-claims` fails when a row of `parity.md` marked `implemented` or
`verified` names a symbol no crate defines. That document is what this project
calls its definition of done, so a done row naming a type nobody wrote leaves a
reader unable to tell a delivered capability from a described one. A row that
deliberately names something absent -- a Python symbol, or a capability it
explains is staying out -- is listed in the script with its reason.

`just examples` runs every shipped example against a server it owns and
fails when one exits nonzero or leaves a socket in `/tmp/libtmux-rs-dev/`.
`cargo test --all-targets` compiles an example and never runs it, so before
this an example that failed on every run passed every gate. It points `$TMUX`
at its own socket, which is how `inspect` and `find` find a server without
reaching the one the reader is using.

The gates worth explaining:

**Doctests must actually run.** A `#[cfg(feature = "…")]` inside a doctest
reads the doctest's own crate, which has no features, so it is always false:
the example compiles to nothing and passes vacuously. Gate the doc attribute
instead — `#![cfg_attr(feature = "x", doc = "…")]`. When adding a doctest for
gated API, break it once and confirm it fails.

**The public surface is recorded.** `crates/libtmux/docs/public-api.txt` lists
every public item. `just api` regenerates it; `just api-check` fails when the
tree disagrees. Adding or changing public API means committing the regenerated
file, and that diff is the review artefact. There is no semver gate while the
crates are prerelease: `cargo-semver-checks` treats a prerelease-to-prerelease
step as a major change and skips every lint, reporting `0 checks: 0 pass, 254
skip` and then `no semver update required`, which reads like a clean bill of
health and is not one. The changelog is the record until the first
non-prerelease version.

**Every crate-root type has a runnable example.**
`just example-coverage-check` fails on a type added without one. It reads
rustdoc's JSON for `libtmux` and takes every `struct`, `enum`, `trait`, and
`type_alias` reachable as `libtmux::X`, following the root's re-exports; a type
counts as covered when its own documentation carries a fenced block. Methods do
not count, and a trivial accessor inherits the example on its type. The escape
hatch is the one the failure names: add an example, or make the type private if
it is not part of the surface.

Write it against a real `TestServer` where behavior is the point: an example
that runs is the only kind that catches a wrong belief about tmux, which is
what they keep catching.

**The fixture root has to be empty afterwards.** `just fixture-root` lists
anything left in `/tmp/libtmux-rs-test/` and fails. It runs after `doctest`
because a doctest is the easy place to leak one: reaching for a hand-named
socket path looks reasonable until you notice tmux never removes it. A fixture
whose owning process is still running is skipped, so a suite in flight
elsewhere does not fail somebody else's gate.

**Doc comments are checked for splits.** `just doc-blocks` fails when a doc
comment opens mid-sentence or sits below a non-doc attribute — the shape a
split leaves when it lands on a sentence boundary. See
[`WRITING.md`](WRITING.md) for the rule this enforces.

**The format catalog is measured against tmux's own source.**
`crates/libtmux/docs/format-coverage.txt` records every format name tmux
publishes and what this crate does about it: `catalogued`, `missing: <scope>`,
or `excluded: <reason>`. The catalog itself is the table in
`crates/libtmux/src/formats.rs`.

`just format-coverage-check` compares the ledger against that catalog, not
against tmux, which is why it needs no tmux source and can sit in the gate. It
fails on three disagreements: a name in the catalog and absent from the ledger,
a name recorded `catalogued` but absent from the catalog, and a name recorded
`missing` that has since been added. Adding a format is therefore two steps —
the catalog entry, then the ledger — and the gate fails in between.

The pinned tmux 3.7b compatibility lane also compares every recorded status
with that tag's source before it builds tmux. Added, removed, or reclassified
upstream formats therefore fail the source-backed gate.

Rerecording does need a tmux checkout, because it reads `format.c` for both
`format_table[]` and the names attached by `format_add` and `cmdq_add_format`:

```console
$ just format-coverage ../tmux
```

The scope on a `missing` row is inferred from which member of the format tree
the callback reads, so it is a guess. Reviewed exceptions live in the script,
not in its generated output, and survive rerecording. A `missing` row is a
field a listing could carry and does not; adding one is ordinary work, leaving
it unrecorded is not.

**Parsers that read from outside are fuzzed.** The control-mode line parser,
the filter-expression wire format, and the workspace YAML loader each have a
target under `fuzz/`. Add one when adding a parser that reads bytes this crate
did not write, and seed it — an unseeded target proves only that arbitrary
input is not valid input.

**Packaging is a gate.** `just package` builds the published crates and
verifies what the tarballs contain. A packaged crate ships its README, so a
README telling a reader to depend on a version that is no longer current is
shipped install instructions, and this is what catches it.

## The language floor

MSRV is **1.85** for the libraries, Edition 2024's first compiler, and
**1.88** for `tmux-mcp`, which cannot meet the lower floor because `rmcp` and
`darling` do not. Two floors because they are two promises. A raise is a minor
bump, and is a compatibility entry in the changelog naming both versions.

The floor is stated in several places, and they have to agree:

- `[workspace.package] rust-version` in `Cargo.toml`
- `crates/tmux-mcp/Cargo.toml`, which sets its own
- the `msrv` recipe in the justfile, which runs each floor by exact name
- the toolchain installs in `.github/workflows/ci.yml`
- the MSRV badge and the Requirements section in `README.md`

`just msrv` runs the library tests on 1.85.0, then `cargo hack check
--each-feature` on 1.85.0 so no single feature raises the floor on its own,
then the `tmux-mcp` tests on 1.88.0. `rust-toolchain.toml` pins a much newer
toolchain for day-to-day work; it is not the floor and does not prove one.

## Dependencies

- A declared version is the **minimum supported**, not the newest published,
  and never one whose own `rust-version` exceeds our floor.
- Shared versions live in `[workspace.dependencies]`. `libtmux-macros` is
  pinned exactly, because the derive expands to paths into a hidden surface
  that only the matching version provides.
- `cargo deny check` gates licences, advisories, and sources: seven licences
  are allowed, yanked crates are denied, wildcard requirements are denied
  outside the workspace's own path dependencies, and unknown registries and
  git sources are denied.
- `libtmux` must not require proc macros. Its own `Filterable` impls are
  hand-written; the derive is for downstream structs only.

## API conventions

The reasoning behind these is in
[`crates/libtmux/docs/design.md`](../crates/libtmux/docs/design.md); what
follows is the rule.

- **Handles and snapshots keep private fields behind accessors.** The two
  shapes `Server::hierarchy` returns are `#[non_exhaustive]` instead, because
  callers read their fields directly. Enums that model something tmux owns are
  `#[non_exhaustive]`; enums whose variants are complete by construction are
  not.
- **tmux emits bytes, not text.** Names, titles, and pane output can be
  invalid UTF-8, so they cross the API as `TmuxText`. Reading a tmux stream
  with anything that requires UTF-8 fails the whole operation the first time a
  pane prints a high byte.
- **Listings come in pairs, and the short name is the loud one.** `sessions()`
  returns `Result`; `sessions_or_empty()` collapses failure into no rows. Both
  halves are load-bearing, and which one gets the short name is the point: a
  caller who writes the obvious thing gets the error, and a caller who wants
  an empty list on failure has to say so. Add both when adding a listing.
- **A failure says what to do about it.** `Error::kind` reduces the variants
  to a decision, and `is_object_gone` is the branch most callers write. tmux
  reports a missing target and a bad argument with the same exit status, so
  the classification reads stderr, and `real_tmux_compat_error_…` pins that
  wording against every supported release.
- `unsafe_code` is `forbid` at the workspace level. `unwrap`, `expect`, and
  `panic` are denied outside tests.

## Pull requests

Keep a change scoped to what was asked for; unrelated cleanup goes in its own
commit or its own pull request. Make the smallest coherent change that solves
the verified problem, and reuse an existing helper, type, or test before
adding one.

Run `just check` before pushing. A pull request that changes public API
carries the regenerated `public-api.txt`; one that changes behavior carries a
changelog entry under `## Unreleased`; one that adds a gate carries the proof
that the gate can fail.

Commit format is in [`WRITING.md`](WRITING.md).

## Review

The diff of `public-api.txt` is the review artefact for an API change, and the
changelog entry is the review artefact for a behavioral one. A reviewer reads
both before the implementation.

Two questions a reviewer is expected to ask: has this test been shown to fail,
and does the comment that came with it survive the gates in
[`WRITING.md`](WRITING.md).

## Releases

Publishing is done by `.github/workflows/release.yml`, on a
`<crate>@v<version>` tag. There is no API token anywhere: the workflow mints a
short-lived one from crates.io by exchanging a GitHub OIDC identity token
through `rust-lang/crates-io-auth-action`. Nothing in the repository can
publish without such a tag, because the `release` environment only accepts
refs matching `*@v*`, and crates.io checks the environment as well as the
workflow file.

**One tag names one crate.** The crates do not share a version — `tmux-mcp`
moves at its own pace — so a workspace-wide tag could not say what was being
released. The tag also orders the work: publishing a crate before the
`libtmux` it requires simply fails, because cargo resolves that dependency
from the registry rather than from the tree. Tags go out leaves-first, and
each waits for the previous to appear on crates.io.

To cut a release:

1. Bump `[workspace.package] version` and the `libtmux` pin in
   `[workspace.dependencies]` together — they are the same number, and a
   published crate whose own dependency requirement points at the previous
   version will not resolve. `tmux-mcp` carries its own version and is bumped
   separately.
2. Bump the version every README tells a reader to depend on. The packaged
   crate ships its README, so a stale one is shipped install instructions;
   `just package` fails on it.
3. `cargo update --workspace`, which moves the four members in `Cargo.lock`
   and nothing else. `just check` passes `--locked` and fails without it.
4. Move the changelog's `Unreleased` entries under a dated heading.
5. `just check`.
6. Push one tag per crate, in dependency order, waiting for each to land:
   `libtmux-macros@vX.Y.Z`, then `libtmux@vX.Y.Z`, then
   `tmux-workspace@vX.Y.Z` and `tmux-mcp@vA.B.C`. `tmux-workspace` inherits
   the workspace version, so skipping it leaves it pinned to a `libtmux` that
   is no longer current.

The workflow checks that the version in the tag is the version in the
manifest, then runs the whole gate on that exact commit, packages the crate,
attests it, publishes, and attaches the `.crate` to a GitHub release. It runs
the gate rather than trusting the tag because a crates.io version is
immutable: it is the one build that cannot be taken back.

**Provenance is attached to the release, not to the registry.** crates.io does
not host or display build attestations yet, and `cargo` does not verify them,
so `actions/attest` signs the packaged `.crate` files and the attestation
lives with the GitHub release. It is Sigstore-signed and logged in Rekor, and
it covers the same bytes crates.io serves, because `cargo package` is
byte-deterministic for a given tree — measured rather than assumed.

**One step is not in this repository.** Each published crate has to name this
workflow as a trusted publisher, once, in the crates.io UI under the crate's
Settings:

| Field | Value |
| --- | --- |
| Repository owner | `libtmux` |
| Repository name | `libtmux-rs` |
| Workflow filename | `release.yml` |
| Environment | `release` |

A crate has to have been published at least once by hand before it can be
configured, which all four have been.

## Compatibility

Every supported tmux release is built from source in CI and runs the whole
workspace, because tmux's own output and error wording change between
releases. The lane covers **3.2a, 3.4, 3.5a, 3.6b, and 3.7b** — the final
patch of each series, which is what a distribution ships and what a user runs;
3.4 is the exception because that series has no later patch.

Where a release is wrong rather than merely old, the crate says so rather than
working around it silently: `run_shell` refuses on 3.3 through 3.4, which drop
the command's output instead of returning it.

Run the lane locally with `just compat`, which builds each release from
source. It is slow, and it is the only thing that catches a version-specific
break before CI does.

While the version is `0.1.0-alpha.*` the API is not settled and any release
may change or remove exported identifiers without a deprecation period. That
is stated in `README.md` and `CHANGELOG.md` in the wording every libtmux port
uses; keep the three in agreement.
