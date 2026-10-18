# Choosing a client

`libtmux` controls tmux with blocking calls and no default crate dependencies.
`libtmux-tokio` provides the same object operations as native Tokio futures.
Both packages include `Server`, `Session`, `Window`, `Pane`, and `Client`.
Both support subprocess dispatch and persistent control mode.

| Package | Calls | Dependencies |
| --- | --- | --- |
| `libtmux` | Blocking; ordinary Rust functions and iterators | Standard library by default; optional query, derive, serialization, tracing, and test support |
| `libtmux-tokio` | Async operations and streams on Tokio | `libtmux` values and protocol, Tokio, process support, and optional features |
| `libtmux-macros` | `Filterable` derive for downstream structs | Procedural macro tooling; selected through either client's `derive` feature |
| `tmux-mcp` | MCP server | `libtmux-tokio` |
| `tmux-workspace` | YAML workspace builder | `libtmux-tokio` |

Use the blocking package in a synchronous application. An async application
that uses Tokio should choose `libtmux-tokio`; blocking calls occupy the thread
that invokes them. Both clients require Linux, tmux 3.2a or later, and Rust
1.97.1 or later.

## Install and call

The package split is unreleased. Use this branch as a Git dependency:

```toml
[dependencies]
libtmux = { git = "https://github.com/libtmux/libtmux-rs", branch = "standard-library-core" }
```

```rust,no_run
use libtmux::{NewSessionOptions, Server};

# fn main() -> Result<(), libtmux::Error> {
let server = Server::new()?;
let session = server.new_session(NewSessionOptions::new("editor"))?;
for window in session.windows()? {
    println!("{}", window.name().to_string_lossy());
}
server.shutdown()?;
# Ok(())
# }
```

For an async application, select `libtmux-tokio` and add Tokio with the runtime
features your application needs. Its corresponding operations use `.await`.
The package's README and API examples contain complete programs.

## Feature boundaries

The blocking package's default feature set is empty. `control-mode` and `plan`
also add no crate dependencies. `query` adds Unicode matching and regular
expressions; `derive`, `serde`, `schema`, `tracing`, and `test-support` add their
respective tooling. `libtmux-tokio` enables `query` by default.

Cargo combines a package's enabled features across a dependency graph. If one
application depends on both clients, the async package can enable optional
features in their shared `libtmux` dependency. Evaluate the actual application
with `cargo tree -e normal,build`, including the features it selects.

## Process and cancellation behavior

The blocking backend uses Linux procfs to observe child exit before reaping.
It invokes `/usr/bin/kill` to signal process groups because stable, safe
standard-library APIs do not provide that operation. Configure another
absolute executable with `ServerBuilder::group_signal_helper`. A missing
helper prevents dispatch. A helper failure returns a cleanup error and may
leave descendants running.

The application must leave child reaping to the library. Process-wide
`waitpid(-1)`, ignored `SIGCHLD`, and `SA_NOCLDWAIT` can release an owned PID
before cleanup. A failed ownership probe retires numeric cleanup; it reports
the failure rather than claiming containment. Processes stuck in an
uninterruptible kernel operation have no bounded cleanup-time guarantee.

Both clients accept a `CancellationToken`. Cancelling stops admission and
requests that active work stop; scoped cleanup uses an uncancelled cleanup
path. Call `shutdown` to join owned client work. It does not remove sessions
or stop the tmux server.

Channel waits can retain a parked tmux client after their caller times out so
a later signal is preserved. `DispatchLimits::max_wait_clients` bounds these
clients separately from ordinary commands, defaulting to 64. Saturation fails
before another client starts. The blocking backend uses three resident
threads per parked client; Tokio uses tasks. Control clients have a separate
`ControlClientLimits` budget.

## Keeping the interfaces aligned

High-level blocking operations are generated from the async client by the
workspace's `sync-client` development tool and checked into source control.
The tool records source modules and their parity disposition. Runtime,
process, cancellation, and control-mode implementations are maintained
separately and tested against the same behavior contracts. The generator is
not a build dependency of either published client.

Release the shared `libtmux-macros` package first when it changes, then
`libtmux`, `libtmux-tokio`, and the applications. Packaging checks verify
included sources and documentation; downstream consumer checks exercise
crate names, features, derive expansion, and examples.
