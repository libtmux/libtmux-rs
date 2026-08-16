# libtmux

[![crates.io]][crate] [![docs.rs]][docs] [![MSRV]][rust-1.85]

[crates.io]: https://img.shields.io/crates/v/libtmux.svg
[crate]: https://crates.io/crates/libtmux
[docs.rs]: https://img.shields.io/docsrs/libtmux
[docs]: https://docs.rs/libtmux
[MSRV]: https://img.shields.io/badge/rustc-1.85+-lightgray.svg
[rust-1.85]: https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/

**Drive tmux from Rust: typed, async control over servers, sessions, windows,
and panes.**

> **Alpha.** The API changes between releases, including in ways that will not
> be called out as breaking, because nothing here is stable yet. Cargo will not
> resolve a prerelease unless the requirement names one, so a plain `0.1`
> requirement does not pick this up: depend on the exact version below, and
> expect to edit it.

```rust
use libtmux::test::TestServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Runs for real. `TestServer` is an isolated tmux on its own socket under
    // `/tmp/libtmux-rs-test/`, torn down at the end. Your own code says
    // `let server = libtmux::Server::new()?;` instead; nothing else changes.
    let guard = TestServer::new().await?;
    let server = guard.server();

    let session = server.new_session("work").await?;
    let window = session.new_window("editor").await?;
    let pane = window.active_pane().await?.expect("a window has a pane");

    pane.send_keys("echo hello").await?;
    pane.send_key_names(["Enter"]).await?;

    for line in pane.capture().await? {
        println!("{}", line.to_string_lossy());
    }

    guard.shutdown().await?;
    Ok(())
}
```

The examples on this page run as written, against a throwaway tmux. To run
them yourself, enable the `test-support` feature as a dev-dependency:
`libtmux = { version = "0.1.0-alpha.6", features = ["test-support"] }`.

Every accessor that reaches tmux is `async`; everything that reads an
already-taken snapshot is not. Commands run without a shell, and results keep
stdout and stderr as raw bytes, so decoding stays the caller's decision.

## Requirements and installation

Rust 1.85 or newer, tmux 3.2a or newer, and a Unix target. Native Windows is
unsupported because tmux is unavailable there; WSL works.

```toml
[dependencies]
libtmux = "0.1.0-alpha.6"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Async operations need an entered Tokio runtime. The default executable is
`tmux`, resolved through the `PATH` captured when the `Server` is built; use
`ServerBuilder::tmux_executable` to select another.

**Minimum supported Rust version.** 1.85, the first compiler to ship Edition
2024. It is checked in CI against the whole test suite, not just a build. A
raise is a minor version bump and is called out in the release notes, so a
patch release never moves it.

## Features

`query` is the only one on by default. `full` turns on every capability
below, so a caller who wants them all does not have to list them.

| Feature | What it adds |
| --- | --- |
| `query` | Typed filter expressions over listings. On by default |
| `plan` | Recording tmux work before running it, and choosing what it costs |
| `control-mode` | One persistent tmux connection, so the server reports changes as they happen rather than being polled |
| `blocking` | A runtime for calling from code that is not async |
| `derive` | `#[derive(Filterable)]`, for filtering your own structs with the same expressions |
| `serde` | Versioned serialization for `FilterExpr<T>`, for sending expressions over a wire |
| `tracing` | Sanitized command instrumentation |
| `test-support` | The real-tmux test guard, for your own tests |
| `full` | Every capability above, but not `test-support` |

## Choosing how commands reach tmux

Three switches decide what a run costs and what it can prove. They compose:
async is the API, control mode is the transport, and chaining is what a
subprocess transport does instead of having one.

### What each one does

| Switch | Off (the default) | On |
| --- | --- | --- |
| **Async** | Nothing: every method is already `async`. `blocking::Runtime` is a runtime you drive them from, not a second API to keep in step | `blocking::Runtime::new()?.run(future)` for scripts and tests |
| **Control mode** | One tmux process per command | One connection for every command, and tmux reports changes as they happen |
| **Chaining** | One tmux process per command | Neighbouring commands share one process, trading the ability to say which one failed |

Measured on one workload of six operations, all leaving identical tmux state:

| Mode | Processes | Attribution on failure |
| --- | --- | --- |
| One command per invocation | 6 | names the failing command |
| Folded into shared invocations | 3 | `Unknown` -- tmux reports one status for the group |
| Control mode | 1 | names the failing command |

Control mode is the only one that buys back the processes without giving up
the answer, because its `%begin`/`%end` blocks are per command. Folding is
what a subprocess transport offers instead of having that. Run
`cargo run --example matrix --features full,test-support` to reproduce the
table on your own machine.

### How to turn each one on

| Switch | Cargo feature | In code |
| --- | --- | --- |
| Async | none | already the default; every method is `async` |
| Blocking runtime | `blocking` | `libtmux::blocking::Runtime::new()?` |
| Control mode | `control-mode` | `ControlMode::attach(&server, session).await?` |
| Chaining, by hand | none | `CommandChain::new(a).then(b)`, then `server.chain(chain).await?` |
| Chaining, by planner | `plan` | `plan.run(&server, Planner::Folding).await?` |
| Folding a pane creation in too | `plan` | `Planner::Marked` |
| Never folding across your own work | `plan` | `Planner::steps_bounded(&plan, &boundaries)` |
| Per-command answers over one connection | `plan`, `control-mode` | `plan.run_over_control_mode(&sender).await?` |

