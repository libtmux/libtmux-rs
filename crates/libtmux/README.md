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

[crates.io]: https://img.shields.io/crates/v/libtmux.svg
[crate]: https://crates.io/crates/libtmux
[docs.rs]: https://img.shields.io/docsrs/libtmux
[docs]: https://docs.rs/libtmux
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

> **Alpha.** The API changes between releases, including in ways that will not
> be called out as breaking, because nothing here is stable yet. Cargo will not
> resolve a prerelease unless the requirement names one, so a plain `0.1`
> requirement does not pick this up: depend on the exact version below, and
> expect to edit it.

See the [migration notes](docs/migration.md) when upgrading from an earlier alpha.
[`libtmux-macros`][pkg-macros], [`tmux-workspace`][pkg-workspace] and
[`tmux-mcp`][pkg-mcp] are published beside this crate, under
[Packages](#packages).

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

Add the crate, and Tokio to drive it:

```toml
[dependencies]
libtmux = "0.1.0-alpha.16"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Create a session, type into its pane, and print what the pane showed:

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

    pane.send_line("echo hello").await?;

    for line in pane.capture().await? {
        println!("{}", line.to_string_lossy());
    }

    guard.shutdown().await?;
    Ok(())
}
```

The examples on this page run as written, against a throwaway tmux. To run
them yourself, enable the `test-support` feature as a dev-dependency:
`libtmux = { version = "0.1.0-alpha.16", features = ["test-support"] }`.

[`Pane::send_line`] types a line and presses Enter, and [`Pane::capture`]
reads what the pane shows. Every method that reaches tmux is `async`.
Everything that reads an already-taken snapshot is not, so walking a tree you
already have costs no round trips.

Commands run without a shell, and results keep stdout and stderr as raw bytes,
so decoding stays the caller's decision.

It needs Rust 1.85 or newer and tmux 3.3a or newer, on a Unix host. Against an
older daemon, an operation returns [`Error::UnsupportedTmuxVersion`] and sends
nothing. To fix it, upgrade tmux and restart its server, or pin the previous
release. [The contract][contract-version] covers unrecognized versions, failed
probes, and the calls that skip the check.

Optional operations have narrower daemon requirements:

| Operation | Qualified daemon |
| --- | --- |
| Ordinary subprocess operations and polling waits | 3.3a or newer |
| Named-buffer deletion or rename, including managed paste cleanup | 3.4 or newer |
| Captured `run-shell` output | 3.5 or newer |
| Control acquisition, pane streams and subscriptions | A daemon ranking at or above 3.7 final |
| Literal `break-pane` | Refused on exact 3.7; available again on 3.7a |
| Literal `split-window -p` | Refused on 3.4; typed percentages use `-l N%` |
| Literal `select-layout` with a layout operand | On 3.3a, a unique preset or standard serialized header; unsafe operands refuse |
| Capture flags `-T` / `-F` | 3.4 / 3.7 or newer |

Development and release-candidate daemons refuse managed control acquisition.
Eligibility comes from the checked daemon and assumes a natively compatible
client/daemon pair. Direct literal argv commands and
flat batches are checked before dispatch; `Command::requires_tmux` carries a
managed staging requirement across the whole chain. The guard checks the
command it is given. It does not look inside tmux command strings nested in
another command (`if-shell`, `run-shell -C`, brace blocks) or inside alias
bodies, and shell programs keep native semantics. Do not nest a command a gate
refuses; on an affected release it is sent as written. On the
pinned Linux builds, 3.3a, 3.4 and 3.5 clients cannot register a control connection to
an exact 3.7 daemon; 3.7d can. Those older clients exit without changing the
daemon, including on a plain native attachment. Use a compatible client for
control; this does not raise the 3.3a subprocess baseline.

Async operations need an entered Tokio runtime. The default executable is
`tmux`, resolved through the `PATH` captured when the [`Server`] is built; use
[`ServerBuilder::tmux_executable`] to select another.

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

Waiting needs no feature. [`Pane::wait_for_text`] and
[`Pane::wait_for_quiet`] are in the library, and [`docs/findings.md`][findings]
records what they had to survive.

## What tmux is made of

```text
Server ──┬── Session ──── Window ──── Pane
         └── Client ─────┘
```

| Type | Held by | Identified by | Notes |
| --- | --- | --- | --- |
| [`Server`] | a socket | its socket path | one daemon; [`Server::new`] finds the default |
| [`Session`] | the server | `$0` | what a client attaches to |
| [`Window`] | *linked into* sessions | `@0` | one window can be linked into several sessions at once |
| [`Pane`] | exactly one window | `%0` | holds the process and the scrollback |
| [`Client`] | the server | its tty | attached to one session at a time |

The third row decides API shape. A window has one identity and several
*links*, so a command that removes one link names the session too, as
`session:@id`. [`Window::unlink`][window-unlink] does exactly that.

It reports [`Error::LinkGone`][error-linkgone] rather than
[`Error::ObjectGone`][error-objectgone] when the link is already gone, because
the window itself may still be running under another session.

## Find sessions, windows, and panes

### [`Server::hierarchy`]: the whole tree in three commands

[`Server::hierarchy`] gathers sessions, windows, and panes in three tmux
commands, rather than one per object. [`Server::sessions`] and
[`Session::windows`][session-windows] are its narrower siblings:

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

The accessors in the loop are not `async`: the result is a snapshot, so
reading names from it costs no round trips.

[`Server::sessions`] returns `Result<Vec<Session>>`, so an unreachable tmux is
an error and a running tmux with no sessions is an empty list.
`sessions().await.unwrap_or_default()` suits a status line. A reconciler that
reads "no sessions" from an outage would delete everything, so it should match
on the error. [The contract][contract-errors] has more.

## Type into a pane and wait

### [`Server::wait_for_channel`]: wait for work that announces itself

If the work can announce itself, [do not poll]. End it with
`tmux wait-for -S <channel>`, which [`Server::signal_channel`] sends, and wait
on that channel:

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
lose the race, and nothing polls. A wait that runs out of time keeps its
client on the channel, so the signal is still there for this process's next
wait.

The example prints `the build ended`. Running out of time is an outcome
([`ChannelWait`]), and tmux being unreachable is an error.
[`examples/orchestrate.rs`][example-orchestrate] runs three jobs this way.

### [`Pane::wait_for_text`]: wait for text a pane prints

For a pane that was *not* written to announce itself, [`Pane::wait_for_text`]
does the same job without a channel:

```rust
use libtmux::{PaneWait, test::TestServer};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let session = guard.server().new_session("waiting").await?;
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
  holding the deadline.

Polling costs a tmux process per tick, and a tick is 120ms, so a wait stays
under ten dispatches a second. Every command is also bounded by the server's
[`default_timeout`][default-timeout]: the deadline above is for the
*condition*, and the timeout is for the transport.

[`Pane::wait_for_quiet`] waits for a pane to stop writing. `command_prompt` and
`display_menu` wait for a person, so the dispatch timeout is what ends them.

## Watch tmux change

### [`ControlSender::subscribe`]: have tmux watch a value

With `control-mode`, a subscription costs one connection rather than a process
per tick. tmux evaluates a format for you and reports it when its value
changes. It watches a format rather than the pane's text, so it suits "how many
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
# let version = guard.server().capabilities().await?.tmux_version().clone();
# let eligible = version.has_behavior(&libtmux::since::CONTROL_ACQUISITION);
# if !eligible {
#     guard.shutdown().await?;
#     return Ok(());
# }
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

### [`SessionTree`] and [`WindowTree`]: ask what a session contains

A question about what a session *contains* needs the shape that holds its
windows, so [`SessionTree`] and [`WindowTree`] carry relations:

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

The block shows the shape against a healthy server, so its interesting arms do
not run. [`examples/recover.rs`][example-recover] makes each failure happen
instead, including the one that costs people: a link is gone and the window it
pointed at is still running. [The contract][contract-errors] has the details.

## Choosing how commands reach tmux

[Three switches] decide what a run costs and what it can prove. They compose:
async is the API, control mode is the transport, and chaining is what a
subprocess transport does instead of having one.

### What each one does

| Switch | Off (the default) | On |
| --- | --- | --- |
| **Async** | Nothing: every method is already `async`. [`blocking::Runtime`] is a runtime you drive them from, and the API stays the async one | `blocking::Runtime::new()?.run(future)` for scripts and tests |
| **Control mode** | One tmux process per command | One connection for every command, and tmux reports changes as they happen |
| **Chaining** | One tmux process per command | Neighbouring commands share one process, trading the ability to say which one failed |

Measured on one workload of six operations, all leaving identical tmux state:

| Mode | Processes | Attribution on failure |
| --- | --- | --- |
| One command per invocation | 6 | names the failing command |
| Folded into shared invocations | 3 | `Unknown` -- tmux reports one status for the group |
| Control mode | 1 | names the failing command |

Control mode is the only one that buys back the processes without giving up
the answer, because its `%begin`/`%end` blocks are per command. Run
`cargo run --example matrix --features full,test-support` to reproduce the
table on your own machine.

### How to turn each one on

| Switch | Cargo feature | In code |
| --- | --- | --- |
| Async | none | already the default; every method is `async` |
| Blocking runtime | `blocking` | `libtmux::blocking::Runtime::new()?` |
| Control mode | `control-mode` | `ControlMode::attach(&server, session).await?` |
| Chaining, by hand | none | [`CommandChain::new(a).then(b)`], then `server.chain(chain).await?` |
| Chaining, by planner | `plan` | `plan.run(&server, Planner::Folding).await?` |
| Folding a pane creation in too | `plan` | [`Planner::Marked`][`plan::Planner`] |
| Never folding across your own work | `plan` | `Planner::steps_bounded(&plan, &boundaries)` |
| Per-command answers over one connection | `plan`, `control-mode` | `plan.run_over_control_mode(&sender).await?` |

### [`Server::over_control_mode`]: send every typed call down one connection

Control mode is never the default transport, and turning the feature on does
not make it one: normal commands stay one process per command until you attach
a connection and use it. [`Server::over_control_mode`] returns a handle whose
ordinary typed calls travel down the connection, and handles reached through
it inherit the route:

```rust
# // `over_control_mode` is behind `control-mode`, so the example is compiled
# // only when that feature is on.
# #[cfg(not(feature = "control-mode"))]
# fn main() {}
# #[cfg(feature = "control-mode")]
use libtmux::control::ControlMode;
# #[cfg(feature = "control-mode")]
use libtmux::test::TestServer;

# #[cfg(feature = "control-mode")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
# let version = guard.server().capabilities().await?.tmux_version().clone();
# if !version.has_behavior(&libtmux::since::CONTROL_ACQUISITION) {
#     guard.shutdown().await?;
#     return Ok(());
# }
    let session = guard.server().new_session("routed").await?;

    let (commands, events) = ControlMode::attach(guard.server(), session.id())
        .await?
        .split();
    let routed = guard.server().over_control_mode(&commands).await?;

    // One connection, no processes: the whole typed API, not a plan's worth of it.
    for pane in routed.panes().await? {
        pane.send_line("echo hello").await?;
    }

    events.shutdown().await?;
    guard.shutdown().await?;
    Ok(())
}
```

Keep the original server for the two things a connection is wrong for: a
command that parks the client's queue, such as `wait-for` or a foreground
`run-shell`, and arguments that are not valid UTF-8, which a text protocol
cannot carry.

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
use libtmux::Command;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = libtmux::test::TestServer::new().await?;
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
narrower than the command behind it. [`Server::cmd`] runs anything, through
the same transport, timeout and error classification as the typed calls, so
reaching for it costs you the method and keeps the machinery:

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
use this crate from outside it, and when either reaches for `cmd` that counts
as a gap in the API.

## Text from tmux is bytes

### [`TmuxText`]: names, titles, and contents as bytes

tmux permits names, titles, and pane contents that are not valid UTF-8, so they
arrive as [`TmuxText`] rather than `String`. There is no implicit conversion:

```rust
let text = libtmux::TmuxText::from("editor");

