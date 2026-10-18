# Standard-library core and Tokio adapter

Run the same tmux values through an offline program and a live Tokio client.
This is a small package-split prototype: pane listing and capture work; the
rest of the published client's API has not been migrated.

| Package | Contains | Dependencies |
| --- | --- | --- |
| `libtmux` | Pane IDs, detached snapshots, capture requests, byte text | None |
| `libtmux-tokio` | `Server`, `Pane`, asynchronous process execution | Core, Tokio |
| `split-demo` | Offline and live consumer programs | Core; adapter for `live` |

The core has no dependency declarations, including optional, build, target,
and development dependencies. It uses `std`; it is not `no_std`.

## Try it

Use the repository's pinned Rust toolchain. Enter this workspace:

```console
$ cd experiments/standard-library-core
```

Construct a request and decode captured bytes without tmux or Tokio:

```console
$ cargo run --locked -p split-demo --bin offline
```

Inspect that consumer's dependency graph:

```console
$ cargo tree --locked -p split-demo
```

With tmux 3.2a or newer on `PATH`, run the live consumer:

```console
$ cargo run --locked -p split-demo --features live --bin live
```

It starts an isolated server under `/tmp/libtmux-rs-dev/`, captures a message,
and uses a detached snapshot after dropping the runtime. It kills its own
server and removes its socket on exit. Set `LIBTMUX_TEST_TMUX` to select a
different tmux executable. Both programs print human-readable output to stdout,
report failures to stderr, and exit zero on success or one on failure.

Run dependency, formatting, lint, unit, doctest, and live checks:

```console
$ bash check.sh
```

## The boundary

`CapturePane` can be built offline. `Server::capture(&request).await` executes
it. `Pane` owns its server connection and a `PaneSnapshot`; callers do not
carry a runtime type parameter. A `PaneSnapshot` survives runtime shutdown
because it contains only core values.

This follows the separation used by [Quinn's protocol crate][quinn] and
[Tokio Tungstenite's adapter][tungstenite]. Those cores still have dependencies;
runtime independence alone does not give a zero-dependency package.

## Before migrating the client

This adapter has a five-second command deadline and a 2 MiB bound per output
pipe. It drains stdout and stderr concurrently and waits for the direct
client on completion or execution failure. Dropping an in-flight future asks
Tokio to kill that client; it does not establish completed reaping or process
group containment. This is not the published client's supervisor.

A real migration should move that supervisor intact, then move the remaining
commands, snapshots, and protocol state. Regex matching belongs in a companion
query package. Serde needs wrappers or wire types for core-owned values because
of Rust's [orphan rules][orphan]. These integrations are not implemented here.

[quinn]: https://github.com/quinn-rs/quinn/tree/main/quinn-proto
[tungstenite]: https://github.com/snapview/tokio-tungstenite
[orphan]: https://doc.rust-lang.org/error_codes/E0117.html
