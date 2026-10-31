# Contributing

This repository is a Cargo workspace of four published crates and one private
developer tool. The gates below are what a change has to pass. It was
extracted from the Python libtmux repository; its `uv`, `pytest`, `ruff`, and
`mypy` conventions do not apply here.

For how the prose reads — README, changelog, release notes, commit messages,
rustdoc, and source comments — see [`WRITING.md`](WRITING.md).

## Getting set up

You need tmux 3.3a or newer on `PATH` and a Unix target. Native Windows is
unsupported because tmux is unavailable there; WSL works.
The CLI's captured-child support has a narrower binding boundary; see
[Bootstrap scripts](../crates/tmux-workspace/docs/cli.md#bootstrap-scripts).

`rust-toolchain.toml` pins the toolchain, so rustup installs it on the first
cargo command. The gate also needs the two MSRV floors and three Cargo tools:

```console
$ rustup toolchain install 1.85.0 1.88.0
```

```console
$ cargo install just cargo-hack cargo-deny
```

Nightly is needed only for `just api-check`, `just example-coverage-check`,
and `just fuzz`, none of which are part of the local gate.

`just` with no argument lists every recipe. Cargo is authoritative; the
justfile only groups Cargo commands.

## Building

```console
$ cargo build --workspace --all-features
```

Five workspace packages; the four library/server crates are published:

| Crate | What it is |
| --- | --- |
| `crates/libtmux` | The async tmux client and object model |
| `crates/libtmux-macros` | `#[derive(Filterable)]`, for downstream structs |
| `crates/tmux-mcp` | A Model Context Protocol server over tmux |
| `crates/tmux-workspace` | A tmuxp-style YAML builder |
| `tools/mcp-swap` | A private agent-client configuration switcher (`publish = false`) |

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

Five pages are compiled along with them, so a Rust example on any of these
pages is a test rather than prose:

- `crates/libtmux/README.md`, by `#![doc = include_str!(…)]` at the top of
  `crates/libtmux/src/lib.rs`: that README is the crate documentation.
- `README.md`, by a `#[cfg(doctest)] #[doc = include_str!(…)]` item further
  down the same file, so the front page is tested without appearing in the
  rendered documentation.
- `crates/tmux-workspace/docs/library.md`, by the same top-of-crate include
  in `crates/tmux-workspace/src/lib.rs`: that page is the library's
  documentation.
- `crates/tmux-workspace/README.md`, by `Readme` in the same file. It is the
  command's page on crates.io, so it is compiled without being rendered.
- `crates/libtmux-macros/README.md`, by `MacrosReadme` in
  `crates/tmux-workspace/src/lib.rs`, reached through a symlink. A proc-macro
  crate cannot doctest a README that derives through it, and the example needs
  `libtmux` under its own name, which is only true from a third crate.
  `crates/libtmux/tests/macros_readme.rs` compiles the same example again as
  an ordinary test.

`crates/tmux-mcp/README.md` has no Rust to compile. Its commands and its JSON
client configurations are checked instead by `crates/tmux-mcp/tests/docs.rs`,
which passes each one, here and on the `libtmux` and `tmux-workspace` READMEs
and the repository README, through the binary's own argument and setting
validation. `every_documented_command_parses` does the same for
`tmux-workspace` commands on those pages, and
`crates/tmux-workspace/tests/readme.rs` loads the workspace YAML on the
`tmux-workspace` and repository READMEs.

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

### Nothing selects a server

A tmux command that names no socket follows an inherited `TMUX` to the server
its caller is inside, and otherwise lands on the default socket under
`TMUX_TMPDIR`. A suite started from a tmux pane could drive, or `kill-server`,
the session it was started from.

`.cargo/config.toml` therefore names a runner, `scripts/sandbox-runner.sh`,
which Cargo puts in front of every test, example and bench binary it starts.
Before the binary runs, the runner:

- exits 78, naming the variable, while `LIBTMUX_SOCKET` or
  `LIBTMUX_SOCKET_PATH` holds a value. `tmux-mcp` reads an empty value as an
  invalid socket, not as unset, so nothing empties them yet; the change that
  makes an empty value mean unset also makes `[env]` empty them, and the
  refusal goes with it;
- unsets `TMUX` and `TMUX_PANE`;
- makes a fresh owner-only directory with `mktemp -d` under `TMPDIR`, exits 78
  when `TMPDIR` is so long that the default socket path would pass 100 bytes
  (`sun_path` holds 104 on macOS and 108 on Linux), and sets `TMUX_TMPDIR` and
  `LIBTMUX_TEST_SANDBOX` to it.

After the binary exits, the runner searches the whole directory for sockets,
without following links, since a test can start a server with `-S` on a path of
its own. A socket file nothing answers on is not a server, because tmux never
unlinks one. It stops each server that answers with `tmux -S <path>
kill-server`, then checks that a connection fails, retrying for about three
seconds. A server that still answers keeps the directory, and so does a
directory that could not be searched; otherwise the directory is removed. The
runner exits with the binary's own status, or with 70 when the binary passed
and left a server: a test that starts a bare `tmux new-session -d` fails the run
that started it and leaves nothing for the next one. Its messages give socket
paths relative to the directory.

`[env]` in the same file covers what the runner does not wrap. It sets `TMUX`
and `TMUX_PANE` to empty, and `TMUX_TMPDIR` and `LIBTMUX_TEST_SANDBOX` to
`.cargo/tmux-sandbox`, a directory of the checkout. Cargo 1.97.1 runs doctests
through the runner; 1.85.0 and 1.88.0 do not (measured), so on those toolchains
a doctest gets `[env]` and no sweep.

- `[env]` empties the variables rather than removing them because it cannot
  unset one. The `libtmux` core, `tmux-workspace` and `tmux-mcp` read an empty
  `TMUX` as unset. The `from_env` constructors fail on an empty variable as
  they do on an unset one, with a different error.
- The fallback directory is committed because tmux ignores a `TMUX_TMPDIR` that
  does not exist and falls back to `/tmp` (measured on 3.8a), which is the
  default socket this exists to avoid. Nothing is meant to listen in it.
- `cargo run` gets the runner and `[env]` when its working directory is inside
  the checkout: Cargo reads `.cargo/config.toml` from the working directory,
  not from `--manifest-path`. To reach a real server on purpose, run the built
  binary. `LIBTMUX_SANDBOX_KEEP_TMUX=1` makes the runner keep the `TMUX` it
  receives, and `[env]` still empties it unless a `--config` replaces it, as
  `scripts/run-examples.sh` does for the server it owns:
  `cargo run --config 'env.TMUX.value="<TMUX>"' --config 'env.TMUX.force=true'`.
- A test binary started by hand has not been through Cargo, so none of this
  applies to it. The canary and the `tmux-mcp` binary tests refuse to run
  there.

`crates/libtmux/tests/sandbox_canary.rs` asserts that the runner applied: `TMUX`
and `TMUX_PANE` are unset, and `TMUX_TMPDIR` is a directory of this user's that
is not the committed fallback. It resolves the default socket under the sandbox
and fails if a server is listening on it; a leftover socket file is not one,
since tmux never unlinks its socket. It starts a bare tmux client, with no `-L`
or `-S`, and requires the server to listen on that default socket; a second run
points the client elsewhere and requires the same assertion to fail. It then
runs itself again in the environment of a tmux pane, where it must fail. A
failure names variables and never their values.
`crates/libtmux/tests/sandbox_runner.rs` runs the runner around small commands
and has a test for each of its behaviours, among them the leaked server above.

The fallback directory takes the checkout's umask, because git records no
directory modes; the runner's is owner-only. What tmux keeps private is
`tmux-<uid>` inside either, which tmux creates 0700 and refuses to use when
others can reach it (measured on 3.8a); the canary checks that one when it
exists, and that the default socket path fits in `sun_path`. The canary, the
runner tests and `crates/tmux-mcp/tests/binary.rs` are not packaged: a
published crate has no `.cargo/config.toml` and no runner.

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
  `Pane::wait_for_quiet` are in the library and need no feature;
  `tmux-mcp` exposes `wait_for_text` for a concrete text condition. A wait must
  assert the outcome it got, since one that runs to its deadline still returns
  successfully.

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
`compat-supervisor-test`, `ci-test`, `doctest`, `api-examples`, `script-env`,
`examples`, `example-tables`, `fixture-root`, `docs`, `doc-blocks`,
`doc-contents`, `changelogs`, `doctests-run`, `parity-claims`,
`format-coverage-check`, `features`, `deny`, `msrv`, `package`.
Clippy runs with `-D warnings`, `docs` with `RUSTDOCFLAGS='-D warnings'`, and
every cargo invocation passes `--locked`, so dependency changes need a
consistent `Cargo.lock` before the gate can run. Committing it is not required
to run the local gate.

`compat-supervisor-test` exercises the Linux pidfd containment used by the
slow compatibility lane. It skips on other Unix targets; CI runs it on Linux.

These gates are **not** in `just check` and run only in CI:

| Gate | Why it is separate |
| --- | --- |
| `just api-check` | Needs nightly rustdoc JSON; the gate stays on stable |
| `just example-coverage-check` | Same, and rides the same nightly build |
| `just compat` | Builds every pinned tmux release from source; 90 minutes |
| `just fuzz <target>` | Needs nightly and a sanitizer; runs weekly |
| `just bench`, `just bench-waits`, `just bench-compare` | Timing on a shared runner; runs weekly against the previous week, failing past 2x |
| `just parity`, `just bench-cli` | Need tmuxp 1.74.0; load every tmuxp fixture through tmuxp and `tmux-workspace` and diff the sessions, then time both; run weekly |

`just parity` needs a pinned tmuxp 1.74.0 on `--tmuxp` (or on `PATH`), not
just tmux, so it runs weekly in CI rather than in `just check`. It loads every `crates/tmux-workspace/tests/fixtures/tmuxp` document
with tmuxp and with `tmux-workspace load`, each on its own throwaway server,
and diffs what tmux itself reports. A documented, deliberate difference from
tmuxp -- see `scripts/tmuxp-parity.py`'s `ALLOWLIST` -- is reported but does
not fail the run; anything else that differs does.

CI does not run macOS tests. macOS is supported on a best-effort basis; the
release workflow builds the macOS binaries. On a pull request, `fuzz parsers`,
`benchmarks` and `tmuxp parity` report as skipping. That is the design, not
a failure.

`just parity-claims` fails when a row of `parity.md` marked `implemented` or
`verified` names no caller-reachable Rust path, or puts an associated item on
the wrong owner. Paths are checked against `public-api.txt`; private details do
not satisfy the gate. `just api-check` proves the index still matches rustdoc
JSON in CI.

`just examples` runs every shipped example against a server it owns and
fails when one exits nonzero or leaves a socket in `/tmp/libtmux-rs-dev/`.
`cargo test --all-targets` compiles an example and never runs it, so before
this an example that failed on every run passed every gate. It points `$TMUX`
at its own socket, which is how `inspect` and `find` find a server without
reaching the one the reader is using.

`just api-examples` checks complete API programs and their setup files against
the native public API inventory. It rejects missing programs, duplicate or
unknown targets, hidden source helpers and incomplete file boundaries.
`just examples` executes these programs along with the other shipped examples;
the manifest check alone does not establish native execution.

The gates worth explaining:

**The scripts that run a binary outside Cargo scrub their own environment.**
`just script-env` fails when `scripts/tmuxp-parity.py` or
`scripts/bench-workspace-cli.py` hands a child `TMUX`, `TMUX_PANE`,
`LIBTMUX_SOCKET` or `LIBTMUX_SOCKET_PATH`, or when the benchmark's private
`TMUX_TMPDIR` is not owner-only. No runner stands in front of them.

**Doctests must actually run.** A `#[cfg(feature = "…")]` inside a doctest
reads the doctest's own crate, which has no features, so it is always false:
the example compiles to nothing and passes vacuously. Gate the doc attribute
instead — `#![cfg_attr(feature = "x", doc = "…")]`. When adding a doctest for
gated API, break it once and confirm it fails.

**The public surface is recorded.** `crates/libtmux/docs/public-api.txt` lists
every public item with its signature and each non-blanket trait
implementation. `just api` regenerates it; `just api-check` fails when the tree
disagrees. Adding or changing public API means committing the regenerated
file, and that diff is the review artefact. A semver gate applies from `0.1.0`
on. Before it, every version was a prerelease, and `cargo-semver-checks` treats
a prerelease-to-prerelease step as a major change and skips every lint,
reporting `0 checks: 0 pass, 254 skip` and then `no semver update required`,
which reads like a clean bill of health and is not one. `0.1.0` is the first
non-prerelease version, so the tool has a release to compare against. No
recipe runs it yet. Because any `0.1.N` release may break the API, a break it
reports is not a failure; it has to be marked `**Breaking.**` in the
changelog.

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

**A Contents list matches its document.** `design.md`, `findings.md` and
`parity.md` open with one, and `just doc-contents` fails when it and the `##` sections
disagree in title, order or anchor. A section that indexes its `###`
subsections under its entry, as `design.md`'s longer ones do, is held to them
the same way.

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

**Parsers that read from outside are fuzzed.** `just fuzz-list` names a target
for each; `design.md` says what each checks. Add one when adding a parser that
reads bytes this workspace did not write, seed it from real output, and give it
an oracle where one exists — an unseeded target proves only that arbitrary
input is not valid input, and one without an oracle proves only that it does
not panic.

**Packaging is a gate.** `just package` builds the published crates and
verifies what the tarballs contain. A packaged crate ships its README, so a
README telling a reader to depend on a version that is no longer current is
shipped install instructions, and this is what catches it.

The gate verifies the workspace tarballs as one prospective release set. Cargo
resolves workspace dependencies from the packaged crates in a temporary
registry, not from their path entries. It does not assert that those versions
are already on crates.io; the release workflow packages one tagged crate at a
time, after its dependencies have been published.

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

The derive's compile-fail cases pin rustc's diagnostics, which change between
releases, so they run on the pinned toolchain only; the MSRV run checks the
passing cases. After a toolchain bump, `just macros-ui-bless` rewrites their
`.stderr` files; review the diff before committing it.

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
- **A listing propagates; nothing collapses a failure into empty rows for
  it.** `sessions()` returns `Result`, and a caller who wants an empty list on
  failure writes `sessions().await.unwrap_or_default()` at the call site. An
  `_or_empty` twin of each listing existed once; nothing called one, and a
  method whose whole purpose is to discard a reason is a way to discard one by
  accident, so they are gone.
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
changelog entry under `## Unreleased` in the crate it changes, or in the root
file when it spans crates ([`WRITING.md`](WRITING.md#changelog) says which);
one that adds a gate carries the proof
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

Releases are numbered `0.1.N` (`0.1.0`, `0.1.1`, and so on), with no
`-alpha` or other prerelease tag unless the maintainer asks for one. To cut a
release:

1. Bump `[workspace.package] version` and the `libtmux` and exact
   `libtmux-macros` requirements in `[workspace.dependencies]` together — they
   use the same number. A published crate whose dependency points at the previous
   version will not resolve. `tmux-mcp` carries its own version and is bumped
   separately.
2. Bump the version every README tells a reader to depend on. The packaged
   crate ships its README, so a stale one is shipped install instructions;
   `just package` fails on it.
3. `cargo update --workspace`, which moves the four members in `Cargo.lock`
   and nothing else. `just check` passes `--locked` and fails without it.
4. Move the `Unreleased` entries of the root changelog and of each crate's
   changelog under a dated heading, and open a fresh `## Unreleased` above it;
   a crate with nothing under the new heading says
   ``No entries for `<crate>`.``
   `just changelogs` fails when a crate has no changelog or no such heading,
   and `just package` when a crate does not ship it.
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

For `tmux-workspace` and `tmux-mcp` it then builds the command for Linux
(musl, x86_64 and aarch64) and macOS (arm64 and x86_64), attests the archives
and attaches them to the same release, through `binaries.yml`. `cargo
binstall` finds them by the `pkg-url` in each crate's
`[package.metadata.binstall]`, so the archive names and that template change
together. A pull request touching either manifest, the lockfile or the
workflow builds all eight without uploading.

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

Pinned compatibility releases are built from source in CI and run the whole
workspace, because tmux's own output and error wording change between
releases. The lanes cover **3.3a, 3.4, 3.5a, 3.6b, 3.7b, and 3.8** — the final
patch of each series, which is what a distribution ships and what a user runs;
3.4 is the exception because that series has no later patch. A tmux master
commit is pinned beside them.

Other lanes build **3.0a, 3.1c, 3.2a, and 3.3**, and check that each is
refused before any command reaches it. 3.0a and 3.1c run as the daemon
behind the 3.3a client as well as with their own client, because an old
daemon answers a current client differently from its own. **3.7** is built to
pin its known `break-pane` defect.

`.github/ci-matrix.txt` lists the lanes, each with the commit its tag must
name. A push or pull request builds the first and last supported release,
3.3a and 3.8. The weekly schedule and a dispatch with `scope=full` build every
lane. The `required` job is the one check that passes only when every planned
job did.

Where a release is wrong rather than merely old, the crate says so rather than
working around it silently: `run_shell` refuses on 3.3 through 3.4, which drop
the command's output instead of returning it.

Run the lanes locally with `just compat`, which builds each release from
source. It is slow, and it is the only thing that catches a version-specific
break before CI does. `LIBTMUX_COMPAT_LANES` picks lanes, and builds the
releases a lane needs first without testing them:

```console
$ LIBTMUX_COMPAT_LANES="3.3a 3.8" just compat
```

Before tagging a release, run every lane on the release commit and wait for
it to pass, since a green push no longer covers the middle of the table:

```console
$ gh workflow run ci.yml \
    --ref master \
    -f scope=full
```

### Running CI against any tmux

Dispatch the `CI` workflow on any branch to build tmux versions, tags,
branches or commits of github.com/tmux/tmux, and to run them with other Rust
toolchains:

```console
$ gh workflow run ci.yml \
    --ref my-branch \
    -f scope=custom \
    -f tmux-ref=a1b2c3d4e5f60718293a4b5c6d7e8f9012345678 \
    -f rust-toolchain=1.85.0 \
    -f tmate=on-failure
```

`tmux-ref` takes a comma-separated list, and `LIBTMUX_COMPAT_LANES` takes the
same names. A ref outside the manifest builds as a development build and runs
the whole workspace. The compile-fail cases quote the pinned toolchain's own
wording, so a toolchain other than `rust-toolchain.toml`'s fails them. `tmate`
opens a session on the one lane that runs, so it needs a single `tmux-ref` and
at most one `rust-toolchain`, and it connects only the account that started the
run. A dispatch runs the fuzz, benchmark and parity jobs as well, as before.

Releases are numbered `0.1.N`, and each release increments `N`. The API is not
settled: any release may change or remove exported identifiers, without a
deprecation period, and the changelog marks each break. That is stated in
`README.md` and the crate READMEs in the wording every libtmux port uses; keep
them in agreement with the sentence in [`WRITING.md`](WRITING.md).
