<!-- libtmux-logo -->
<p align="center">
  <picture>
    <source srcset="assets/logo.svg" type="image/svg+xml">
    <img src="assets/logo.png" width="128" height="128" alt="libtmux for Rust">
  </picture>
</p>
<!-- /libtmux-logo -->

<div align="center">

# libtmux for Rust

[![CI]][actions] [![libtmux]][libtmux-crate] [![tmux-mcp]][mcp-crate] [![MSRV]][rust-1.97.1] [![License]][license]

[CI]: https://github.com/libtmux/libtmux-rs/actions/workflows/ci.yml/badge.svg
[actions]: https://github.com/libtmux/libtmux-rs/actions/workflows/ci.yml
[libtmux]: https://img.shields.io/crates/v/libtmux.svg?label=libtmux
[libtmux-crate]: https://crates.io/crates/libtmux
[tmux-mcp]: https://img.shields.io/crates/v/tmux-mcp.svg?label=tmux-mcp
[mcp-crate]: https://crates.io/crates/tmux-mcp
[MSRV]: https://img.shields.io/badge/rustc-1.97.1+-lightgray.svg
[rust-1.97.1]: https://www.rust-lang.org/tools/install
[License]: https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg
[license]: #license

**Typed blocking and async clients for tmux servers, sessions, windows, and panes.**

</div>

> **Alpha.** Releases carry an `-alpha` prerelease tag. The API is not
> settled, and any release may change or remove exported identifiers without a
> deprecation period. Pin an exact version. Not recommended for production.

`libtmux` provides a usable blocking client with no default crate dependencies.
`libtmux-tokio` provides native async operations with the same object model.
Both support subprocess dispatch, persistent control mode, queries, plans,
scoped operations, and isolated test servers.

## Choose a package

| Use | Package |
| --- | --- |
| Control tmux from synchronous Rust | [`libtmux`](crates/libtmux/README.md) |
| Control tmux from a Tokio application | [`libtmux-tokio`](crates/libtmux-tokio/README.md) |
| Derive query fields for your structs | [`libtmux-macros`](crates/libtmux-macros/README.md) |
| Expose tmux through MCP | [`tmux-mcp`](crates/tmux-mcp/README.md) |
| Load tmuxp YAML workspaces | [`tmux-workspace`](crates/tmux-workspace/README.md) |

The clients require Rust 1.97.1, Linux or WSL, and tmux 3.2a or newer.
The blocking client also uses Linux procfs and `/usr/bin/kill` for process
supervision. [Package boundaries](crates/libtmux/docs/packages.md) explains
features, cancellation, resource limits, and release order.

## Drive tmux

The package split is unreleased. Use this branch as a Git dependency:

```toml
[dependencies]
libtmux = { git = "https://github.com/libtmux/libtmux-rs", branch = "standard-library-core", features = ["test-support"] }
```

The following example creates and cleans up its own server using the
`test-support` feature. An application controlling an existing server uses
`Server::new()` and can omit that feature.

```rust
use libtmux::test::TestServer;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestServer::new()?;
    let server = fixture.server();
    let session = server.new_session("work")?;
    let window = session.new_window("editor")?;
    let pane = window.active_pane()?.ok_or("new window has no pane")?;
    pane.send_line("echo built")?;
    assert_eq!(server.sessions()?.len(), 1);
    fixture.shutdown()?;
    Ok(())
}
```

The [blocking examples](crates/libtmux/examples/api/README.md) and
[async examples](crates/libtmux-tokio/examples/api/README.md) cover construction,
listings, creation, queries, cardinality, input, and capture. Each collection
includes a consumer manifest and complete programs.

## Control mode and plans

The `control-mode` feature keeps a tmux client attached for commands and
notifications. It supports direct commands, routed object operations, pane
output, and snapshot updates. The `plan` feature records operations before
executing them. Both features add no dependencies to the blocking package.

Run the `watch`, `matrix`, and `orchestrate` examples in either client package
to exercise these features. Each example owns its fixture and cleanup. See
[design](crates/libtmux/docs/design.md), [findings](crates/libtmux/docs/findings.md),
and the [capability ledger](crates/libtmux/docs/parity.md) for their contracts.

## Load workspaces from YAML

`tmux-workspace` loads tmuxp workspace files through the native async client.
Build the CLI from this checkout:

```console
$ cargo build --locked --package tmux-workspace
```

Preview the bundled workspace without creating a tmux session:

```console
$ PATH="$PWD/target/debug:$PATH" tmux-workspace load --dry-run \
    crates/tmux-workspace/tests/fixtures/tmuxp/minimal.yaml
```

See the [workspace guide](crates/tmux-workspace/README.md) for loading,
validation, and supported tmuxp settings.

## Development

`just` lists the recipes. `just check` runs the local verification gates;
Cargo is authoritative. Tests use real tmux on isolated sockets under
`/tmp/libtmux-rs-test/`.

The blocking operations are checked-in generated source. `just sync-check`
checks their canonical async source and the manual implementation inventory.
Both client packages have their own runtime, control-mode, and downstream
consumer checks.

See [CONTRIBUTING.md](.github/CONTRIBUTING.md) for build and release policy and
[WRITING.md](.github/WRITING.md) for documentation style.

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
