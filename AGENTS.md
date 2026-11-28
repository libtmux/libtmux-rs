# AGENTS.md

Rules for this repository: `libtmux` for Rust, a port of the Python library of
the same name. Nothing here is Python except the support scripts under
`scripts/` and the pseudo-terminal fixtures of the `tmux-workspace` tests, which
use uv, ruff, ty and pytest as [`.github/SCRIPTS.md`](.github/SCRIPTS.md) says.
Any other convention you recognise from that project (mypy, for one) does not
apply unless a file here says so.

Follow the conventions already in the tree, and keep a change scoped to what
was asked for.

## What is here

One Cargo workspace with seven publishable crates and two private packages:

| Crate | What it is |
| --- | --- |
| `crates/libtmux-core` | Pure operations, identities, plans, queries and reply decoding |
| `crates/libtmux-blocking` | The native blocking tmux client |
| `crates/libtmux-tokio` | The Tokio tmux client and object model |
| `crates/libtmux` | The shared domain with optional blocking and Tokio clients |
| `crates/libtmux-macros` | `#[derive(Filterable)]`, for downstream structs |
| `crates/tmux-mcp` | A Model Context Protocol server over tmux |
| `crates/tmux-workspace` | A tmuxp-style YAML builder |
| `tools/mcp-swap` | A private agent-client configuration switcher (`publish = false`) |
| `tools/readme-tests` | Executes the Rust examples in the repository README |

`tmux-mcp` carries its own `version` and `rust-version` rather than inheriting
the workspace's, because it moves at its own pace and its dependencies need a
newer compiler than the libraries promise. `fuzz/` is excluded from the
workspace: it needs nightly and a sanitizer.

`just` lists every recipe. Cargo is authoritative; the justfile only groups
Cargo commands.

## Which policy applies

- For changes to documentation or any user-facing text — `README.md`,
  `CHANGELOG.md` (the root file and the one in each crate), release notes,
  commit messages, CLI and help text, error messages, rustdoc, or source
  comments — follow
  [`.github/WRITING.md`](.github/WRITING.md).
- Support scripts: what may stay, their language, layout and checks:
  [`.github/SCRIPTS.md`](.github/SCRIPTS.md).
- For building, testing, the gates a change must pass, the language floor,
  dependencies, releases, or opening a pull request, follow
  [`.github/CONTRIBUTING.md`](.github/CONTRIBUTING.md).

Each is the single home for its subject. Where a rule appears to be stated
twice, the file named above governs.

## Change discipline

- Make the smallest coherent change that solves the verified problem, and keep
  unrelated cleanup out of it.
- Reuse an existing helper, type, or test before adding one.
- `tmux-mcp` and `tmux-workspace` exist to use `libtmux` from outside it. When
  either needs a workaround, that is a finding about `libtmux`, not about them.
- A passing gate is evidence only once it has been shown capable of failing.
  Pair a new test with a deliberate break that proves it bites.
- tmux sockets live under `/tmp/libtmux-rs-test/` for fixtures and
  `/tmp/libtmux-rs-dev/` for hand spikes, and never outside them. More than one
  libtmux runs on a developer's machine, and a socket collision reads as a bug
  in whatever ran next.

## References

- [design.md](crates/libtmux/docs/design.md) — why the crate is shaped this
  way: shared domain, native clients, creation receipts and daemon identity
- [Tokio design.md](crates/libtmux-tokio/docs/design.md) — the snapshot and
  format boundary, query grammar, control mode and test architecture
- [findings.md](crates/libtmux-tokio/docs/findings.md) — every tmux defect and
  platform surprise worked around, and what each gate turned up
- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) — the
  documentation baseline `.github/WRITING.md` builds on
- [Python libtmux](https://libtmux.git-pull.com/) — the library this ports
- [tmux(1)](http://man.openbsd.org/OpenBSD-current/man1/tmux.1) — the manual