assert_eq!(text.as_bytes(), b"editor");
assert_eq!(text.as_str().expect("valid UTF-8"), "editor");
assert_eq!(text.to_string_lossy(), "editor");
```

## Testing against real tmux

### [`TestServer`]: a private tmux per test

`test-support` exports [`libtmux::test::TestServer`], the same guard this
crate's own suite uses:

```toml
[dev-dependencies]
libtmux = { version = "0.1.0-alpha.16", features = ["test-support"] }
```

Each guard owns a tmux child on a private socket with an empty config, so tests
cannot reach your real server or each other. `shutdown().await` closes escaped
clients, waits the daemon, and reports cleanup failures, and `Drop` cleans up
best-effort even after the runtime has ended.

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
| `blocking` | A runtime for calling from code that is not async |
| `derive` | `#[derive(Filterable)]`, for filtering your own structs with the same expressions |
| `serde` | Versioned serialization for [`FilterExpr<T>`], for sending expressions over a wire |
| `schema` | JSON Schema for serialized plans and portable filter expressions |
| `tracing` | Sanitized command instrumentation: a `tmux_command` span per dispatch |
| `test-support` | The real-tmux test guard, for your own tests |
| `full` | Every capability above, but not `test-support` |

## If you know the Python libtmux

This is a port of it, and the object model is the same one: a server holding
sessions, windows linked into them, panes inside those. Code you have written
against the Python library will read as familiar here. Four things differ, and
they are the ones worth knowing before you start.

