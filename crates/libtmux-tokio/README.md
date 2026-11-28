<!-- libtmux-logo -->
<div align="center">
  <picture>
    <source srcset="https://raw.githubusercontent.com/libtmux/libtmux-rs/master/crates/libtmux-tokio/assets/logo.svg" type="image/svg+xml">
    <img src="https://raw.githubusercontent.com/libtmux/libtmux-rs/master/crates/libtmux-tokio/assets/logo.png" width="128" height="128" alt="libtmux for Rust">
  </picture>
</div>
<!-- /libtmux-logo -->

<div align="center">

# libtmux-tokio

[![crates.io]][crate] [![docs.rs]][docs] [![MSRV]][rust-1.85]
[![libtmux.org](https://img.shields.io/badge/docs-libtmux.org-0b7285)](https://libtmux.org/en/rs/)

[crates.io]: https://img.shields.io/crates/v/libtmux-tokio.svg?label=libtmux-tokio
[crate]: https://crates.io/crates/libtmux-tokio
[docs.rs]: https://img.shields.io/docsrs/libtmux-tokio
[docs]: https://docs.rs/libtmux-tokio
[MSRV]: https://img.shields.io/badge/rustc-1.85+-lightgray.svg
[rust-1.85]: https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/

**Drive tmux from Rust: typed, async control over servers, sessions, windows,
and panes.**

</div>

[Type into a pane][`Pane::send_line`] and [read what it printed][`Pane::capture`],
[wait][`Pane::wait_for_text`] for an answer instead of sleeping,
[find][`Server::hierarchy`] and [filter][`Filterable::filter_fields`] what a
server holds, and [hear about changes][`ControlSender::subscribe`] as tmux
makes them.

It suits tools that drive a terminal program, test suites that need a real
tmux, and agents that share a terminal.

> Releases are numbered `0.1.N`, and each release increments `N`. The API is
> not settled: any release may change or remove exported identifiers, and the
> changelog marks each break. Pin an exact version to avoid one.

`libtmux-tokio` is unpublished. Use this checkout for the API below.
Published `libtmux` 0.1.2 has the earlier root async API and Tokio-backed
blocking bridge; its [released guide][released-guide] covers that API.

See the [migration notes](docs/migration.md) for the native driver split.
[`libtmux-macros`][pkg-macros], [`tmux-workspace`][pkg-workspace] and
[`tmux-mcp`][pkg-mcp] are also in the workspace, under
[Packages](#packages).

[released-guide]: https://github.com/libtmux/libtmux-rs/blob/1027012583abdc8b07ea0fd071e0c6cacb051278/README.md

**Contents** — [Quick start](#quick-start) ·
[Where to start](#where-to-start) ·
[What tmux is made of](#what-tmux-is-made-of) ·
[Find things](#find-sessions-windows-and-panes) ·
[Type and wait](#type-into-a-pane-and-wait) ·
[Watch tmux](#watch-tmux-change) · [Filtering](#filtering) ·
[Failures](#failures-say-what-to-do-about-them) ·
[Choosing a transport](#choosing-how-commands-reach-tmux) ·
[Cancellation](#cancellation-and-shutdown) ·
[Raw commands](#when-the-typed-api-does-not-cover-it) ·
[Bytes](#text-from-tmux-is-bytes) · [Testing](#testing-against-real-tmux) ·
[Features](#features) · [Python libtmux](#if-you-know-the-python-libtmux) ·
[Packages](#packages) · [Leaves out](#what-it-leaves-out) ·
[Support](#support-and-status) · [Documentation](#documentation)

## Quick start

Connect to a running tmux server by passing its absolute socket path. From a
binary project next to the `libtmux-rs` checkout, add the client and Tokio:

```toml
[dependencies]
libtmux-tokio = { path = "../libtmux-rs/crates/libtmux-tokio" }
tokio = { version = "1", features = ["rt", "time"] }
```

Save this as `src/main.rs`. It prints the session names and confirms client
cleanup. It borrows the daemon and leaves its sessions alone.

<!-- tokio-example -->

```rust,no_run
use std::io::{self, Write};
use libtmux_tokio::{Error, Server};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = std::env::args_os().nth(1).ok_or("pass an absolute socket path")?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let sessions = runtime.block_on(async {
        let server = Server::builder().socket_path(socket).build()?;
        Error::after_cleanup(server.sessions().await, server.shutdown().await)
    })?;
    let mut stdout = io::stdout().lock();
    for session in sessions {
        stdout.write_all(session.name().as_bytes())?;
        stdout.write_all(b"\n")?;
    }
    Ok(())
}
```

<!-- /tokio-example -->

Run `cargo run -- /absolute/path/to/socket` with an endpoint you already use.
The program creates no daemon or session and needs no `test-support` feature.

### Isolated servers for tests

`test-support` supplies an isolated server for the test examples below:

```toml
[dependencies]
libtmux-tokio = { path = "../libtmux-rs/crates/libtmux-tokio", features = ["test-support"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Create a session, type into its pane, and print what the pane showed:

```rust
use libtmux_tokio::test::TestServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Runs for real. `TestServer` is an isolated tmux on its own socket under
    // `/tmp/libtmux-rs-test/`, torn down at the end.
    let guard = TestServer::new().await?;
    let server = guard.server();

    let receipt = server.new_session("work").await?;
    let session = server.inspect_session(&receipt).await?;
    let window = session.new_window("editor").await?;
    let pane = window.active_pane().await?.expect("a window has a pane");

    pane.send_line("echo hello").await?;

    for line in pane.capture().await? {
        println!("{}", line.to_string_lossy());
    }

    guard.shutdown().await?;
    Ok(())
}
```

- **Throwaway tmux.** The fixture examples use their own private sockets.
- **Your own server.** Use the supplied-endpoint program above for normal
  connections. `Server::shutdown` closes the client without killing the daemon.
- **Which server.** With neither `LIBTMUX_SOCKET_PATH` nor `LIBTMUX_SOCKET`
  set, `Server::new` names no socket and tmux chooses the server.

[`Pane::send_line`] types a line and presses Enter, and [`Pane::capture`]
reads what the pane shows. Every method that reaches tmux is `async`. Reading a
snapshot you already hold is not, so walking it costs no round trips.

Commands run without a shell. Results keep stdout and stderr as raw bytes, so
decoding is your call.

It needs Rust 1.85 or newer and tmux 3.3a or newer, on a Unix host. [Release
candidates][tmux-rc] are not supported.

Against an older daemon, an operation returns
[`Error::UnsupportedTmuxVersion`] before it dispatches. Upgrade tmux and
restart its server.

Some operations need a newer tmux:

| Operation | tmux |
| --- | --- |
| Named-buffer deletion or rename | 3.4 |
| Capture flags `-T` and `-F` | 3.4 and 3.7 |
| Captured `run-shell` output | 3.5 |
| Control mode: attach, pane streams, subscriptions | 3.7 |

[The contract][contract-narrow] lists every operation a release refuses, and
what [a command nested in another command][contract-nested] can do to the
server. [The version check][contract-version] covers unrecognized versions and
failed probes.

Async operations need an entered Tokio runtime. The default executable is
`tmux`, found on the `PATH` captured when the [`Server`] is built; select
another with [`ServerBuilder::tmux_executable`].

## Where to start

By what you came to do:

| I want to | Call | Shown in |
| --- | --- | --- |
| Make a session and tear it down | [`Server::with_session`] | `examples/scratch.rs` |
| Reach a session someone left running | [`Server::session`], [`Server::sessions`] | `examples/inspect.rs` |
| Find the pane my process is in | [`Pane::from_env`] | `examples/inspect.rs` |
| Add a window, split a pane | [`Session::new_window`], [`Pane::split`] | `examples/scratch.rs` |
| Type into a pane | [`Pane::send_line`], [`Pane::send_keys`], [`Pane::send_key_names`] | `examples/scratch.rs` |
| Read what a pane printed | [`Pane::capture`], [`Pane::capture_with`] | `examples/scratch.rs` |
| Wait for output rather than sleep | [`Pane::wait_for_text`], [`Pane::wait_for_quiet`] | `examples/scratch.rs` |
| Follow a pane as it writes | [`Pane::stream_output`] | `examples/watch.rs` |
| Hear about changes as they happen | [`ControlMode::attach`], [`ControlSender::subscribe`] | `examples/watch.rs` |
| Select panes by a typed expression | [`Filterable::filter_fields`], [`QueryIteratorExt::matching`] | `examples/find.rs` |
| Send tmux something this crate has no method for | [`Server::cmd`] | below |
| Compare what each transport costs | [`plan::Plan`], [`plan::Planner`] | `examples/matrix.rs` |
| Clean up fixtures a killed run left | [`test::reap_abandoned_servers`] | `examples/sweep.rs` |
| Tell a gone object from a gone link | [`Error::is_object_gone`], [`Error::is_transient`] | `examples/recover.rs` |

Waiting needs no feature: [`Pane::wait_for_text`] and [`Pane::wait_for_quiet`]
are in the library. [`docs/findings.md`][findings] records what they had to
survive.

## What tmux is made of

```text
Server ──┬── Session ──── Window ──── Pane
         └── Client ─────┘
```

| Type | Held by | Identified by | Notes |
| --- | --- | --- | --- |
| [`Server`] | a socket | its socket path | one daemon; [`Server::new`] passes tmux no socket unless one is named |
| [`Session`] | the server | `$0` | what a client attaches to |
| [`Window`] | *linked into* sessions | `@0` | one window can be linked into several sessions at once |
| [`Pane`] | exactly one window | `%0` | holds the process and the scrollback |
| [`Client`] | the server | its tty | attached to one session at a time |

[`Server::new`] and [`Server::builder`] choose the socket in this order:

1. The one you name in code.
2. `LIBTMUX_SOCKET_PATH`, as `-S`.
3. `LIBTMUX_SOCKET`, as `-L`.
4. None: tmux picks, which is the server `TMUX` names inside tmux, else its
   default socket. `Server::from_env` names the `TMUX` socket as `-S` instead.

A window has one identity and several *links*:

- `window.link().unlink().await?` removes the placement at its observed
  session and index. The guard checks which window still occupies that index.
- [`Error::LinkGone`][error-linkgone] means the placement disappeared.
  [`Error::ObjectGone`][error-objectgone] means the window itself has gone.

## Find sessions, windows, and panes

### [`Server::hierarchy`]: the whole tree with three listings

[`Server::hierarchy`] first discovers the daemon generation, then gathers
sessions, windows, and panes with three listings fenced to it.

After the initial version check, an unbound handle uses four dispatches.
A bound handle reuses its generation and needs three. The count does not
grow with the number of objects.

[`Server::sessions`] and [`Session::windows`][session-windows] are its narrower
siblings:

```rust
use libtmux_tokio::test::TestServer;

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

The accessors in the loop are not `async`: the result is a snapshot, so
reading names from it costs no round trips.

[`Server::sessions`] returns `Result<Vec<Session>>`:

- An unreachable tmux is an error. A running tmux with no sessions is an empty
  list.
- `sessions().await.unwrap_or_default()` suits a status line.
- A reconciler that reads "no sessions" from an outage would delete
  everything, so it should match on the error.

[The contract][contract-errors] has more.

## Type into a pane and wait

### [`Server::wait_for_channel`]: wait for work that announces itself

If the work can announce itself, [do not poll]. End it with
`tmux wait-for -S <channel>`, which [`Server::signal_channel`] sends, and wait
on that channel:

```rust
use libtmux_tokio::ChannelWait;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = libtmux_tokio::test::TestServer::builder().start().await?;
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
lose the race, and nothing polls. A wait that runs out of time keeps its client
on the channel, so the signal is there for the next wait.

It prints `the build ended`. Running out of time is an outcome
([`ChannelWait`]), and tmux being unreachable is an error.
[`examples/orchestrate.rs`][example-orchestrate] runs three jobs this way.

### [`Pane::wait_for_text`]: wait for text a pane prints

For a pane that was *not* written to announce itself, [`Pane::wait_for_text`]
does the same job without a channel:

```rust
use libtmux_tokio::{PaneWait, test::TestServer};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let session = guard.session("waiting").await?;
    let pane = session.panes().await?.remove(0);

    pane.send_line("printf 'ready\n'").await?;

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

It prints nothing, and the assertion is the check: a wait that reached its
deadline still returns successfully, so [`PaneWait`] is the answer.

- **It looks before it sleeps,** so text already present is an answer rather
  than a wait.
- **It reads the scrollback with wrapped lines joined,** so output that
  scrolled away is still found and a needle spanning a wrap still matches.
- **It answers [`PaneWait::Dead`]** when the pane's process ends, rather than
  holding the deadline. The answer carries no text, and a pane whose process
  has ended is destroyed unless `remain-on-exit` is on, so there is usually
  nothing left to capture. Set `remain-on-exit` on the pane if you need its
  last output, then call [`Pane::capture`] after the `Dead` answer.

A tick is 120ms and costs a tmux process. The deadline above is for the
*condition*; the server's [`default_timeout`][default-timeout] bounds each
command.

[`Pane::wait_for_quiet`] waits for a pane to stop writing. `command_prompt` and
`display_menu` wait for a person, so the dispatch timeout is what ends them.

## Watch tmux change

### [`ControlSender::subscribe`]: have tmux watch a value

With `control-mode`, a subscription costs one connection rather than a process
per tick. tmux evaluates a format and reports it when its value changes. It
suits "how many windows are there now", not "did this line appear":

```rust
# // `control` is behind `control-mode`, so the example is compiled only when
# // that feature is on. Without the guard this block fails to build under any
# // configuration lacking it, and `just doctest` runs `--all-features`, so
# // nothing would say so.
# #[cfg(not(feature = "control-mode"))]
# fn main() {}
# #[cfg(feature = "control-mode")]
use libtmux_tokio::control::{ControlMode, Event, Subscription};
# #[cfg(feature = "control-mode")]
use libtmux_tokio::test::TestServer;

# #[cfg(feature = "control-mode")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
# let version = guard.server().capabilities().await?.tmux_version().clone();
# let eligible = version.has_behavior(&libtmux_tokio::since::CONTROL_ACQUISITION);
# if !eligible {
#     guard.shutdown().await?;
#     return Ok(());
# }
    let session = guard.session("watching").await?;
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
        if let Event::SubscriptionChanged { name, value, .. } = event? {
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

The first report arrives without anything having changed, so a subscription
reads the value as well as watching it. tmux coalesces reports to at most once
a second: a subscription says what a value became, and skips the steps between.

To follow a pane as it writes, use [`Pane::stream_output`].
[`examples/watch.rs`][example-watch] runs both.

## Filtering

### [`Filterable::filter_fields`]: a comparison that means nothing does not compile

[Typed field handles] build an expression without accepting an untyped field
name or value, so a comparison that has no meaning for a field does not
compile:

```rust
use std::time::Duration;

use libtmux_tokio::query::{Filterable as _, QueryIteratorExt as _};
use libtmux_tokio::test::{TestServer, retry_until};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let server = guard.server();
    server.new_session("work").await?;

    let fields = libtmux_tokio::Pane::filter_fields();

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

A question about what a session *contains* needs the shape that holds its
windows, so [`SessionTree`] and [`WindowTree`] carry relations:

```rust
use libtmux_tokio::query::{Filterable as _, QueryIteratorExt as _};
use libtmux_tokio::test::TestServer;
use libtmux_tokio::{SessionTree, WindowTree};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let server = guard.server();

    let receipt = server.new_session("ci").await?;
    let ci = server.inspect_session(&receipt).await?;
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

With `serde`, an expression lowers to a [versioned JSON envelope], so a CLI, an
MCP server, or a config file can carry one.

## Failures say what to do about them

### [`Error::kind`]: the decision a caller makes

[`Error`] has a variant per failure mode, and [`Error::kind`] reduces those to
the decision a caller makes:

- **[`Error::is_object_gone`]** is the branch most programs write, because an
  object disappearing is an ordinary race and the request was fine.
- **[`Error::is_transient`]** means the same call can be repeated without
  duplicating an effect. A timeout is `false`, because tmux may have completed
  the command before its caller stopped waiting.
- **[`Error::AfterEffect`]**, classified as [`ErrorKind::PartialEffect`], means
  tmux accepted one effect of a composed call before a later step failed.
  Replay may repeat the accepted effect.

```rust
use libtmux_tokio::test::TestServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let session = guard.session("work").await?;

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

Against a healthy server the interesting arms do not run.
[`examples/recover.rs`][example-recover] makes each failure happen, including
the costly one: a link is gone and its window is still running.
[The contract][contract-errors] has the details.

## Choosing how commands reach tmux

Control mode and chaining decide what a run costs and what it can prove.
The Tokio adapter can route a typed call through a subprocess or a control connection.

### What each one does

| Switch | Off (the default) | On |
| --- | --- | --- |
| **Control mode** | One tmux process per command | Eligible commands share a connection that also receives tmux notifications |
| **Chaining** | One tmux process per command | Neighbouring commands share one process, trading the ability to say which one failed |

The matrix runs six operations against a private tmux server, then counts the
panes and windows. This is one subprocess run. The wall times vary by machine.

<!-- example-output: matrix --package libtmux-tokio --all-features -- --baseline -->

```text
mode                     feature            dispatches processes      wall  attribution  query output
----------------------------------------------------------------------------------------------------------------------
async/sequential         plan                        6         7     19ms  per-command  2 panes, 2 windows, 2 active
async/folded             plan                        3         4     14ms  merged       2 panes, 2 windows, 2 active
async/marked-fold        plan                        3         4     13ms  merged       2 panes, 2 windows, 2 active

every mode built the same thing: true
dispatches ranged 3..6, processes ranged 4..7

the same failure, dispatched two ways:
  Sequential   1 dispatch(es) -> [Failed, Skipped]
  Folding      1 dispatch(es) -> [Unknown, Unknown]
  Sequential names the failing operation; Folding cannot, because tmux
  reports one status for the group whichever member failed.
```

Run `cargo run -p libtmux-tokio --all-features --example matrix` to include
control mode on a qualified daemon. The process count includes qualification
probes and the attached client.

### How to turn each one on

| Switch | Cargo feature | In code |
| --- | --- | --- |
| Async | none | already the default; every method is `async` |
| Control mode | `control-mode` | `ControlMode::attach(&server, session).await?` |
| Chaining, by hand | none | [`CommandChain::new(a).then(b)`], then `server.chain(chain).await?` |
| Chaining, by planner | `plan` | `server.run_plan(&plan, Planner::Folding).await?` |
| Folding a pane creation in too | `plan` | [`Planner::Marked`][`plan::Planner`] |
| Never folding across your own work | `plan` | `Planner::steps_bounded(&plan, &boundaries)` |
| Per-command answers over one connection | `plan`, `control-mode` | `sender.run_plan(&plan).await?` |

### [`Server::over_control_mode`] shares an attached control client

The handle selects a transport before it submits a command. Numeric
projections and input commands can share the control client. Reading pane
contents or waiting on a channel uses an ordinary client on the same daemon.

```rust
# // `over_control_mode` is behind `control-mode`, so the example is compiled
# // only when that feature is on.
# #[cfg(not(feature = "control-mode"))]
# fn main() {}
# #[cfg(feature = "control-mode")]
use libtmux_tokio::control::ControlMode;
# #[cfg(feature = "control-mode")]
use libtmux_tokio::test::TestServer;

# #[cfg(feature = "control-mode")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
# let version = guard.server().capabilities().await?.tmux_version().clone();
# if !version.has_behavior(&libtmux_tokio::since::CONTROL_ACQUISITION) {
#     guard.shutdown().await?;
#     return Ok(());
# }
    let session = guard.session("routed").await?;

    let (commands, events) = ControlMode::attach(guard.server(), session.id())
        .await?
        .split();
    let routed = guard.server().over_control_mode(&commands).await?;

    for pane in routed.panes().await? {
        pane.send_line("echo hello").await?;
    }

    events.shutdown().await?;
    guard.shutdown().await?;
    Ok(())
}
```

An entire chain uses the ordinary route when any member can park the client,
return ambiguous text or require bytes outside a control line. Give it an
explicit target when it depends on the selected session or pane.

## Cancellation and shutdown

### [`ServerBuilder::default_timeout`]: every command is bounded

Each subprocess command runs in an isolated process group with a supervised
deadline, 30 seconds by default. Dropping the command future, reaching its
timeout, or shutting the server down signals the group.

The signal reaches the tmux client, and a command the server has already
received still runs. A dropped mutation may or may not have happened, and a
retry can repeat it.

### Dropping a future: a deadline for one call

A caller that wants a shorter deadline for one command does not need an API for
it. Dropping the future is what signals the group, so wrapping the call is a
per-call deadline with the cleanup already attached:

```rust
use libtmux_tokio::Command;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = libtmux_tokio::test::TestServer::new().await?;
    let server = guard.server();

    // Ends at 400ms rather than the server's 30-second default, and the
    // client's process group is signalled and reaped on the way out. The
    // `sleep` runs under the tmux server, so it finishes regardless.
    let bounded = tokio::time::timeout(
        Duration::from_millis(400),
        server.cmd(Command::new("run-shell").arg("sleep 3")),
    )
    .await;
    assert!(bounded.is_err());

    // The server is still usable: a bounded call is not a broken connection.
    server.sessions().await?;

    guard.shutdown().await?;
    Ok(())
}
```

### [`Server::shutdown()`]: end everything

It cancels active subprocess work and persistent control connections, rejects
later commands and attaches, and is safe to call concurrently or repeatedly.
Await it, or await your commands, before tearing the runtime down.
[The contract][contract-cancellation] has the control-mode details.

## When the typed API does not cover it

### [`Server::cmd`]: any tmux command, through the same machinery

tmux has more commands than this crate has methods, and a method can be
narrower than the command behind it. [`Server::cmd`] uses the same transport
and timeout as typed calls. A tmux refusal remains a nonzero
[`CommandResult`], so check `success()`; version guards and transport failures
return `Err`:

```rust
use libtmux_tokio::{Command, test::TestServer};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let server = guard.server();
    let receipt = server.new_session("escape-hatch").await?;
    let session = server.inspect_session(&receipt).await?;
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

    // The answer carries tmux's own bytes and exit status.
    let version = server
        .cmd(Command::new("display-message").arg("-p").arg("#{version}"))
        .await?;
    assert!(!version.stdout().is_empty());

    guard.shutdown().await?;
    Ok(())
}
```

A command you needed here is worth reporting: `tmux-mcp` and `tmux-workspace`
use this crate from outside it, and when either reaches for `cmd` that counts
as a gap in the API.

## Text from tmux is bytes

### [`TmuxText`]: names, titles, and contents as bytes

tmux permits names, titles, and pane contents that are not valid UTF-8, so they
arrive as [`TmuxText`] rather than `String`. There is no implicit conversion:

```rust
use libtmux_tokio::TmuxText;

let text = TmuxText::from("editor");

assert_eq!(text.as_bytes(), b"editor");
assert_eq!(text.as_str().expect("valid UTF-8"), "editor");
assert_eq!(text.to_string_lossy(), "editor");
```

## Testing against real tmux

### [`TestServer`]: a private tmux per test

`test-support` exports [`libtmux_tokio::test::TestServer`], the same guard this
crate's own suite uses. For a test project next to the checkout:

```toml
[dev-dependencies]
libtmux-tokio = { path = "../libtmux-rs/crates/libtmux-tokio", features = ["test-support"] }
```

Each guard owns a tmux child on a private socket with an empty config, so tests
cannot reach your real server or each other.

- `shutdown().await` closes escaped clients, waits the daemon, and reports
  cleanup failures.
- `Drop` cleans up best-effort, even after the runtime has ended.

There is no mock and no replay mode: a test here talks to tmux, because what
tmux answers is the thing under test. CI needs tmux installed, which on a
GitHub Actions Ubuntu runner is one step:

```yaml
- run: sudo apt-get install -y --no-install-recommends tmux
```

[`LIBTMUX_TEST_TMUX`][contract-test-guard] points the guard at a specific
build when one release matters. [The contract][contract-test-guard] lists what
the guard isolates.

## Features

`query` is the only one on by default. `full` turns on every capability
below, so a caller who wants them all does not have to list them.

| Feature | What it adds |
| --- | --- |
| `query` | Typed filter expressions over listings. On by default |
| `plan` | Recording tmux work before running it, and choosing what it costs |
| `control-mode` | One persistent tmux connection, so the server reports changes as they happen rather than being polled |
| `derive` | `#[derive(Filterable)]`, for filtering your own structs with the same expressions |
| `serde` | Versioned serialization for [`FilterExpr<T>`], for sending expressions over a wire |
| `schema` | JSON Schema for serialized plans and portable filter expressions |
| `tracing` | Sanitized command instrumentation: a `tmux_command` span per dispatch |
| `test-support` | The real-tmux test guard, for your own tests |
| `full` | Every capability above, but not `test-support` |

Use `libtmux-blocking`, or the default `libtmux` facade, for a native
synchronous client. `libtmux-tokio` has no `blocking` feature.

## If you know the Python libtmux

This is a port, with the same object model: a server holding sessions, windows
linked into them, and panes inside those. Four things differ:

| | Python `libtmux` | this crate |
| --- | --- | --- |
| Calling | synchronous throughout | `async` throughout on Tokio |
| Pane text | `capture_pane` returns `list[str]` | [`Pane::capture`] returns [`TmuxText`], which keeps bytes no `String` would hold |
| Filtering | `QueryList` lives in `_internal` | `query` is public, and field handles are generated per type |
| Transport | one tmux process per command | the same by default, plus control mode and command chaining |

Waiting differs too. Python keeps `retry_until` in a test helper, so production
code writes its own loop. [`Pane::wait_for_text`] and [`Pane::wait_for_quiet`]
are in this library and need no feature.

## Packages

### [`libtmux-tokio`](https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux-tokio)

This crate: the async tmux client and object model. [`examples/`][libtmux-examples]
holds longer programs.

[API reference][libtmux-api] · [docs.rs][libtmux-docsrs]

### [`libtmux-macros`](https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux-macros)

`#[derive(Filterable)]`: your own structs take the same typed queries as tmux
objects. The `derive` feature re-exports it.

[API reference][macros-docsrs] · [Guide][macros-guide]

```rust
# // `derive` is an optional feature, so the example is compiled only when it
# // is on.
# #[cfg(feature = "derive")]
# {
use libtmux_tokio::query::{Filterable as _, QueryIteratorExt as _};

#[derive(libtmux_tokio::Filterable)]
#[filterable(target = "job")]
struct Job {
    name: String,
    attempts: u32,
}

let jobs = [
    Job { name: "build".into(), attempts: 3 },
    Job { name: "deploy".into(), attempts: 1 },
];

let fields = Job::filter_fields();
let retried = fields.name.starts_with("bui").and(fields.attempts.gt(2));

assert_eq!(jobs.iter().matching(&retried).count(), 1);
# }
```

### [`tmux-workspace`](https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-workspace)

A native tmuxp, as one binary with no Python: it loads workspace files, freezes
running sessions back into them, and imports tmuxinator and teamocil projects.

[Command reference][workspace-cli] · [API reference][workspace-api] ·
[docs.rs][workspace-docsrs] · [Guide][workspace-guide]

```console
$ cargo install tmux-workspace --locked
```

Build the session a workspace file describes, and attach to it:

```console
$ tmux-workspace load ./dev.yaml
```

Save a running session back to a file:

```console
$ tmux-workspace freeze dev -o dev.yaml
```

### [`tmux-mcp`](https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-mcp)

A Model Context Protocol server over this crate, read-biased: every tool that
changes state names what it changes.

[Tools][mcp-tools] · [API reference][mcp-api] · [docs.rs][mcp-docsrs] ·
[Connect a client][mcp-connect] · [Guide][mcp-guide]

```console
$ cargo install tmux-mcp
```

Register it with Claude Code. [The guide][mcp-guide] covers the other clients:

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

- **A mock or a replay mode.** A test talks to a real tmux, because what tmux
  answers is the thing under test. [`TestServer`] makes that cheap.
- **Blocking calls.** The [`libtmux` facade](https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/README.md) selects
  the native blocking driver for synchronous programs.
- **Text decoding.** Names, titles, and pane contents arrive as
  [`TmuxText`] bytes, and the caller decides how to decode them.
- **A default control-mode transport.** [Control mode][`Server::over_control_mode`]
  is a connection you attach and route through, and ordinary calls stay one
  process each.
- **Native Windows.** tmux is unavailable there; WSL works.

## Support and status

- **Rust 1.85 or newer.** A raise is a minor version, named in the changelog.
- **tmux 3.3a or newer.** See the [supported releases][tmux-support].
  [Release candidates][tmux-rc] are not supported.
- **Linux, including WSL.** macOS is best-effort.
- **Unix only.** Native Windows is unsupported, because tmux does not run
  there.

## Documentation

| | |
| --- | --- |
| [API documentation](https://docs.rs/libtmux) | The public surface, one page per item, with runnable examples |
| [API reference][libtmux-api] | The same surface on libtmux.org, with guides and concepts beside it |
| [`docs/design.md`][design] | Why the crate is shaped the way it is: the transport, the snapshot and format boundary, the query grammar, the test guard, and the compatibility lanes |
| [`docs/findings.md`][findings] | Every tmux defect and platform surprise the crate works around, each with the tmux releases it holds for and what the crate does |
| [`docs/contracts.md`][contracts] | What the crate promises: the version check, deadlines, errors, and the test guard |
| [`docs/migration.md`](docs/migration.md) | Moving from an earlier alpha |
| [`CHANGELOG.md`](CHANGELOG.md) | What each release of this crate changed; the [main changelog](https://github.com/libtmux/libtmux-rs/blob/master/CHANGELOG.md) holds what spans crates |
| [`examples/`][libtmux-examples] | Programs that run and clean up after themselves |

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE)
or [MIT license](LICENSE-MIT) at your option.

[design]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-tokio/docs/design.md
[findings]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-tokio/docs/findings.md
[contracts]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-tokio/docs/contracts.md
[contract-version]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-tokio/docs/contracts.md#the-tmux-version-check
[contract-narrow]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-tokio/docs/contracts.md#operations-with-narrower-requirements
[contract-nested]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-tokio/docs/contracts.md#commands-nested-in-another-command
[tmux-support]: https://libtmux.org/en/tmux/guides/tmux-support/
[tmux-rc]: https://libtmux.org/en/tmux/guides/tmux-support/#release-candidates
[contract-cancellation]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-tokio/docs/contracts.md#deadlines-cancellation-and-shutdown
[contract-errors]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-tokio/docs/contracts.md#errors
[contract-test-guard]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-tokio/docs/contracts.md#the-test-guard
[example-orchestrate]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-tokio/examples/orchestrate.rs
[example-watch]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-tokio/examples/watch.rs
[example-recover]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-tokio/examples/recover.rs
[pkg-macros]: https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux-macros
[pkg-workspace]: https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-workspace
[pkg-mcp]: https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-mcp
[error-linkgone]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-error-linkgone/
[error-objectgone]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-error-objectgone/
[session-windows]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-session-windows/
[default-timeout]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-serverbuilder-default_timeout/
[libtmux-api]: https://libtmux.org/en/rs/latest/reference/
[libtmux-docsrs]: https://docs.rs/libtmux-tokio/latest/libtmux_tokio/
[libtmux-examples]: https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux-tokio/examples
[macros-docsrs]: https://docs.rs/libtmux-macros/latest/libtmux_macros/derive.Filterable.html
[macros-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-macros/README.md
[workspace-cli]: https://libtmux.org/en/rs/latest/workspace/cli/
[workspace-api]: https://libtmux.org/en/rs/latest/workspace/reference/
[workspace-docsrs]: https://docs.rs/tmux-workspace/latest/tmux_workspace/
[workspace-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-workspace/README.md
[mcp-tools]: https://libtmux.org/en/rs/latest/mcp/tools/
[mcp-api]: https://libtmux.org/en/rs/latest/mcp/reference/
[mcp-docsrs]: https://docs.rs/tmux-mcp/latest/tmux_mcp/
[mcp-connect]: https://libtmux.org/en/rs/latest/mcp/guides/connect-client/
[mcp-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/README.md

[`Error::UnsupportedTmuxVersion`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-error-unsupportedtmuxversion/
[`Server`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server/
[`ServerBuilder::tmux_executable`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-serverbuilder-tmux_executable/
[`FilterExpr<T>`]: https://libtmux.org/en/rs/latest/reference/libtmux_core-query-filterexpr/
[`Server::cmd`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server-cmd/
[`Server::new`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server-new/
[`Server::builder`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server-builder/
[`Server::with_session`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server-with_session/
[`Server::session`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server-session/
[`Server::sessions`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server-sessions/
[`Pane::from_env`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-from_env/
[`Session::new_window`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-session-new_window/
[`Pane::split`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-split/
[`Pane::send_line`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-send_line/
[`Pane::send_keys`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-send_keys/
[`Pane::send_key_names`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-send_key_names/
[`Pane::capture`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-capture/
[`Pane::capture_with`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-capture_with/
[`Pane::wait_for_text`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-wait_for_text/
[`Pane::wait_for_quiet`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-wait_for_quiet/
[`Pane::stream_output`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-stream_output/
[`ControlMode::attach`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-control-controlmode-attach/
[`ControlSender::subscribe`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-control-controlsender-subscribe/
[`Filterable::filter_fields`]: https://libtmux.org/en/rs/latest/reference/libtmux_core-query-filterable-filter_fields/
[`QueryIteratorExt::matching`]: https://libtmux.org/en/rs/latest/reference/libtmux_core-query-queryiteratorext-matching/
[`plan::Plan`]: https://libtmux.org/en/rs/latest/reference/libtmux_core-plan-plan/
[`plan::Planner`]: https://libtmux.org/en/rs/latest/reference/libtmux_core-plan-planner/
[`test::reap_abandoned_servers`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-test-reap_abandoned_servers/
[`Error::is_object_gone`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-error-is_object_gone/
[`Error::is_transient`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-error-is_transient/
[`PaneWait`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-panewait/
[`PaneWait::Dead`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-panewait-dead/
[do not poll]: https://libtmux.org/en/rs/latest/topics/waiting-and-retry/
[`Server::over_control_mode`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server-over_control_mode/
[`Server::wait_for_channel`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server-wait_for_channel/
[`Server::signal_channel`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server-signal_channel/
[`ChannelWait`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-channelwait/
[`Session`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-session/
[`Window`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-window/
[`Pane`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane/
[`Client`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-client/
[`Server::hierarchy`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server-hierarchy/
[Typed field handles]: https://libtmux.org/en/rs/latest/concepts/queries/
[`SessionTree`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-sessiontree/
[`WindowTree`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-windowtree/
[versioned JSON envelope]: https://libtmux.org/en/rs/latest/reference/libtmux_core-query-filterexpr/
[`TmuxText`]: https://libtmux.org/en/rs/latest/reference/libtmux_core-tmuxtext/
[`Error`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-error/
[`Error::kind`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-error-kind/
[`Error::AfterEffect`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-error-aftereffect/
[`ErrorKind::PartialEffect`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-errorkind-partialeffect/
[`ServerBuilder::default_timeout`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-serverbuilder-default_timeout/
[`Server::shutdown()`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server-shutdown/
[`libtmux_tokio::test::TestServer`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-test-testserver/
[`TestServer`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-test-testserver/
[`CommandChain::new(a).then(b)`]: https://libtmux.org/en/rs/latest/reference/libtmux_core-commandchain/
[`CommandResult`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-commandresult/
