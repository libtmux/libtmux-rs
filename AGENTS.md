# AGENTS.md — the Rust workspace

Rules for this workspace, which is the whole repository. It was extracted from
the Python libtmux repository, and the commit format, changelog conventions,
slop prevention, and shipped-versus-branch-internal rule it inherited from that
repository's root `AGENTS.md` still apply. **Its commands do not**: nothing
here uses `uv`, `pytest`, `ruff`, or `mypy`.

## Layout and commands

| Crate | Published | What it is |
| --- | --- | --- |
| `crates/libtmux` | yes | The async tmux client and object model |
| `crates/libtmux-macros` | yes | `#[derive(Filterable)]`, for downstream structs |
| `crates/tmux-mcp` | yes | An MCP server, built to exercise the public API |
| `crates/tmux-workspace` | yes | A tmuxp-YAML builder, same purpose |

`just` lists every recipe. `just check` runs what CI runs. Cargo is
authoritative; the justfile only groups Cargo commands.

`tmux-mcp` carries its own `version` and `rust-version` rather than inheriting
them, because it moves at its own pace and its dependencies need a compiler the
libraries do not. Every crate here is published.

The last two exist to use the API from outside. When they need a workaround,
that is a finding about the API, not about them, and being genuinely published
is part of the job: a crate built only inside its own workspace never proves
its dependency requirements resolve. `tmux-workspace` could not be published
at all until `libtmux` shipped the `plan` feature it asks for, which is not
visible from inside the tree.

## Things that will bite

**Doctests must actually run.** A `#[cfg(feature = "…")]` inside a doctest
reads the doctest's own crate, which has no features, so it is always false:
the example compiles to nothing and passes vacuously. Gate the doc attribute
instead — `#![cfg_attr(feature = "x", doc = "…")]`. When adding a doctest for
gated API, break it once and confirm it fails.

**Tests run against real tmux.** `libtmux::test::TestServer` gives an isolated
socket and deterministic cleanup. There are no mocks; a test that would need
one is usually asking for a design change. Tests are functions, not methods on
a struct.

**This workspace owns `/tmp/libtmux-rs-test/` and nothing outside it.** More
than one libtmux lives on a developer's machine, and the Python suites put
their sockets in `/tmp` too. Sharing the root makes "whose leftover is this"
unanswerable, and a stray reaper or a socket collision then reads as a bug in
whatever ran next: one such session ended with 3,023 of 4,096 pseudo-terminals
held and `fork` failing with `No space left on device` in an unrelated build.

- Fixtures go under `/tmp/libtmux-rs-test/`. `TestServer` puts them there;
  do not pass a socket path outside it.
- Throwaway sockets for hand spikes go under `/tmp/libtmux-rs-dev/`, so they
  are as obviously ours as the fixtures are, and can be swept without
  thinking.
- `reap_abandoned_servers` only ever looks inside the fixture root, so it
  cannot reach another workspace's server however abandoned that server
  looks. Keep it that way.
- Socket paths are bounded by `sun_path` at about 108 bytes, which is why the
  root is short. A scratch directory deep under `$TMPDIR` will fail to bind
  with "File name too long".

**tmux emits bytes, not text.** Names, titles, and pane output can be invalid
UTF-8, so they cross the API as `TmuxText`. Reading a tmux stream with
anything that requires UTF-8 fails the whole operation the first time a pane
prints a high byte.

**Listings come in pairs, and the short name is the loud one.** `sessions()`
returns `Result`; `sessions_or_empty()` collapses failure into no rows. Both
halves are load-bearing, and which one gets the short name is the whole point:
a caller who writes the obvious thing gets the error, and a caller who wants
an empty list on failure has to say so. Add both when adding a listing.

**A failure says what to do about it.** `Error::kind` reduces the variants to
a decision, and `is_object_gone` is the branch most callers write. tmux
reports a missing target and a bad argument with the same exit status, so the
classification reads stderr; `real_tmux_compat_error_…` pins that wording
against every supported release.

## Conventions to keep

- Handles and snapshots keep private fields behind accessors. The two shapes
  `Server::hierarchy` returns are `#[non_exhaustive]` instead, because callers
  read their fields directly. Enums that model something tmux owns are
  `#[non_exhaustive]`; enums whose variants are complete by construction are
  not.
- `libtmux` must not require proc macros. Its own `Filterable` impls are
  hand-written; the derive is for downstream structs only.
- A declared dependency version is the minimum supported, not the newest
  published, and never one whose own `rust-version` exceeds the MSRV. Shared
  versions live in `[workspace.dependencies]`.
