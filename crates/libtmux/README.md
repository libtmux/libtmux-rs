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
`libtmux = { version = "0.1.0-alpha.8", features = ["test-support"] }`.

Every accessor that reaches tmux is `async`; everything that reads an
already-taken snapshot is not. Commands run without a shell, and results keep
stdout and stderr as raw bytes, so decoding stays the caller's decision.

## Requirements and installation

Rust 1.85 or newer, tmux 3.2a or newer, and a Unix target. Native Windows is
unsupported because tmux is unavailable there; WSL works.

```toml
[dependencies]
libtmux = "0.1.0-alpha.8"
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

## Where to start, by what you came to do

| I want to | Call | Shown in |
| --- | --- | --- |
| Make a session and tear it down | `Server::with_session` | `examples/scratch.rs` |
| Reach a session someone left running | `Server::session`, `Server::sessions` | `examples/inspect.rs` |
| Find the pane my process is in | `Pane::from_env` | `examples/inspect.rs` |
| Add a window, split a pane | `Session::new_window`, `Pane::split` | `examples/scratch.rs` |
| Type into a pane | `Pane::send_keys`, `Pane::send_key_names` | `examples/scratch.rs` |
| Read what a pane printed | `Pane::capture`, `Pane::capture_with` | `examples/scratch.rs` |
| Wait for output rather than sleep | `Pane::wait_for_text`, `Pane::wait_for_quiet` | `examples/scratch.rs` |
| Follow a pane as it writes | `Pane::stream_output` | `examples/watch.rs` |
| Hear about changes as they happen | `ControlMode::attach`, `ControlSender::subscribe` | `examples/watch.rs` |
| Select panes by a typed expression | `Filterable::filter_fields`, `QueryIteratorExt::matching` | `examples/find.rs` |
| Send tmux something this crate has no method for | `Server::cmd` | below |
| Compare what each transport costs | `plan::Plan`, `plan::Planner` | `examples/matrix.rs` |
| Clean up fixtures a killed run left | `test::reap_abandoned_servers` | `examples/sweep.rs` |
| Tell a gone object from a gone link | `Error::is_object_gone`, `Error::is_transient` | `examples/recover.rs` |

Waiting needs no feature: `Pane::wait_for_text` and `Pane::wait_for_quiet` are
in the library, poll the scrollback with wrapped lines joined, and answer
`PaneWait::Dead` when the pane's process ends rather than running to the
deadline. `docs/design.md` records what they had to survive, and what a
control-mode doorbell was measured to buy.

## Waiting for something to happen

If the work can announce itself, do not poll. End it with
`tmux wait-for -S <channel>` and wait on that channel:

```rust
use libtmux::ChannelWait;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = libtmux::test::TestServer::builder().start().await?;
    let server = guard.server();

    // Stands in for the pane running `make; tmux wait-for -S build-done`.
    // Signalling first is safe: tmux keeps it for the next waiter.
    server.signal_channel("build-done").await?;

    match server.wait_for_channel("build-done", Duration::from_secs(60)).await? {
        ChannelWait::Signalled => println!("the build ended"),
        // Running out of time is an outcome, not an error, so this stays
        // distinguishable from tmux being unreachable.
        _ => println!("it is still going"),
    }

    guard.shutdown().await?;
    Ok(())
}
```

tmux keeps a signal nobody is waiting on, so the job finishing first does not
lose the race, and nothing polls. `examples/orchestrate.rs` runs three jobs
this way.

For a pane that was *not* written to announce itself, `Pane::wait_for_text`
does the same job without a channel:

```rust
use libtmux::{PaneWait, test::TestServer};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let session = guard.server().new_session("waiting").await?;
    let pane = session.panes().await?.remove(0);

    pane.send_keys("printf 'ready\n'").await?;
    pane.send_key_names(["Enter"]).await?;

    // Checked rather than discarded: a wait that reached its deadline still
    // returns successfully, so the outcome is the answer.
    assert_eq!(
        pane.wait_for_text("ready", Duration::from_secs(10)).await?,
        PaneWait::Arrived,
    );

    guard.shutdown().await?;
    Ok(())
}
```

What it handles, and what a loop written by hand usually does not: it looks
before it sleeps, so text already present is an answer rather than a wait; it
reads the scrollback with wrapped lines joined, so output that scrolled away
is still found and a needle spanning a wrap still matches; and it answers
`PaneWait::Dead` when the pane's process ends rather than holding the
deadline.

Three things that are not obvious:

- **Every command is already bounded.** A dispatch carries the server's
  `default_timeout`, 30 seconds unless you change it, so a hung tmux ends the
  call rather than the loop. The deadline above is for the *condition*, not the
  transport.
- **Polling costs a tmux process per tick.** With `control-mode` you can
  subscribe to a format instead and be told when it changes -- see
  `ControlSender::subscribe`. tmux coalesces those reports to at most once a
  second, so a subscription says what a value became, not every step it took.

The subscription form, which costs one connection rather than a process per
tick. It watches a format rather than the pane's text, so it suits "how many
windows are there now" better than "did this line appear":

```rust
# // `control` is behind `control-mode`, so the example is compiled only when
# // that feature is on. Without the guard this block fails to build under any
# // configuration lacking it, and `just doctest` runs `--all-features`, so
# // nothing would say so.
# #[cfg(not(feature = "control-mode"))]
# fn main() {}
# #[cfg(feature = "control-mode")]
use libtmux::control::{ControlMode, Event, Subscription};
# #[cfg(feature = "control-mode")]
use libtmux::test::TestServer;