Control mode is never the default transport, and turning the feature on does
not make it one: normal commands stay one process per command until you attach
a connection and use it.

## Walking the hierarchy

`Server::hierarchy` gathers the whole tree in three tmux commands, rather than
one per object:

```rust
use libtmux::test::TestServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let server = guard.server();
    server.new_session("work").await?;

    for branch in server.hierarchy().await? {
        println!("{}", branch.session.name().to_string_lossy());

        for window in &branch.windows {
            println!("  {}", window.window.name().to_string_lossy());

            for pane in &window.panes {
                println!("    {pane}");
            }
        }
    }

    guard.shutdown().await?;
    Ok(())
}
```

Listings come in pairs, and the short name is the honest one. `sessions()`
returns `Result<Vec<Session>>`, so an unreachable tmux is an error rather than
an empty list. `sessions_or_empty()` collapses failure into no rows, which
suits a status line and nothing that reconciles state -- a reconciler reading
"no sessions" from an outage will happily delete everything.

## Filtering

Typed field handles build an expression without accepting an untyped field name
or value, so a comparison that has no meaning for a field does not compile:

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

    let active_shell = fields
        .pane_current_command
        .starts_with("sh")
        .and(fields.pane_active.eq(true));

    assert_eq!(panes.iter().matching(&active_shell).count(), 1);

    guard.shutdown().await?;
    Ok(())
}
```

A question about what a session *contains* needs the shape that holds its
windows, so `SessionTree` and `WindowTree` carry relations:

```rust
use libtmux::query::{Filterable as _, QueryIteratorExt as _};
use libtmux::test::TestServer;
use libtmux::{SessionTree, WindowTree};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let server = guard.server();

    let ci = server.new_session("ci").await?;
    ci.new_window("build").await?;
    server.new_session("idle").await?;

    let sessions = SessionTree::filter_fields();
    let windows = WindowTree::filter_fields();
    let building = sessions.windows.any(windows.window.window_name.eq("build"));

    let names: Vec<_> = server
        .hierarchy()
        .await?
        .iter()
        .matching(&building)
        .map(|branch| branch.session.name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["ci"]);

    guard.shutdown().await?;
    Ok(())
}
```

With `serde`, an expression lowers to a versioned JSON envelope, so a CLI, an
MCP server, or a config file can carry one.

## Text from tmux is bytes

tmux permits names, titles, and pane contents that are not valid UTF-8, so they
arrive as `TmuxText` rather than `String`. There is no implicit conversion:

```rust
let text = libtmux::TmuxText::from("editor");

assert_eq!(text.as_bytes(), b"editor");
assert_eq!(text.as_str().expect("valid UTF-8"), "editor");
assert_eq!(text.to_string_lossy(), "editor");
```

## Failures say what to do about them

`Error` has a variant per failure mode; `Error::kind` reduces those to the
decision a caller makes. `Error::is_object_gone` is the branch most programs
write, because an object disappearing is an ordinary race rather than a failed
request.

```rust
use libtmux::test::TestServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let session = guard.server().new_session("work").await?;

    match session.windows().await {
        Ok(windows) => println!("{} windows", windows.len()),
        Err(error) if error.is_object_gone() => println!("gone"),
        Err(error) if error.is_transient() => println!("retry: {error}"),
        Err(error) => return Err(error.into()),
    }

    guard.shutdown().await?;
    Ok(())
}
```

## Cancellation and shutdown

Each command runs in an isolated process group with a supervised deadline,
30 seconds by default and configurable through `ServerBuilder::default_timeout`.
Dropping the command future, reaching its timeout, or shutting the server down
signals the group and waits for the direct child while the runtime is alive.

`Server::shutdown()` is shared by all clones: it cancels active work, rejects
later commands, and is safe to call concurrently or repeatedly. Await it, or
await your commands, before tearing the runtime down — runtime destruction
signals best-effort but cannot promise that child reaping finished.

## Testing against real tmux

`test-support` exports `libtmux::test::TestServer`, the same guard this crate's
own suite uses:

```toml
[dev-dependencies]
libtmux = { version = "0.1.0-alpha.6", features = ["test-support"] }
```

Each guard owns a tmux child on a private socket with an empty config, so tests
cannot reach your real server or each other. `shutdown().await` closes escaped
clients, waits the daemon, and reports cleanup failures; `Drop` forces
best-effort cleanup even after the runtime has ended. On Linux, cleanup also
sweeps processes by an exact environment marker through pidfds, so PID reuse
cannot redirect a signal.

The guarantees and their limits are set out in `docs/design.md`, which ships
with the crate.

## Compatibility

Every supported tmux release is built from source in CI and runs the whole
workspace: 3.2a, 3.4, 3.5a, 3.6b, and 3.7b. The floor and the ceiling matter
equally — 3.4 and 3.5a are the releases that wrap command output differently,
which is why the format codec carries a second dialect.

## Documentation

[API documentation](https://docs.rs/libtmux) covers the public surface. Two
longer documents ship inside the crate, next to the source:

- `docs/design.md` — why the crate is shaped the way it is: the transport, the
  snapshot and format boundary, the query grammar, the test guard, and the
  compatibility lanes.
- `docs/parity.md` — the capability ledger against Python libtmux, naming each
  Rust symbol and the test that exercises it.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE)
or [MIT license](LICENSE-MIT) at your option.