- MSRV is 1.85, Edition 2024's first compiler. A raise is a minor bump.
- Every supported tmux release builds from source in CI and runs the whole
  workspace. Version-specific behaviour belongs in a `real_tmux_compat_` test,
  not in a comment.

**The public surface is recorded.** `crates/libtmux/docs/public-api.txt`
lists every public item. `just api` regenerates it; `just api-check` fails
when the tree disagrees. Adding or changing public API means committing the
regenerated file, and the diff is the review artefact -- there is no semver
gate while the crates are prerelease.

**Every crate-root type has a runnable example, and that is enforced.**
`just example-coverage-check` fails on a type added without one. Write the
example against a real `TestServer` where behaviour is the point; three
examples added this way failed on first run and each had encoded a wrong
belief about tmux, so the verification is the value, not the prose.

**Doc comments are checked for splits.** `just doc-blocks` fails when a doc
comment opens mid-sentence. rustdoc treats a doc comment as an attribute and
never checks that the prose describes the item it precedes, so a block
inserted one line too high silently hands a type its neighbour's summary --
which has shipped twice. Never insert a doc block without landing above the
whole of the previous item's comment.

**Parsers that read from outside are fuzzed.** The control-mode line parser,
the filter-expression wire format, and the workspace YAML loader each have a
target under `fuzz/`, reached with `just fuzz <target>`. It is not a workspace
member and not part of `just check`: it needs nightly and a sanitizer, and the
gate has to stay runnable on stable. Add a target when adding a parser that
reads bytes this crate did not write, and seed it -- an unseeded target proves
only that arbitrary input is not valid input.

## Releasing

Publishing is done by `.github/workflows/release.yml`, on a
`<crate>@v<version>` tag. There is no API token anywhere: the workflow mints a
short-lived one from crates.io by exchanging a GitHub OIDC identity token,
through `rust-lang/crates-io-auth-action`. Nothing in the repository can
publish without such a tag, because the `release` environment only accepts refs
matching `*@v*`, and crates.io checks the environment as well as the workflow
file.

**One tag names one crate.** The crates do not share a version -- `tmux-mcp`
moves at its own pace -- so a workspace-wide tag could not say what was being
released. The tag also orders the work: publishing a crate before the `libtmux`
it requires simply fails, because cargo resolves that dependency from the
registry rather than from the tree. Tags therefore go out leaves-first, and
each waits for the previous to appear on crates.io.

To cut a release:

1. Bump `[workspace.package] version` and the `libtmux` pin in
   `[workspace.dependencies]` together -- they are the same number, and a
   published crate whose own dependency requirement points at the previous
   version will not resolve. `tmux-mcp` carries its own version and is bumped
   separately.
2. Move the changelog's `Unreleased` entries under a dated heading.
3. `just check`.
4. Push one tag per crate, in dependency order, waiting for each to land:
   `libtmux-macros@vX.Y.Z`, then `libtmux@vX.Y.Z`, then `tmux-mcp@vA.B.C`.

The workflow checks that the version in the tag is the version in the
manifest, then runs the whole gate on that exact commit, packages the crate,
attests it, publishes, and attaches the `.crate` to a GitHub release. It runs
the gate rather than trusting the tag because a crates.io version is
immutable: it is the one build that cannot be taken back.

**Provenance is attached to the release, not to the registry.** crates.io does
not host or display build attestations yet, and `cargo` does not verify them,
so `actions/attest` signs the packaged `.crate` files and the attestation
lives with the GitHub release. It is real -- Sigstore-signed and logged in
Rekor -- and it covers the same bytes crates.io serves, because `cargo
package` is byte-deterministic for a given tree, which was measured rather
than assumed.

**There is no `semver` recipe, on purpose.** `cargo-semver-checks` cannot say
anything useful here: it treats a prerelease-to-prerelease step as a major
change and skips every lint, so a run reports `0 checks: 0 pass, 254 skip` and
then `no semver update required`. That reads like a clean bill of health and
is not one. The changelog is the record of what changed until the first
non-prerelease version, at which point the recipe is worth adding back.

**One step is not in this repository.** Each published crate has to name this
workflow as a trusted publisher, once, in the crates.io UI under the crate's
Settings. The values are:

| Field | Value |
| --- | --- |
| Repository owner | `libtmux` |
| Repository name | `libtmux-rs` |
| Workflow filename | `release.yml` |
| Environment | `release` |

A crate has to have been published at least once by hand before it can be
configured, which all three have been.

## Where the reasoning lives

`crates/libtmux/docs/design.md` carries the rationale: transport, snapshot and
format boundary, query grammar, control mode, test architecture, compatibility
lanes, and the lint and dependency gates. `crates/libtmux/docs/parity.md` is
the capability ledger against Python libtmux.
