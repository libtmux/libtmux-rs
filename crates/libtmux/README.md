<!-- libtmux-logo -->
<div align="center">
  <picture>
    <source srcset="https://raw.githubusercontent.com/libtmux/libtmux-rs/master/crates/libtmux/assets/logo.svg" type="image/svg+xml">
    <img src="https://raw.githubusercontent.com/libtmux/libtmux-rs/master/crates/libtmux/assets/logo.png" width="128" height="128" alt="libtmux for Rust">
  </picture>
</div>
<!-- /libtmux-logo -->

<div align="center">

# libtmux

[![crates.io]][crate] [![docs.rs]][docs] [![MSRV]][rust-1.85]
[![libtmux.org](https://img.shields.io/badge/docs-libtmux.org-0b7285)](https://libtmux.org/en/rs/)

[crates.io]: https://img.shields.io/crates/v/libtmux.svg?label=libtmux
[crate]: https://crates.io/crates/libtmux
[docs.rs]: https://img.shields.io/docsrs/libtmux
[docs]: https://docs.rs/libtmux
[MSRV]: https://img.shields.io/badge/rustc-1.85+-lightgray.svg
[rust-1.85]: https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/

Drive tmux with a native blocking or Tokio client.

</div>

The default client is `libtmux::blocking::Server`. Enable the `tokio` feature
to use `libtmux::tokio::Server`. Both clients use the same types for tmux IDs,
creation requests and receipts. With default features disabled the facade
exports the shared domain types alone.

## Create and clean a session

From a binary project next to this checkout, add the default blocking client:

```console
$ cargo add libtmux --path ../libtmux-rs/crates/libtmux
```

[`blocking::Server::at`][server-at] binds the client to your socket. Save this
as `src/main.rs`; it creates a detached session, observes its pane key and
kills the session. The reader code and its normal dependencies need no Tokio.

[server-at]: https://libtmux.org/en/rs/latest/reference/libtmux_blocking-server-at/

<!-- blocking-example -->

```rust,no_run
use libtmux::{NewSession, blocking::Server};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = std::env::args_os().nth(1).ok_or("pass an absolute socket path")?;
    let server = Server::at(socket)?;
    let created = server.new_session(NewSession::new("readme").command("sleep 30"))?;
    let observed = server.observe_pane(created.pane().id());
    server.kill_created_session(&created)?;
    assert_eq!(&observed?, created.pane());
    println!("created and cleaned one session");
    Ok(())
}
```

<!-- /blocking-example -->

Pass an absolute socket path in a private directory you own. These commands
choose a separate endpoint for this program:

```console
$ mkdir -p /tmp/libtmux-rs-dev
```

```console
$ directory=$(mktemp -d /tmp/libtmux-rs-dev/readme.XXXXXX)
```

```console
$ cargo run -- "$directory/socket"
```

It prints `created and cleaned one session`. The program kills the session it
created even if observing its pane fails.

The [mixed-client program](examples/api_mixed.rs) passes the same pane key
to a Tokio capture. Enable `tokio` when you need that client.

## Describe a session

`NewSession` describes what to create. Constructing it performs no I/O.
A plain name reaches tmux literally, including a `#` in a branch name.

```rust
use libtmux::NewSession;

let request = NewSession::new("issue#185").window_name("editor").size(120, 40);
let operands = request.operands(NewSession::RECEIPT_FORMAT);
assert!(operands.iter().any(|arg| arg.as_os_str() == "issue##185"));
```

Use `libtmux-core` directly when recording requests or decoding tmux output
offline. A blocking-only application can depend on `libtmux` with the
defaults. A Tokio application can enable only `tokio` or combine both
drivers in one process.

[Architecture decision](docs/adr/0001-native-blocking-and-tokio.md) ·
[Migration guide](docs/migration.md) ·
[Versioned Rust guide](https://libtmux.org/en/rs/)

> Releases are numbered `0.1.N`, and each release increments `N`. The API is not
> settled: any release may change or remove exported identifiers, and the
> changelog marks each break. Pin an exact version to avoid one.

[Changelog](CHANGELOG.md)