| | Python `libtmux` | this crate |
| --- | --- | --- |
| Calling | synchronous throughout | `async` throughout; [`blocking::Runtime`] drives it from code that is not |
| Pane text | `capture_pane` returns `list[str]` | [`Pane::capture`] returns [`TmuxText`], which keeps bytes no `String` would hold |
| Filtering | `QueryList` lives in `_internal` | `query` is public, and field handles are generated per type |
| Transport | one tmux process per command | the same by default, plus control mode and command chaining |

Waiting differs in this crate's favour. The Python library keeps `retry_until`
in `libtmux/test/retry.py`, a test helper, so production code that waits on a
pane writes the loop itself. [`Pane::wait_for_text`] and
[`Pane::wait_for_quiet`] are here in the library and need no feature.

## Packages

Four crates are published from this repository. Each header links to the
crate's directory, where its own README starts.

### [`libtmux`](https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux)

This crate: the async tmux client and object model. The examples on this page
are its own, and [`examples/`][libtmux-examples] holds longer programs.

[API reference][libtmux-api] · [docs.rs][libtmux-docsrs]

### [`libtmux-macros`](https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux-macros)

`#[derive(Filterable)]`, so your own structs take the same typed query grammar
as tmux objects. You do not depend on this crate: the `derive` feature
re-exports the macro.

