# libtmux

A blocking tmux client with no default crate dependencies.

`Server`, `Session`, `Window`, `Pane`, and `Client` expose listings, creation,
options, hooks, key bindings, buffers, pane input, capture, and scoped cleanup.
Commands use argument arrays, and output preserves bytes until you choose how
to decode them. Accessors read snapshots; explicit refresh reads tmux again.

Choose `libtmux-tokio` for native asynchronous I/O, timers, and task
supervision. A blocking operation belongs on a synchronous thread.

> **Alpha.** The API changes between releases, including in ways that will not
> be called out as breaking, because nothing here is stable yet. Cargo will not
> resolve a prerelease unless the requirement names one, so a plain `0.1`
> requirement does not pick this up: depend on the exact version below, and
> expect to edit it.

## Installation

Linux, including WSL, Rust 1.97.1 or later, and tmux 3.2a or later are required.
The executable and environment are captured when a server is built. Select a
specific executable with `ServerBuilder::tmux_executable`.

The package split is unreleased. Use this branch as a Git dependency:

```toml
[dependencies]
libtmux = { git = "https://github.com/libtmux/libtmux-rs", branch = "standard-library-core" }
```

The client uses Linux procfs and `/usr/bin/kill` to clean up process groups
before reaping their leaders. `ServerBuilder::group_signal_helper` can select
another absolute executable with the same arguments. A failed helper is
reported as `WaitChild`; descendant cleanup is then unproven.

The library exclusively owns its child processes. Do not reap them with
`waitpid(-1)`, or install `SIGCHLD=SIG_IGN` or `SA_NOCLDWAIT`. Kernel I/O can
delay child termination even after a deadline has expired.

## Create and inspect

```no_run
use libtmux::Server;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = Server::new()?;
    let session = server.new_session("work")?;
    let window = session.new_window("editor")?;
    let pane = window.active_pane()?.ok_or("window has no active pane")?;
    pane.send_line("cargo test")?;
    for line in pane.capture()? {
        println!("{}", line.to_string_lossy());
    }
    server.shutdown()?;
    Ok(())
}
```

`shutdown` closes the client's work; it does not kill the tmux sessions you
created. `with_session`, `with_window`, and `with_pane` pair resource creation
with cleanup. `ScopeError` preserves creation, operation, and cleanup failures.

For a runnable isolated version, use `test::TestServer` with `test-support`.
[The complete examples](examples/api/README.md) include private sockets,
cleanup, expected output, and standalone Cargo manifests.

## Cancellation and shutdown

A blocking call runs on the calling thread. Cancel from another thread with
`CancellationToken`; `Server::with_cancellation_token` makes a view that shares
the executor, request IDs, cached capabilities, and channel-wait state.

Cancellation of an already-running callback is cooperative. Resource-creation
scopes wait until they know the resource ID so they can clean it up. Queued
channel-lock scopes return on cancellation and release any later grant.

Call `shutdown` to observe cleanup failures. Dropping a handle alone leaves
tmux objects intact. A timeout or cancellation does not roll back an effect
that tmux already accepted; inspect the resulting state before retrying a
mutation.

`wait_for_channel` retains its client after a timeout because tmux cannot
withdraw a queued waiter without consuming a future signal. Another wait on
the same server view shares that client. `lock_channel` likewise releases an
abandoned grant when tmux eventually grants it. Shutdown ends parked clients.

## Control mode

Enable `control-mode` to attach `control::ControlMode` to a session. Split it
into a command sender and event receiver, subscribe to format changes, watch
pane output, or route the object model through `Server::over_control_mode`.

Reply correlation follows the completion of expanded commands, including
aliases and nested commands. Notifications and foreground replies stay
separate. Queue, line, reply, and output limits fail explicitly rather than
silently truncating a result. Snapshot boundaries identify which events came
after a capture.

Commands that hold tmux's command queue also delay following requests. Use
the subprocess channel APIs for parked waits and locks. A command that closes
a control connection before its completion boundary reports a closed
connection.

## Features

No feature is enabled by default. The default dependency graph contains only
`libtmux`; values, command execution, concrete handles, and errors use the
standard library. Optional features add the dependencies their capability
requires. `plan` and `control-mode` also work without crate dependencies.

| Feature | What it adds |
| --- | --- |
| `query` | Typed filtering, cardinality checks, and the portable query grammar |
| `plan` | Inspectable plans and execution budgets |
| `control-mode` | Persistent connections, notifications, subscriptions, and pane output |
| `derive` | `#[derive(Filterable)]` for your own types |
| `serde` | Versioned portable query and plan serialisation |
| `schema` | JSON Schema for portable queries and plans |
| `tracing` | Redacted command spans and cleanup diagnostics |
| `test-support` | Owned tmux fixtures and abandoned-fixture cleanup |
| `full` | All capabilities above except `test-support` |

## Errors and diagnostics

| Question | API |
| --- | --- |
| What kind of failure was this? | `Error::kind` |
| Did the object disappear? | `Error::is_object_gone` |
| Is retrying safe? | Inspect `Error::is_transient` and whether an effect was accepted |
| Which request failed? | Request ID and redacted command summary, when present |
| Did scoped cleanup also fail? | `ScopeError` |

The `tracing` feature records command outcomes. Diagnostics omit arguments
marked sensitive and raw process output; public arguments may appear in
bounded, escaped command summaries. Command spans carry request IDs scoped to
one server core. Errors from rejected control-mode input carry no request ID.
Returned results retain output for callers that need it.

## Further reading

- [Architecture](docs/design.md)
- [Package boundaries and migration](docs/packages.md)
- [Compatibility findings](docs/findings.md)
- [Python capability ledger](docs/parity.md)
- [Earlier alpha migrations](docs/migration.md)
