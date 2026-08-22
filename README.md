# libtmux for Rust

[![CI]][actions] [![libtmux]][libtmux-crate] [![tmux-mcp]][mcp-crate] [![MSRV]][rust-1.85] [![License]][license]

[CI]: https://github.com/libtmux/libtmux-rs/actions/workflows/ci.yml/badge.svg
[actions]: https://github.com/libtmux/libtmux-rs/actions/workflows/ci.yml
[libtmux]: https://img.shields.io/crates/v/libtmux.svg?label=libtmux
[libtmux-crate]: https://crates.io/crates/libtmux
[tmux-mcp]: https://img.shields.io/crates/v/tmux-mcp.svg?label=tmux-mcp
[mcp-crate]: https://crates.io/crates/tmux-mcp
[MSRV]: https://img.shields.io/badge/rustc-1.85+-lightgray.svg
[rust-1.85]: https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/
[License]: https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg
[license]: #license

**Drive tmux from Rust: typed, async control over servers, sessions, windows,
and panes — and a query layer that makes "which pane is running the tests?" one
expression instead of a parsing problem.**

> **Alpha.** Releases carry an `-alpha` prerelease tag. The API is not
> settled, and any release may change or remove exported identifiers without a
> deprecation period. Pin an exact version. Not recommended for production.

You may be looking for:

- [API documentation](https://docs.rs/libtmux) — every public item, with a
  runnable example
- [The `libtmux` guide](crates/libtmux/README.md) — features, the three
  transport switches, testing
- [`tmux-mcp`](crates/tmux-mcp/README.md) — the MCP server, a **separate
  package**, if you want an agent to drive tmux
- [Design notes](crates/libtmux/docs/design.md) — why it is shaped this way
- [Parity ledger](crates/libtmux/docs/parity.md) — capability-by-capability
  against Python libtmux

## Is this for you?

| You want to | Use |
| --- | --- |
| Script tmux from Rust, async | [`libtmux`](crates/libtmux) |
| Ask "which pane/window/session matches X?" | [`libtmux`](crates/libtmux) query layer, below |
| Let an AI agent read and drive tmux | [`tmux-mcp`](crates/tmux-mcp) |
| Filter *your own* structs with the same grammar | [`libtmux-macros`](crates/libtmux-macros) |
| Build a workspace from a tmuxp-style YAML file | [`tmux-workspace`](crates/tmux-workspace) |

Not for you if you need Windows without WSL — tmux does not run there — or a
synchronous-first API. Blocking callers get a runtime, not a mirrored API.

## Install

The version has to be written out in full: Cargo does not resolve a prerelease
unless the requirement names one, so a plain `0.1` requirement selects nothing.

```console
$ cargo add libtmux@0.1.0-alpha.6
```

<details>
<summary>Cargo.toml</summary>

```toml
[dependencies]
libtmux = "0.1.0-alpha.6"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

</details>

## Drive tmux

```rust
use libtmux::test::TestServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // This example runs. `TestServer` is an isolated tmux on its own socket
    // under `/tmp/libtmux-rs-test/`, torn down at the end, so it cannot touch
    // sessions you are using. In your own code, that line is
    // `let server = libtmux::Server::new()?;` and the rest is unchanged.
    let guard = TestServer::new().await?;
    let server = guard.server();

    let session = server.new_session("work").await?;
    let window = session.new_window("editor").await?;
    let pane = window.active_pane().await?.expect("a window has a pane");

    pane.send_keys("echo built").await?;
    pane.send_key_names(["Enter"]).await?;

    assert_eq!(server.sessions().await?.len(), 1);

    guard.shutdown().await?;
    Ok(())
}
```

The examples on this page run as written, against a throwaway tmux. To run
them yourself, enable the `test-support` feature as a dev-dependency:
`libtmux = { version = "0.1.0-alpha.6", features = ["test-support"] }`.

Everything that reaches tmux is `async`. Everything that reads an
already-taken snapshot is not, so walking a tree you already have costs no
round trips.

## Query what is there

Typed field handles build the expression, so a comparison that has no meaning
for a field is a compile error rather than an empty result:

```rust
use libtmux::query::{Filterable as _, QueryIteratorExt as _};
use libtmux::test::TestServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let server = guard.server();
    server.new_session("work").await?;

    let panes = server.panes().await?;
    let fields = libtmux::Pane::filter_fields();

    // `pane_active` is a flag, so `.eq(true)` compiles; `.gt(..)` would not.
    let active = fields
        .pane_current_command
        .starts_with("sh")
        .and(fields.pane_active.eq(true));

    let found = panes.iter().matching(&active).count();
    assert_eq!(found, 1, "the session's one pane is active and running a shell");

    guard.shutdown().await?;
    Ok(())
}
```

Ask what a session *contains*, and the relation is part of the expression:

```rust
use libtmux::query::{Filterable as _, QueryIteratorExt as _};
use libtmux::test::TestServer;
use libtmux::{SessionTree, WindowTree};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let server = guard.server();

    let building = server.new_session("ci").await?;
    building.new_window("build").await?;
    server.new_session("idle").await?;

    let sessions = SessionTree::filter_fields();
    let windows = WindowTree::filter_fields();
    let has_build = sessions.windows.any(windows.window.window_name.eq("build"));

    // `hierarchy` gathers the whole tree in three tmux commands, not one
    // per object.
    let matched: Vec<_> = server
        .hierarchy()
        .await?
        .iter()
        .matching(&has_build)
        .map(|branch| branch.session.name().to_string_lossy().into_owned())
        .collect();

    assert_eq!(matched, ["ci"], "only the session holding a `build` window");

    guard.shutdown().await?;
    Ok(())
}
```

With the `serde` feature an expression lowers to a versioned JSON envelope, so
a CLI, a config file, or an MCP tool call can carry one.

## Pick how commands reach tmux

Three switches, each a Cargo feature, none of them the default. The same
workload under each — this is `cargo run --example matrix --all-features`,
verbatim:

```text
mode                     feature            dispatches processes    wall  attribution
blocking/sequential      plan                        6         6   168ms  per-command
async/sequential         plan                        6         6   189ms  per-command
async/folded             plan                        3         3    59ms  merged
async/marked-fold        plan                        3         3    57ms  merged
control-mode/streaming   plan,control-mode           6         1     8ms  per-command