# #[cfg(feature = "control-mode")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let session = guard.server().new_session("watching").await?;
    let (commands, mut events) = ControlMode::attach(guard.server(), session.id())
        .await?
        .split();

    commands
        .subscribe("windows", &Subscription::Session, "#{session_windows}")
        .await?;

    // The first report arrives without anything having changed, so a
    // subscription reads the value as well as watching it.
    let mut reports = 0;
    while let Some(event) = events.next_event().await {
        if let Event::SubscriptionChanged { name, value, .. } = event {
            println!("{} = {}", name.to_string_lossy(), value.to_string_lossy());
            reports += 1;
            if reports == 1 {
                break;
            }
        }
    }

    commands.unsubscribe("windows").await?;
    events.shutdown().await?;
    guard.shutdown().await?;
    Ok(())
}
```

- **`command_prompt` and `display_menu` wait for a person**, not for a
  condition, and the dispatch timeout is what ends that wait. They are not
  building blocks for this.

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

## When the typed API does not cover it

tmux has more commands than this crate has methods, and a method can be narrower
than the command behind it. `Server::cmd` runs anything, through the same
transport, timeout and error classification as the typed calls -- so reaching
for it costs you the method, not the machinery.

```rust
use libtmux::{Command, test::TestServer};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let server = guard.server();
    let session = server.new_session("escape-hatch").await?;
    let pane = session.panes().await?.remove(0);

    // `Pane::clock_mode` has no typed way back out for every mode, so this is
    // how you leave one the API does not model.
    server
        .cmd(
            Command::new("copy-mode")
                .arg("-q")
                .arg("-t")
                .arg(pane.id().to_string()),
        )
        .await?;

    // The answer carries tmux's own bytes. A command tmux refuses is an `Err`
    // classified the same way a typed call's would be.
    let version = server
        .cmd(Command::new("display-message").arg("-p").arg("#{version}"))
        .await?;
    assert!(!version.stdout().is_empty());

    guard.shutdown().await?;
    Ok(())
}
```

A command you needed here is worth reporting: `tmux-mcp` and `tmux-workspace`
use this crate from outside it, and when either reaches for `cmd` that counts as
a gap in the API rather than a use of the escape hatch.

## What tmux is made of

```text
Server ──┬── Session ──── Window ──── Pane
         └── Client ─────┘
```

| Type | Held by | Identified by | Notes |
| --- | --- | --- | --- |
| `Server` | a socket | its socket path | one daemon; `Server::new()` finds the default |
| `Session` | the server | `$0` | what a client attaches to |
| `Window` | *linked into* sessions | `@0` | one window can be linked into several sessions at once |
| `Pane` | exactly one window | `%0` | holds the process and the scrollback |
| `Client` | the server | its tty | attached to one session at a time |

The third row is the one that surprises people, and it decides API shape. A
window has one identity and several *links*, so a command that removes one link
has to name the session too -- `session:@id`, because `@id` alone could not say
which link to remove, and a bare index names a slot rather than whichever
window is sitting in it. [`Window::unlink`] does exactly that, and reports
[`Error::LinkGone`] rather than [`Error::ObjectGone`] when the link is already
gone, because the window itself may still be running under another session.

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
use std::time::Duration;

use libtmux::query::{Filterable as _, QueryIteratorExt as _};
use libtmux::test::{TestServer, retry_until};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let server = guard.server();
    server.new_session("work").await?;

    let fields = libtmux::Pane::filter_fields();

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

The block below shows the shape against a healthy server, so its interesting
arms do not run. `examples/recover.rs` makes each failure happen instead --
including the one that costs people, where a link is gone and the window it
pointed at is still running.

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
libtmux = { version = "0.1.0-alpha.8", features = ["test-support"] }
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

## If you know the Python libtmux

This is a port of it, and the object model is the same one: a server holding
sessions, windows linked into them, panes inside those. Code you have written
against the Python library will read as familiar here. Four things differ, and
they are the ones worth knowing before you start.

| | Python `libtmux` | this crate |
| --- | --- | --- |
| Calling | synchronous throughout | `async` throughout; `blocking::Runtime` drives it from code that is not |
| Pane text | `capture_pane` returns `list[str]` | `capture` returns `TmuxText`, which keeps bytes no `String` would hold |
| Filtering | `QueryList` lives in `_internal` | `query` is public, and field handles are generated per type |
| Transport | one tmux process per command | the same by default, plus control mode and command chaining |

One thing differs, and in this crate's favour: waiting. The Python library
keeps `retry_until` in `libtmux/test/retry.py`, a test helper, so production
code that waits on a pane writes the loop itself. `Pane::wait_for_text` and
`Pane::wait_for_quiet` are here in the library and need no feature.

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
