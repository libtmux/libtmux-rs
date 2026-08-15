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
| `crates/tmux-workspace` | no | A tmuxp-YAML builder, same purpose |

`just` lists every recipe. `just check` runs what CI runs. Cargo is
authoritative; the justfile only groups Cargo commands.

`tmux-mcp` carries its own `version` and `rust-version` rather than inheriting
them, because it moves at its own pace and its dependencies need a compiler the
libraries do not. Only `tmux-workspace` is unpublished, and `publish = false`
is what says so.

Both of those crates exist to use the API from outside. When they need a
workaround, that is a finding about the API, not about them.

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

**Listings come in pairs.** The plain form returns an empty `Vec` on failure;
the `try_` form propagates. Both halves are load-bearing — a `try_` form that
quietly returned no rows would make the pair meaningless. Add both when adding
a listing.

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

## Where the reasoning lives

`crates/libtmux/docs/design.md` carries the rationale: transport, snapshot and
format boundary, query grammar, control mode, test architecture, compatibility
lanes, and the lint and dependency gates. `crates/libtmux/docs/parity.md` is
the capability ledger against Python libtmux.