[API reference][macros-docsrs] · [Guide][macros-guide]

```rust
# // `derive` is an optional feature, so the example is compiled only when it
# // is on.
# #[cfg(feature = "derive")]
# {
use libtmux::query::{Filterable as _, QueryIteratorExt as _};

#[derive(libtmux::Filterable)]
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

A native tmuxp: it loads tmuxp workspace files, freezes running sessions back
into them, and imports tmuxinator and teamocil projects, as one binary with no
Python.

[Command reference][workspace-cli] · [API reference][workspace-api] ·
[docs.rs][workspace-docsrs] · [Guide][workspace-guide]

```console
$ cargo install tmux-workspace \
    --version 0.1.0-alpha.16 \
    --locked
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

A Model Context Protocol server over this crate. It is read-biased, and every
tool that changes state names what it changes.

[Tools][mcp-tools] · [API reference][mcp-api] · [docs.rs][mcp-docsrs] ·
[Connect a client][mcp-connect] · [Guide][mcp-guide]

```console
$ cargo install tmux-mcp --version 0.1.0-alpha.17
```

Register it with Claude Code. The guide has the same step for the other
clients:

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
- **A second, blocking API.** [`blocking::Runtime`] is a runtime you drive the
  async API from.
- **Text decoding.** Names, titles, and pane contents arrive as
  [`TmuxText`] bytes, and the caller decides how to decode them.
- **A default control-mode transport.** [Control mode][`Server::over_control_mode`]
  is a connection you attach and route through, and ordinary calls stay one
  process each.
- **Native Windows.** tmux is unavailable there; WSL works.

## Support and status

While the version is `0.1.0-alpha.*` the API is not settled, and any release
may change or remove exported identifiers. Pin an exact version.

- **Rust 1.85+**, the first compiler to ship Edition 2024. CI runs the whole
  test suite on it, and a raise is a minor version bump named in the release
  notes.
- **tmux 3.3a or newer.** CI builds 3.3a, 3.4, 3.5a, 3.6b, and 3.7b from
  source and runs the whole workspace against each. 3.4 and 3.5a wrap command
  output differently, which is why the format codec carries a second dialect.
- **Linux, including WSL.** CI runs the tests on Linux. On macOS, CI runs them
  against Homebrew's current tmux on pushes to `master` and manual runs, and
  not on pull requests.
