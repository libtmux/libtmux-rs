<!-- libtmux-logo -->
<div align="center">
  <picture>
    <source srcset="assets/logo.svg" type="image/svg+xml">
    <img src="assets/logo.png" width="128" height="128" alt="libtmux for Rust">
  </picture>
</div>
<!-- /libtmux-logo -->

<div align="center">

# libtmux for Rust

[![CI]][actions] [![libtmux]][libtmux-crate]
[![libtmux.org](https://img.shields.io/badge/docs-libtmux.org-0b7285)](https://libtmux.org/en/rs/)
[![License]][license] [![MSRV]][rust-1.85]

[CI]: https://github.com/libtmux/libtmux-rs/actions/workflows/ci.yml/badge.svg
[actions]: https://github.com/libtmux/libtmux-rs/actions/workflows/ci.yml
[libtmux]: https://img.shields.io/crates/v/libtmux.svg?label=libtmux
[libtmux-crate]: https://crates.io/crates/libtmux
[tmux-mcp]: https://img.shields.io/crates/v/tmux-mcp.svg?label=tmux-mcp
[mcp-crate]: https://crates.io/crates/tmux-mcp
[MSRV]: https://img.shields.io/badge/rustc-1.85+-lightgray.svg
[rust-1.85]: https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/
[License]: https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg
[license]: https://github.com/libtmux/libtmux-rs/blob/master/LICENSE-MIT

**Drive tmux from Rust: typed, async control over servers, sessions, windows,
and panes — and a query layer that makes "which pane is running the tests?" one
expression instead of a parsing problem.**

</div>

[Type into a pane][`Pane::send_line`], [query][Typed field handles] what a
server holds with typed expressions, [load tmuxp workspaces][workspace-cli-load]
from YAML, and [let an agent drive tmux][mcp-tools] over the Model Context
Protocol.

> Releases are numbered `0.1.N`, and each release increments `N`. The API is
> not settled: any release may change or remove exported identifiers, and the
> changelog marks each break. Pin an exact version to avoid one.

**Contents** — [Quick start](#quick-start) ·
[Is this for you?](#is-this-for-you) ·
[Query what is there](#query-what-is-there) ·
[Pick how commands reach tmux](#pick-how-commands-reach-tmux) ·
[Load workspaces](#load-workspaces-from-yaml) ·
[Drive tmux from an agent](#drive-tmux-from-an-agent) ·
[Packages](#packages) · [What it leaves out](#what-it-leaves-out) ·
[Support](#support-and-status) · [Documentation](#documentation)

## Quick start

```console
$ cargo add libtmux
```

<details>
<summary>Cargo.toml</summary>

```toml
[dependencies]
libtmux = "0.1.1"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

</details>

Create a session and a window, type into the pane, and check what tmux holds:

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
    let pane = window.active_pane().await?.expect("the new window has a pane");

    pane.send_line("echo built").await?;

    assert_eq!(server.sessions().await?.len(), 1);

    guard.shutdown().await?;
    Ok(())
}
```

The examples on this page run as written, against a throwaway tmux. To run
them yourself, enable the `test-support` feature as a dev-dependency:
`libtmux = { version = "0.1.1", features = ["test-support"] }`.

Everything that reaches tmux is `async`. Everything that reads an
already-taken snapshot is not, so walking a tree you already have costs no
round trips.

## Is this for you?

| You want to | Use |
| --- | --- |
| Script tmux from Rust, async | [`libtmux`](crates/libtmux) |
| Ask "which pane/window/session matches X?" | [`libtmux`](crates/libtmux) query layer, below |
| Let an AI agent read and drive tmux | [`tmux-mcp`](crates/tmux-mcp) |
| Filter *your own* structs with the same grammar | [`libtmux-macros`](crates/libtmux-macros) |
| Load tmuxp workspaces from the command line, or build them from Rust | [`tmux-workspace`](crates/tmux-workspace) |

Not for you if you need Windows without WSL, because tmux does not run there,
or a synchronous-first API. A blocking caller drives the async API through
[`blocking::Runtime`].

## Query what is there

### [`Filterable::filter_fields`]: a comparison that means nothing does not compile

[Typed field handles] build the expression, so a comparison that has no meaning
for a field is a compile error rather than an empty result:

```rust
use std::time::Duration;

use libtmux::query::{Filterable as _, QueryIteratorExt as _};
use libtmux::test::{TestServer, retry_until};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let server = guard.server();
    server.new_session("work").await?;

    let fields = libtmux::Pane::filter_fields();

    // `pane_active` is a flag, so `.eq(true)` compiles; `.gt(..)` would not.
    let active = fields
        .pane_current_command
        .starts_with("sh")
        .and(fields.pane_active.eq(true));

    // tmux hands back a pane the moment it forks, before the shell in it has
    // started, so what a pane is running is worth waiting for rather than
    // assuming. A wait must assert the outcome it got: this one fails if the
    // deadline passes without the expression ever matching.
    retry_until(Duration::from_secs(5), async || {
        server
            .panes()
            .await
            .is_ok_and(|panes| panes.iter().matching(&active).count() == 1)
    })
    .await?;

    guard.shutdown().await?;
    Ok(())
}
```

### [`SessionTree`] and [`WindowTree`]: ask what a session contains

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

With the `serde` feature an expression lowers to a [versioned JSON envelope], so
a CLI, a config file, or an MCP tool call can carry one.

## Pick how commands reach tmux

### [`Plan::run`]: the same work at different costs

[Three switches], each a Cargo feature, none of them the default. This is the
baseline workload under each, printed by
`cargo run --example matrix --all-features -- --baseline`.

- Every column but `wall` is exact and checked against this block by
  `just example-tables`. The timings are one run on one developer machine.
- `processes` is counted: the example runs tmux through a wrapper that logs
  each start.
- `routed` is [`Plan::run`] on a [`Server::over_control_mode`] handle.

<!-- example-output: matrix --all-features -- --baseline -->

```text
mode                     feature            dispatches processes      wall  attribution  query output
----------------------------------------------------------------------------------------------------------------------
blocking/sequential      plan                        6         7     16ms  per-command  2 panes, 2 windows, 2 active
async/sequential         plan                        6         7     16ms  per-command  2 panes, 2 windows, 2 active
async/folded             plan                        3         4     10ms  merged       2 panes, 2 windows, 2 active
async/marked-fold        plan                        3         4     10ms  merged       2 panes, 2 windows, 2 active

every mode built the same thing: true
dispatches ranged 3..6, processes ranged 4..7

the same failure, dispatched two ways:
  Sequential   1 dispatch(es) -> [Failed, Skipped]
  Folding      1 dispatch(es) -> [Unknown, Unknown]
  Sequential names the failing operation; Folding cannot, because tmux
  reports one status for the group whichever member failed.
```

The result is the same and the cost differs, and so does the *evidence*:

- **[Folding]** a chain into one tmux invocation halves the dispatches. tmux
  reports one status for the group, so a failure cannot be blamed on a member,
  and attribution degrades to `unknown` rather than guessing.
- **[Control mode]** keeps per-command evidence over one persistent connection
  after its version probes. Run the example without `-- --baseline` to include
  control rows with a compatible client and a daemon ranking at 3.7 or
  newer.

[The `libtmux` guide](crates/libtmux/README.md#choosing-how-commands-reach-tmux)
has the behavior table, the how-to-turn-it-on table, and the named presets.

## Load workspaces from YAML

### [`tmux-workspace load`][workspace-cli-load]: build a session from a file

[`tmux-workspace`](crates/tmux-workspace) is a native tmuxp. It
[loads][workspace-cli-load] tmuxp workspace files, [freezes][workspace-cli-freeze]
running sessions back into them, and [imports][workspace-cli-import]
tmuxinator and teamocil projects.

```console
$ cargo install tmux-workspace --locked
```

```console
$ tmux-workspace load ./dev.yaml
```

See [its README](crates/tmux-workspace/README.md) for every command.

## Drive tmux from an agent

### [`tmux-mcp`][mcp-tools]: a tmux server for agents

[`tmux-mcp`](crates/tmux-mcp) is a separate package: a Model Context Protocol
server over this API, read-biased, where every tool that changes state names
exactly what it changes.

```console
$ cargo install tmux-mcp
```

See [its README](crates/tmux-mcp/README.md) for the tool list and for wiring
it into an MCP client.

## Packages

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

[`libtmux`](crates/libtmux) does not require proc macros: its own
[`Filterable`] implementations are hand-written, and the derive exists for
structs outside this workspace.

The last two crates are also how the API gets used from outside it. When
either needs a workaround, that is treated as a finding about `libtmux` rather
than about them.

### [`libtmux`](https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux)

The async tmux client and object model: servers, sessions, windows, and panes
as typed handles, and the query layer over them.

[![libtmux]][libtmux-crate] [API reference][libtmux-api] ·
[docs.rs][libtmux-docsrs] · [Guide][libtmux-guide] ·
[Programs][libtmux-examples]

The examples above are this crate's: [Quick start](#quick-start),
[Query what is there](#query-what-is-there), and
[Pick how commands reach tmux](#pick-how-commands-reach-tmux). The programs
under `examples/` are longer.

### [`libtmux-macros`](https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux-macros)

`#[derive(Filterable)]`, so your own structs take the same typed query grammar
as tmux objects. You do not depend on this crate: `libtmux` re-exports the
derive behind its `derive` feature.

[![libtmux-macros]][macros-crate] [API reference][macros-docsrs] ·
[Guide][macros-guide]

```console
$ cargo add libtmux --features derive
```

The example in [its README][macros-guide] derives `Filterable` for a `Job`
struct and filters a list of jobs with the expressions `libtmux` uses for
panes.

### [`tmux-workspace`](https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-workspace)

A native tmuxp: it loads tmuxp workspace files, freezes running sessions back
into them, and imports tmuxinator and teamocil projects, as one binary with no
Python.

[![tmux-workspace]][workspace-crate] [Command reference][workspace-cli] ·
[API reference][workspace-api] · [docs.rs][workspace-docsrs] ·
[Guide][workspace-guide]

Describe a session in `dev.yaml`:

```yaml
session_name: dev
windows:
  - window_name: editor
    panes:
      - vim
      - git status
```

Save a running session back to a file:

```console
$ tmux-workspace freeze dev -o dev.yaml
```

### [`tmux-mcp`](https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-mcp)

A Model Context Protocol server over `libtmux`. It is read-biased, and every
tool that changes state names what it changes.

[![tmux-mcp]][mcp-crate] [Tools][mcp-tools] · [API reference][mcp-api] ·
[docs.rs][mcp-docsrs] · [Connect a client][mcp-connect] · [Guide][mcp-guide]

Register it with Claude Code. [Its README][mcp-guide] has the same step for
Codex, Gemini, Cursor, VS Code, and the other clients:

```console
$ claude mcp add tmux -- tmux-mcp
```

Offer only the read tools, on the tmux socket named `work`:

```console
$ claude mcp add tmux \
    --env LIBTMUX_TOOLSETS=inspect \
    -- tmux-mcp --socket-name work
```

## What it leaves out

- **A synchronous API.** Blocking callers drive the async API through
  [`blocking::Runtime`].
- **Native Windows.** tmux does not run there, so use WSL.
- **A mock tmux.** Tests run against a real tmux, because what tmux answers is
  the thing under test. [`libtmux::test::TestServer`] gives each test an
  isolated socket.
- **Proc macros in `libtmux` itself.** Its own [`Filterable`] implementations
  are hand-written, and the derive exists for structs outside this workspace.

## Support and status

- **Rust 1.85 or newer** for the libraries. `tmux-mcp` needs 1.88.
- **tmux 3.3a or newer.** CI builds pinned tmux 3.3a and 3.8 from source on each
  push, and 3.3a, 3.4, 3.5a, 3.6b, 3.7b, and 3.8 every week and on demand, and
  runs the whole workspace against each, because tmux's output and error
  wording change between releases. tmux release candidates are not
  supported: a candidate such as `X.Y-rcN` ranks below `X.Y`, and a feature
  can require one only until its release ships. See
  [Release candidates](https://libtmux.org/en/tmux/guides/tmux-support/#release-candidates).
- **A refusal on two releases.** [`run_shell`] refuses on 3.3a and 3.4, which
  drop the command's output instead of returning it.
- **Linux, including WSL.** CI runs the tests on Linux. macOS is supported
  on a best-effort basis; the release workflow builds binaries there, and CI
  does not run the tests.
- **Unix only.** Native Windows is unsupported because tmux is unavailable
  there.

## Documentation

You may be looking for:

- [API documentation](https://docs.rs/libtmux) — every public item, with a
  runnable example
- [API reference](https://libtmux.org/en/rs/latest/reference/) — the same
  items on libtmux.org, with guides and concepts beside them
- [The `libtmux` guide](crates/libtmux/README.md) — features, the three
  transport switches, testing
- [`tmux-mcp`](crates/tmux-mcp/README.md) — the MCP server, a **separate
  package**, if you want an agent to drive tmux
- [Examples](crates/libtmux/examples) — programs that run and clean up after
  themselves, from reading a server to watching one over control mode
- [Design notes](crates/libtmux/docs/design.md) — why it is shaped this way
- [Parity ledger](crates/libtmux/docs/parity.md) — capability-by-capability
  against Python libtmux

## Development

`just` lists every recipe. Cargo is authoritative; the justfile only groups
Cargo commands.

```console
$ just check
```

That runs what CI runs: formatting, Clippy, tests, doctests, docs, the feature
powerset, `cargo deny`, both MSRV builds, and a packaging check.

Tests run against real tmux rather than a mock. [`libtmux::test::TestServer`]
gives each test an isolated socket under `/tmp/libtmux-rs-test/` and
deterministic cleanup.

Every Rust example on this page is compiled by `cargo test --doc`, including
the ones in this file. See
[`.github/CONTRIBUTING.md`](.github/CONTRIBUTING.md) for the gates and the
things that will bite, and [`.github/WRITING.md`](.github/WRITING.md) for how
the prose reads.

The `tmux-workspace` commands and the workspace YAML on this page are parsed by
that crate's tests, and the `tmux-mcp` commands by that crate's, against the
real command lines.

## Attribution

Please use the following BibTeX template to cite libtmux-rs in scientific discourse:

```bibtex
@misc{libtmux-rs,
   author = {Tony Narlock},
   year = {2026},
   note = {https://libtmux.org/en/rs/},
   title = {libtmux-rs: Rust wrapper for tmux}
}
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this workspace by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.

[libtmux-api]: https://libtmux.org/en/rs/latest/reference/
[libtmux-docsrs]: https://docs.rs/libtmux/latest/libtmux/
[libtmux-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/README.md
[libtmux-examples]: https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux/examples
[macros-docsrs]: https://docs.rs/libtmux-macros/latest/libtmux_macros/derive.Filterable.html
[macros-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-macros/README.md#quick-start
[workspace-cli]: https://libtmux.org/en/rs/latest/workspace/cli/
[workspace-cli-load]: https://libtmux.org/en/rs/latest/workspace/cli/load/
[workspace-cli-freeze]: https://libtmux.org/en/rs/latest/workspace/cli/freeze/
[workspace-cli-import]: https://libtmux.org/en/rs/latest/workspace/cli/import/
[workspace-api]: https://libtmux.org/en/rs/latest/workspace/reference/
[workspace-docsrs]: https://docs.rs/tmux-workspace/latest/tmux_workspace/
[workspace-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-workspace/README.md
[mcp-tools]: https://libtmux.org/en/rs/latest/mcp/tools/
[mcp-api]: https://libtmux.org/en/rs/latest/mcp/reference/
[mcp-docsrs]: https://docs.rs/tmux-mcp/latest/tmux_mcp/
[mcp-connect]: https://libtmux.org/en/rs/latest/mcp/guides/connect-client/
[mcp-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/README.md

[Typed field handles]: https://libtmux.org/en/rs/latest/reference/query-filterable-filter_fields/
[versioned JSON envelope]: https://libtmux.org/en/rs/latest/reference/query-filterexpr/
[Three switches]: https://libtmux.org/en/rs/latest/concepts/transports/
[Folding]: https://libtmux.org/en/rs/latest/reference/plan-planner-planner-folding/
[`Plan::run`]: https://libtmux.org/en/rs/latest/reference/plan-plan-run/
[`Server::over_control_mode`]: https://libtmux.org/en/rs/latest/reference/server-server-over_control_mode/
[Control mode]: https://libtmux.org/en/rs/latest/reference/control-controlmode/
[`run_shell`]: https://libtmux.org/en/rs/latest/reference/server-server-run_shell/
[`Filterable`]: https://libtmux.org/en/rs/latest/reference/query-filterable/
[`libtmux::test::TestServer`]: https://libtmux.org/en/rs/latest/reference/test-testserver/
[`Filterable::filter_fields`]: https://libtmux.org/en/rs/latest/reference/query-filterable-filter_fields/
[`Pane::send_line`]: https://libtmux.org/en/rs/latest/reference/pane-pane-send_line/
[`SessionTree`]: https://libtmux.org/en/rs/latest/reference/server-discovery-sessiontree/
[`WindowTree`]: https://libtmux.org/en/rs/latest/reference/server-discovery-windowtree/
[`blocking::Runtime`]: https://libtmux.org/en/rs/latest/reference/blocking-runtime/
