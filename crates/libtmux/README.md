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