- **Unix only.** Native Windows is unsupported because tmux is unavailable
  there.

## Documentation

| | |
| --- | --- |
| [API documentation](https://docs.rs/libtmux) | The public surface, one page per item, with runnable examples |
| [API reference][libtmux-api] | The same surface on libtmux.org, with guides and concepts beside it |
| [`docs/design.md`][design] | Why the crate is shaped the way it is: the transport, the snapshot and format boundary, the query grammar, the test guard, and the compatibility lanes |
| [`docs/findings.md`][findings] | Every tmux defect and platform surprise the crate works around, each with the tmux releases it holds for and what the crate does |
| [`docs/parity.md`][parity] | The capability ledger against Python libtmux, naming each Rust symbol and the test that exercises it |
| [`docs/contracts.md`][contracts] | What the crate promises: the version check, deadlines, errors, and the test guard |
| [`docs/migration.md`](docs/migration.md) | Moving from an earlier alpha |
| [`examples/`][libtmux-examples] | Programs that run and clean up after themselves |

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE)
or [MIT license](LICENSE-MIT) at your option.

[design]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/docs/design.md
[findings]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/docs/findings.md
[parity]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/docs/parity.md
[contracts]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/docs/contracts.md
[contract-version]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/docs/contracts.md#the-tmux-version-check
[contract-cancellation]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/docs/contracts.md#deadlines-cancellation-and-shutdown
[contract-errors]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/docs/contracts.md#errors
[contract-test-guard]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/docs/contracts.md#the-test-guard
[example-orchestrate]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/examples/orchestrate.rs
[example-watch]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/examples/watch.rs
[example-recover]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/examples/recover.rs
[pkg-macros]: https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux-macros
[pkg-workspace]: https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-workspace
[pkg-mcp]: https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-mcp
[window-unlink]: https://libtmux.org/en/rs/latest/reference/window-window-unlink/
[error-linkgone]: https://libtmux.org/en/rs/latest/reference/error-error-linkgone/
[error-objectgone]: https://libtmux.org/en/rs/latest/reference/error-error-objectgone/
[session-windows]: https://libtmux.org/en/rs/latest/reference/session-session-windows/
[default-timeout]: https://libtmux.org/en/rs/latest/reference/server-builder-serverbuilder-default_timeout/
[libtmux-api]: https://libtmux.org/en/rs/latest/reference/
[libtmux-docsrs]: https://docs.rs/libtmux/latest/libtmux/
[libtmux-examples]: https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux/examples
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

