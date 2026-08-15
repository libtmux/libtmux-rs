# libtmux-rs

Drive tmux from Rust: typed, async control over servers, sessions, windows, and
panes.

```rust
use libtmux::Server;

let server = Server::new()?;
let session = server.new_session("work").await?;
let window = session.new_window("editor").await?;
let pane = window.active_pane().await?.expect("a window has a pane");

pane.send_keys("cargo test").await?;
```

> **Alpha.** Every crate here is prerelease. The API changes between releases,
> including in ways that will not be called out as breaking, because nothing is
> stable yet. Cargo will not resolve a prerelease unless the requirement names
> one, so a plain `0.1` requirement does not pick these up: depend on the exact
> version, and expect to edit it.

The crate documentation is in [`crates/libtmux/README.md`](crates/libtmux/README.md);
this file describes the workspace.

## Crates

| Crate | crates.io | What it is |
| --- | --- | --- |
| [`libtmux`](crates/libtmux) | [![crates.io](https://img.shields.io/crates/v/libtmux.svg)](https://crates.io/crates/libtmux) | The async tmux client and object model |
| [`libtmux-macros`](crates/libtmux-macros) | [![crates.io](https://img.shields.io/crates/v/libtmux-macros.svg)](https://crates.io/crates/libtmux-macros) | `#[derive(Filterable)]`, for downstream structs |
| [`tmux-mcp`](crates/tmux-mcp) | [![crates.io](https://img.shields.io/crates/v/tmux-mcp.svg)](https://crates.io/crates/tmux-mcp) | A Model Context Protocol server over tmux |
| [`tmux-workspace`](crates/tmux-workspace) | not published | A tmuxp-style YAML builder |

`libtmux` does not require proc macros: its own `Filterable` implementations are
hand-written, and the derive exists for structs outside this workspace.

The last two crates are also how the API gets used from outside it. When either
needs a workaround, that is treated as a finding about `libtmux` rather than
about them.

## Requirements

Rust 1.85 or newer, tmux 3.2a or newer, and a Unix target. Native Windows is
unsupported because tmux is unavailable there; WSL works.

Every supported tmux release -- 3.2a, 3.4, 3.5a, 3.6, and 3.7b -- is built from
source in CI and runs the whole workspace, because tmux's own output and error
wording change between releases.

## Working on it

`just` lists every recipe. Cargo is authoritative; the justfile only groups
Cargo commands.

```console
$ just check
```

That runs what CI runs: formatting, Clippy, tests, doctests, docs, the feature
powerset, `cargo deny`, the MSRV build, and a packaging check.

Tests run against real tmux rather than a mock. `libtmux::test::TestServer`
gives each test an isolated socket under `/tmp/libtmux-rs-test/` and
deterministic cleanup.

## Documentation

- [`crates/libtmux/docs/design.md`](crates/libtmux/docs/design.md) -- why the
  crate is shaped the way it is: transport, the snapshot and format boundary,
  the query grammar, control mode, test architecture, and the compatibility
  lanes.
- [`crates/libtmux/docs/parity.md`](crates/libtmux/docs/parity.md) -- the
  capability ledger against Python libtmux.
- [`AGENTS.md`](AGENTS.md) -- conventions, and the things that will bite.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this workspace by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