every mode built the same thing: true
```

Same result, different cost — and different *evidence*. Folding a chain into
one tmux invocation halves the dispatches, but tmux reports one status for the
group, so a failure cannot be blamed on a member: attribution degrades to
`unknown` by design rather than guessing. Control mode keeps per-command
evidence over a single process.

[The `libtmux` guide](crates/libtmux/README.md#choosing-how-commands-reach-tmux)
has the behavior table, the how-to-turn-it-on table, and the named presets.

## Drive tmux from an agent

[`tmux-mcp`](crates/tmux-mcp) is a separate package: a Model Context Protocol
server over this API, read-biased, where every tool that changes state names
exactly what it changes.

```console
$ cargo install tmux-mcp --version 0.1.0-alpha.7
```

See [its README](crates/tmux-mcp/README.md) for the tool list and for wiring
it into an MCP client.

## Requirements

Rust 1.85 or newer, tmux 3.2a or newer, and a Unix target. Native Windows is
unsupported because tmux is unavailable there; WSL works.

Every supported tmux release — 3.2a, 3.4, 3.5a, 3.6b, and 3.7b — is built from
source in CI and runs the whole workspace, because tmux's own output and error
wording change between releases. Where a release is wrong rather than merely
old, the crate says so: `run_shell` refuses on 3.3 through 3.4, which drop the
command's output instead of returning it.

## Crates

| Crate | Version | What it is |
| --- | --- | --- |
| [`libtmux`](crates/libtmux) | [![libtmux]][libtmux-crate] | The async tmux client and object model |
| [`libtmux-macros`](crates/libtmux-macros) | [![libtmux-macros]][macros-crate] | `#[derive(Filterable)]`, for your own structs |
| [`tmux-mcp`](crates/tmux-mcp) | [![tmux-mcp]][mcp-crate] | A Model Context Protocol server over tmux |
| [`tmux-workspace`](crates/tmux-workspace) | [![tmux-workspace]][workspace-crate] | A tmuxp-style YAML builder |

[libtmux-macros]: https://img.shields.io/crates/v/libtmux-macros.svg?label=libtmux-macros
[macros-crate]: https://crates.io/crates/libtmux-macros
[tmux-workspace]: https://img.shields.io/crates/v/tmux-workspace.svg?label=tmux-workspace
[workspace-crate]: https://crates.io/crates/tmux-workspace

`libtmux` does not require proc macros: its own `Filterable` implementations
are hand-written, and the derive exists for structs outside this workspace.

The last two crates are also how the API gets used from outside it. When
either needs a workaround, that is treated as a finding about `libtmux` rather
than about them.

## Development

`just` lists every recipe. Cargo is authoritative; the justfile only groups
Cargo commands.

```console
$ just check
```

That runs what CI runs: formatting, Clippy, tests, doctests, docs, the feature
powerset, `cargo deny`, both MSRV builds, and a packaging check.

Tests run against real tmux rather than a mock. `libtmux::test::TestServer`
gives each test an isolated socket under `/tmp/libtmux-rs-test/` and
deterministic cleanup.

Every Rust example on this page is compiled by `cargo test --doc`, including
the ones in this file. See [`AGENTS.md`](AGENTS.md) for the conventions and
the things that will bite.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this workspace by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