[`Error::UnsupportedTmuxVersion`]: https://libtmux.org/en/rs/latest/reference/error-error-unsupportedtmuxversion/
[`Server`]: https://libtmux.org/en/rs/latest/reference/server-server/
[`ServerBuilder::tmux_executable`]: https://libtmux.org/en/rs/latest/reference/server-builder-serverbuilder-tmux_executable/
[`FilterExpr<T>`]: https://libtmux.org/en/rs/latest/reference/query-filterexpr/
[`Server::cmd`]: https://libtmux.org/en/rs/latest/reference/server-server-cmd/
[`Server::new`]: https://libtmux.org/en/rs/latest/reference/server-server-new/
[`Server::with_session`]: https://libtmux.org/en/rs/latest/reference/server-server-with_session/
[`Server::session`]: https://libtmux.org/en/rs/latest/reference/server-server-session/
[`Server::sessions`]: https://libtmux.org/en/rs/latest/reference/server-server-sessions/
[`Pane::from_env`]: https://libtmux.org/en/rs/latest/reference/pane-pane-from_env/
[`Session::new_window`]: https://libtmux.org/en/rs/latest/reference/session-session-new_window/
[`Pane::split`]: https://libtmux.org/en/rs/latest/reference/pane-pane-split/
[`Pane::send_line`]: https://libtmux.org/en/rs/latest/reference/pane-pane-send_line/
[`Pane::send_keys`]: https://libtmux.org/en/rs/latest/reference/pane-pane-send_keys/
[`Pane::send_key_names`]: https://libtmux.org/en/rs/latest/reference/pane-pane-send_key_names/
[`Pane::capture`]: https://libtmux.org/en/rs/latest/reference/pane-pane-capture/
[`Pane::capture_with`]: https://libtmux.org/en/rs/latest/reference/pane-pane-capture_with/
[`Pane::wait_for_text`]: https://libtmux.org/en/rs/latest/reference/pane-pane-wait_for_text/
[`Pane::wait_for_quiet`]: https://libtmux.org/en/rs/latest/reference/pane-pane-wait_for_quiet/
[`Pane::stream_output`]: https://libtmux.org/en/rs/latest/reference/pane-pane-stream_output/
[`ControlMode::attach`]: https://libtmux.org/en/rs/latest/reference/control-controlmode-attach/
[`ControlSender::subscribe`]: https://libtmux.org/en/rs/latest/reference/control-controlsender-subscribe/
[`Filterable::filter_fields`]: https://libtmux.org/en/rs/latest/reference/query-filterable-filter_fields/
[`QueryIteratorExt::matching`]: https://libtmux.org/en/rs/latest/reference/query-queryiteratorext-matching/
[`plan::Plan`]: https://libtmux.org/en/rs/latest/reference/plan-plan/
[`plan::Planner`]: https://libtmux.org/en/rs/latest/reference/plan-planner-planner/
[`test::reap_abandoned_servers`]: https://libtmux.org/en/rs/latest/reference/test-reap_abandoned_servers/
[`Error::is_object_gone`]: https://libtmux.org/en/rs/latest/reference/error-error-is_object_gone/
[`Error::is_transient`]: https://libtmux.org/en/rs/latest/reference/error-error-is_transient/
[`PaneWait`]: https://libtmux.org/en/rs/latest/reference/pane-panewait/
[`PaneWait::Dead`]: https://libtmux.org/en/rs/latest/reference/pane-panewait-dead/
[do not poll]: https://libtmux.org/en/rs/latest/topics/waiting-and-retry/
[`blocking::Runtime`]: https://libtmux.org/en/rs/latest/reference/blocking-runtime/
[Three switches]: https://libtmux.org/en/rs/latest/concepts/transports/
[`Server::over_control_mode`]: https://libtmux.org/en/rs/latest/reference/server-server-over_control_mode/
[`Server::wait_for_channel`]: https://libtmux.org/en/rs/latest/reference/server-server-wait_for_channel/
[`Server::signal_channel`]: https://libtmux.org/en/rs/latest/reference/server-server-signal_channel/
[`ChannelWait`]: https://libtmux.org/en/rs/latest/reference/server-channelwait/
[`Session`]: https://libtmux.org/en/rs/latest/reference/session-session/
[`Window`]: https://libtmux.org/en/rs/latest/reference/window-window/
[`Pane`]: https://libtmux.org/en/rs/latest/reference/pane-pane/
[`Client`]: https://libtmux.org/en/rs/latest/reference/client-client/
[`Server::hierarchy`]: https://libtmux.org/en/rs/latest/reference/server-server-hierarchy/
[Typed field handles]: https://libtmux.org/en/rs/latest/concepts/queries/
[`SessionTree`]: https://libtmux.org/en/rs/latest/reference/server-discovery-sessiontree/
[`WindowTree`]: https://libtmux.org/en/rs/latest/reference/server-discovery-windowtree/
[versioned JSON envelope]: https://libtmux.org/en/rs/latest/reference/query-filterexpr/
[`TmuxText`]: https://libtmux.org/en/rs/latest/reference/formats-text-tmuxtext/
[`Error`]: https://libtmux.org/en/rs/latest/reference/error-error/
[`Error::kind`]: https://libtmux.org/en/rs/latest/reference/error-error-kind/
[`Error::AfterEffect`]: https://libtmux.org/en/rs/latest/reference/error-error-aftereffect/
[`ErrorKind::PartialEffect`]: https://libtmux.org/en/rs/latest/reference/error-errorkind-partialeffect/
[`ServerBuilder::default_timeout`]: https://libtmux.org/en/rs/latest/reference/server-builder-serverbuilder-default_timeout/
[`Server::shutdown()`]: https://libtmux.org/en/rs/latest/reference/server-server-shutdown/
[`libtmux::test::TestServer`]: https://libtmux.org/en/rs/latest/reference/test-testserver/
[`TestServer`]: https://libtmux.org/en/rs/latest/reference/test-testserver/
[`CommandChain::new(a).then(b)`]: https://libtmux.org/en/rs/latest/reference/command-commandchain/
