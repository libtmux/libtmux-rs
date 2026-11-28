<!-- libtmux-logo -->
<div align="center">
  <picture>
    <source srcset="assets/logo.svg" type="image/svg+xml">
    <img src="assets/logo.png" width="128" height="128" alt="libtmux for Rust">
  </picture>
</div>
<!-- /libtmux-logo -->

<div align="center">

# libtmux for Rust

[![CI]][actions] [![libtmux]][libtmux-crate]
[![libtmux.org](https://img.shields.io/badge/docs-libtmux.org-0b7285)](https://libtmux.org/en/rs/)
[![License]][license] [![MSRV]][rust-1.85]

[CI]: https://github.com/libtmux/libtmux-rs/actions/workflows/ci.yml/badge.svg
[actions]: https://github.com/libtmux/libtmux-rs/actions/workflows/ci.yml
[libtmux]: https://img.shields.io/crates/v/libtmux.svg?label=libtmux
[libtmux-crate]: https://crates.io/crates/libtmux
[tmux-mcp]: https://img.shields.io/crates/v/tmux-mcp.svg?label=tmux-mcp
[mcp-crate]: https://crates.io/crates/tmux-mcp
[MSRV]: https://img.shields.io/badge/rustc-1.85+-lightgray.svg
[rust-1.85]: https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/
[License]: https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg
[license]: https://github.com/libtmux/libtmux-rs/blob/master/LICENSE-MIT

Drive tmux from Rust using a native blocking client or Tokio. Both clients
share session, window and pane IDs, creation requests and receipts.

</div>

[Type into a pane][`Pane::send_line`], [query][Typed field handles] what a
server holds with typed expressions, [load tmuxp workspaces][workspace-cli-load]
from YAML, and [let an agent drive tmux][mcp-tools] over the Model Context
Protocol.

> Releases are numbered `0.1.N`, and each release increments `N`. The API is
> not settled: any release may change or remove exported identifiers, and the
> changelog marks each break. Pin an exact version to avoid one.

**Contents** — [Quick start](#quick-start) ·
[Is this for you?](#is-this-for-you) ·
[Work in panes](#work-in-panes) ·
[Query what is there](#query-what-is-there) ·
[Refresh an object](#refresh-an-object) ·
[Pick how commands reach tmux](#pick-how-commands-reach-tmux) ·
[Watch tmux change](#watch-tmux-change) ·
[Load workspaces](#load-workspaces-from-yaml) ·
[Drive tmux from an agent](#drive-tmux-from-an-agent) ·
[Packages](#packages) · [What it leaves out](#what-it-leaves-out) ·
[Support](#support-and-status)

## Quick start

The native drivers are available from this checkout. With a binary project
next to the `libtmux-rs` directory, add the default blocking client:

```console
$ cargo add libtmux --path ../libtmux-rs/crates/libtmux
```

Save this as `src/main.rs`. [`blocking::Server::at`][blocking-server-at] selects
the socket. The program creates a session, observes its pane key and kills
the session:

[blocking-server-at]: https://libtmux.org/en/rs/latest/reference/libtmux_blocking-server-at/

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

### Use the Tokio client

To run the async examples, enable the Tokio client and its private test fixture:

```console
$ cargo add libtmux --path ../libtmux-rs/crates/libtmux --features tokio,test-support,plan,query,control-mode
```

```console
$ cargo add tokio --features rt-multi-thread,macros
```

The [mixed-client example](crates/libtmux/examples/api_mixed.rs) creates a
session outside the runtime, then passes the new pane's key to a Tokio capture.
The [published 0.1.2 guide](https://github.com/libtmux/libtmux-rs/blob/1027012583abdc8b07ea0fd071e0c6cacb051278/README.md)
documents the crate available from the registry.

Create a session and a window, type into the pane, and check what tmux holds:

```rust
use libtmux::tokio::test::TestServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // This example runs. `TestServer` is an isolated tmux on its own socket
    // under `/tmp/libtmux-rs-test/`, torn down at the end, so it cannot touch
    // sessions you are using.
    let guard = TestServer::new().await?;
    let server = guard.server();

    let receipt = server.new_session("work").await?;

    let session = server.inspect_session(&receipt).await?;
    let window = session.new_window("editor").await?;
    let pane = window.active_pane().await?.expect("the new window has a pane");

    pane.send_line("echo built").await?;

    assert_eq!(server.sessions().await?.len(), 1);

    guard.shutdown().await?;
    Ok(())
}
```

- **Throwaway tmux.** The quick start names its private socket explicitly.
  The Tokio examples use an owned test server.
- **Your own server.** Use `libtmux::tokio::Server::new()?` in place of the guard, and
  omit `guard.shutdown()`. It needs no `test-support` feature.
- **Which server.** With neither `LIBTMUX_SOCKET_PATH` nor `LIBTMUX_SOCKET`
  set, `Server::new` names no socket and tmux chooses the server.

The Tokio client awaits operations that contact tmux. Both clients can read
an existing creation receipt without another round trip.

## Is this for you?

| You want to | Use |
| --- | --- |
| Script tmux from Rust, blocking or async | [`libtmux`](crates/libtmux) |
| Ask "which pane/window/session matches X?" | [`libtmux`](crates/libtmux) query layer, below |
| Let an AI agent read and drive tmux | [`tmux-mcp`](crates/tmux-mcp) |
| Filter your own structs with the same grammar | [`libtmux-macros`](crates/libtmux-macros) |
| Load tmuxp workspaces from the command line, or build them from Rust | [`tmux-workspace`](crates/tmux-workspace) |

Not for you if you need Windows without WSL, because tmux does not run there.

## Work in panes

### [`Window::split`]: split a window, type into the new pane, and read it back

[`Pane::send_line`] types, [`Pane::capture`] reads the screen, and
[`Pane::capture_with`] reaches into scrollback. [`Pane::split`] divides a pane
you name, and [`Pane::resize`] and [`Window::resize`] set a size in cells:

```rust
use std::time::Duration;

use libtmux::tokio::test::TestServer;
use libtmux::tokio::{PaneWait, SplitDirection, SplitOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let session = guard.session("work").await?;
    let mut window = session
        .active_window()
        .await?
        .expect("a session has a window");

    // No client sizes this window, so say how big it is.
    window.resize(100, 40).await?;

    // The new pane runs `sh`, so what it prints does not depend on your shell.
    let mut pane = window
        .split(SplitOptions::new(SplitDirection::Below).command("sh"))
        .await?;
    pane.resize(100, 10).await?;
    assert_eq!((pane.width(), pane.height()), (100, 10));

    pane.send_line("echo $((6 * 7))").await?;
    let seen = pane.wait_for_text("42", Duration::from_secs(5)).await?;
    assert_eq!(seen, PaneWait::Arrived);
    let screen = pane.capture().await?;
    assert!(screen.iter().any(|line| line.to_string_lossy() == "42"));

    pane.kill().await?;
    assert_eq!(window.panes().await?.len(), 1);

    guard.shutdown().await?;
    Ok(())
}
```

- Kill with [`Pane::kill`], [`Window::kill`], [`Session::kill`] or
  [`Server::kill`].
- Set and read an option with [`Session::set_option`] and
  [`Session::typed_option`], and set a hook with [`Session::set_hook`].

## Query what is there

### [`Filterable::filter_fields`]: a comparison that means nothing does not compile

[Typed field handles] build the expression, so a comparison that has no meaning
for a field is a compile error rather than an empty result:

```rust
use std::time::Duration;

use libtmux::tokio::query::{Filterable as _, QueryIteratorExt as _};
use libtmux::tokio::test::{TestServer, retry_until};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let server = guard.server();
    server.new_session("work").await?;

    let fields = libtmux::tokio::Pane::filter_fields();

    // `pane_active` is a flag, so `.eq(true)` compiles; `.gt(..)` would not.
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

Ask what a session *contains*, and the relation is part of the expression:

```rust
use libtmux::tokio::query::{Filterable as _, QueryIteratorExt as _};
use libtmux::tokio::test::TestServer;
use libtmux::tokio::{SessionTree, WindowTree};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let server = guard.server();

    let receipt = server.new_session("ci").await?;

    let building = server.inspect_session(&receipt).await?;
    building.new_window("build").await?;
    server.new_session("idle").await?;

    let sessions = SessionTree::filter_fields();
    let windows = WindowTree::filter_fields();
    let has_build = sessions
        .windows
        .any(windows.window.window_name.eq("build"));

    // `hierarchy` gathers the whole tree in three tmux commands, not one
    // per object.
    let matched: Vec<_> = server
        .hierarchy()
        .await?
        .iter()
        .matching(&has_build)
        .map(|branch| branch.session.name().to_string_lossy().into_owned())
        .collect();

    assert_eq!(matched, ["ci"], "only the session holding a `build` window");

    guard.shutdown().await?;
    Ok(())
}
```

With the `serde` feature an expression lowers to a [versioned JSON envelope], so
a CLI, a config file, or an MCP tool call can carry one.

## Refresh an object

### [`Window::refresh`] and [`Window::refreshed`]: read a held handle again

A `Session`, `Window` or `Pane` you hold is what tmux said when it was read. A
change made through another handle, another client or tmux itself does not
reach it.

- `refresh().await?` reads it again in place, as Python libtmux's `.refresh()`
  does.
- `refreshed().await?` returns a new handle and leaves the receiver as it was.
- Methods that change an object, such as `rename`, update the handle they were
  called on.

```rust
use libtmux::tokio::test::TestServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let server = guard.server();

    let receipt = server.new_session("work").await?;

    let session = server.inspect_session(&receipt).await?;
    let mut editor = session.new_window("editor").await?;

    // A second handle to the same window renames it.
    let mut other = editor.clone();
    other.rename("build").await?;

    // `editor` still holds what tmux said before the rename.
    assert_eq!(editor.name().to_string_lossy(), "editor");

    let later = editor.refreshed().await?;
    assert_eq!(later.name().to_string_lossy(), "build");
    assert_eq!(editor.name().to_string_lossy(), "editor");

    editor.refresh().await?;
    assert_eq!(editor.name().to_string_lossy(), "build");

    guard.shutdown().await?;
    Ok(())
}
```

[`Session`] and [`Pane`] have the same two methods. A refresh returns
`Error::ObjectGone` when the object no longer exists.

## Pick how commands reach tmux

A plan contains typed operations and references to objects that earlier steps
will create. The Tokio client submits the plan. Each acknowledged session
returns the same receipt as an immediate call.

### [`Planner::explain`]: chain commands, and see what each step costs

A plan, behind the `plan` feature, records commands without sending them. Ask a
planner how it would group them before anything runs:

- [`Planner::Sequential`] is one tmux process per command, and names the one
  that failed.
- [`Planner::Folding`] shares a process between neighbours that need no answer.
- [`Planner::Marked`] also folds in the first commands sent to a new pane.

```rust
use libtmux::tokio::plan::{
    NewSession, Plan, Planner, SendKeys, SplitWindow, StepReason,
};
use libtmux::tokio::test::TestServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let server = guard.server();

    // Recording sends nothing: a step can name a pane an earlier step makes.
    let mut plan = Plan::new();
    let session = plan.add(NewSession::new("build"));
    let pane = plan.add(SplitWindow::new(session.window()).focus());
    plan.add(SendKeys::new(pane).text("echo one").enter());
    plan.add(SendKeys::new(pane).text("echo two").enter());

    assert_eq!(Planner::Sequential.steps(&plan).len(), 4);
    assert_eq!(Planner::Marked.steps(&plan).len(), 2);

    // `explain` names what keeps a step in a process of its own.
    let reasons: Vec<StepReason> = Planner::Folding
        .explain(&plan)
        .into_iter()
        .map(|(_, reason)| reason)
        .collect();
    assert_eq!(
        reasons,
        [StepReason::CreatesId, StepReason::CreatesId, StepReason::Folded],
    );

    let result = server.run_plan(&plan, Planner::Marked).await?;
    assert!(result.is_complete());
    assert_eq!(result.dispatches(), 2);

    guard.shutdown().await?;
    Ok(())
}
```

A qualified control sender can execute the same plan with `sender.run_plan(&plan)`. More:
[the `libtmux` guide](crates/libtmux-tokio/README.md#choosing-how-commands-reach-tmux).

## Watch tmux change

### [`ControlMode::attach`]: hear changes as tmux makes them

Control mode keeps one tmux client attached. It needs the `control-mode`
feature and tmux 3.7 or newer. [`ControlSender::subscribe`] has tmux watch a
numeric or ID projection. [`Server::over_control_mode`] lets a typed handle
share the connection. The example waits for tmux to report the window count.

```rust
use libtmux::tokio::control::{ControlMode, Event, Subscription};
use libtmux::tokio::test::TestServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
# let version = guard.server().daemon_version().await?;
# if !version.has_behavior(&libtmux::tokio::since::CONTROL_ACQUISITION) {
#     guard.shutdown().await?;
#     return Ok(());
# }
    let session = guard.session("watching").await?;
    let (commands, mut events) =
        ControlMode::attach(guard.server(), session.id())
            .await?
            .split();

    commands
        .subscribe("windows", &Subscription::Session, "#{session_windows}")
        .await?;

    // The first report arrives with nothing changed: a subscription reads the
    // value as well as watching it.
    while let Some(event) = events.next_event().await {
        if let Event::SubscriptionChanged { name, value, .. } = event? {
            println!(
                "{} = {}",
                name.to_string_lossy(),
                value.to_string_lossy()
            );
            break;
        }
    }

    commands.unsubscribe("windows").await?;
    events.shutdown().await?;
    guard.shutdown().await?;
    Ok(())
}
```

It prints `windows = 1`. tmux reports a value at most once a second, and says
what it became, not the steps between. More:
[the `libtmux` guide](crates/libtmux-tokio/README.md#watch-tmux-change).

## Load workspaces from YAML

### [`tmux-workspace load`][workspace-cli-load]: build a session from a file

[`tmux-workspace`](crates/tmux-workspace) is a native tmuxp. It
[loads][workspace-cli-load] tmuxp workspace files,
[freezes][workspace-cli-freeze] running sessions back into them, and
[imports][workspace-cli-import] tmuxinator and teamocil projects.

```console
$ cargo install tmux-workspace --locked
```

```console
$ tmux-workspace load ./dev.yaml
```

See [its README](crates/tmux-workspace/README.md) for every command.

## Drive tmux from an agent

### [`tmux-mcp`][mcp-tools]: a tmux server for agents

[`tmux-mcp`](crates/tmux-mcp) is a separate package: a Model Context Protocol
server over this API, read-biased, where every tool that changes state names
exactly what it changes.

```console
$ cargo install tmux-mcp
```

See [its README](crates/tmux-mcp/README.md) for the tool list and for wiring
it into an MCP client.

## Packages

| Crate | Version | What it is |
| --- | --- | --- |
| [`libtmux`](crates/libtmux) | [![libtmux]][libtmux-crate] | Shared types and native client namespaces |
| [`libtmux-core`](crates/libtmux-core) | Source checkout | Offline types, commands and receipts |
| [`libtmux-blocking`](crates/libtmux-blocking) | Source checkout | Native blocking client |
| [`libtmux-tokio`](crates/libtmux-tokio) | Source checkout | Tokio client and control driver |
| [`libtmux-macros`](crates/libtmux-macros) | [![libtmux-macros]][macros-crate] | `#[derive(Filterable)]`, for your own structs |
| [`tmux-mcp`](crates/tmux-mcp) | [![tmux-mcp]][mcp-crate] | A Model Context Protocol server over tmux |
| [`tmux-workspace`](crates/tmux-workspace) | [![tmux-workspace]][workspace-crate] | A tmuxp-style YAML builder |

[libtmux-macros]: https://img.shields.io/crates/v/libtmux-macros.svg?label=libtmux-macros
[macros-crate]: https://crates.io/crates/libtmux-macros
[tmux-workspace]: https://img.shields.io/crates/v/tmux-workspace.svg?label=tmux-workspace
[workspace-crate]: https://crates.io/crates/tmux-workspace

### [`libtmux`](https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux)

The facade exports the shared types and concrete blocking and Tokio clients.

[![libtmux]][libtmux-crate] [API reference][libtmux-api] ·
[docs.rs][libtmux-docsrs] · [Guide][libtmux-guide] ·
[Programs][libtmux-examples]

The examples above are this crate's.

### [`libtmux-macros`](https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux-macros)

`#[derive(Filterable)]`: your own structs take the same typed queries as tmux
objects. `libtmux` re-exports it behind its `derive` feature.

[![libtmux-macros]][macros-crate] [API reference][macros-docsrs] ·
[Guide][macros-guide]

```console
$ cargo add libtmux --features derive
```

[Its README][macros-guide] filters a list of `Job` structs with the expressions
`libtmux` uses for panes.

### [`tmux-workspace`](https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-workspace)

A native tmuxp, as one binary with no Python: it loads workspace files, freezes
running sessions back into them, and imports tmuxinator and teamocil projects.

[![tmux-workspace]][workspace-crate] [Command reference][workspace-cli] ·
[API reference][workspace-api] · [docs.rs][workspace-docsrs] ·
[Guide][workspace-guide]

Describe a session in `dev.yaml`:

```yaml
session_name: dev
windows:
  - window_name: editor
    panes:
      - vim
      - git status
```

Save a running session back to a file:

```console
$ tmux-workspace freeze dev -o dev.yaml
```

### [`tmux-mcp`](https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-mcp)

A Model Context Protocol server over `libtmux`, read-biased: every tool that
changes state names what it changes.

[![tmux-mcp]][mcp-crate] [Tools][mcp-tools] · [API reference][mcp-api] ·
[docs.rs][mcp-docsrs] · [Connect a client][mcp-connect] · [Guide][mcp-guide]

Register it with Claude Code. [Its README][mcp-guide] covers the other
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

- **Native Windows.** tmux does not run there, so use WSL.
- **A mock tmux.** Tests run against a real tmux, because what tmux answers is
  the thing under test. [`libtmux::tokio::test::TestServer`] gives each test an
  isolated socket.
- **Proc macros in `libtmux` itself.** Its own [`Filterable`] implementations
  are hand-written, and the derive exists for structs outside this workspace.

## Support and status

- **Rust 1.85 or newer** for the libraries. `tmux-mcp` needs 1.88.
- **tmux 3.3a or newer.** See the [supported releases][tmux-support].
  [Release candidates][tmux-rc] are not supported.
- **A refusal on two releases.** [`run_shell`] refuses on tmux 3.3a and 3.4,
  which drop the command's output.
- **Linux, including WSL.** macOS is best-effort.
- **Unix only.** Native Windows is unsupported, because tmux does not run
  there.

[Documentation](https://libtmux.org/en/rs/) ·
[API reference][libtmux-api] ·
[docs.rs][libtmux-docsrs] ·
[Design notes](crates/libtmux/docs/design.md) ·
[Changelog](CHANGELOG.md) ·
[Contributing](.github/CONTRIBUTING.md) ·
[AI policy](https://libtmux.org/en/meta/ai-policy/)

Licensed under [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT). Part of the
[libtmux.org](https://libtmux.org/) family alongside
[Python libtmux](https://github.com/tmux-python/libtmux).

[libtmux-api]: https://libtmux.org/en/rs/latest/reference/
[libtmux-docsrs]: https://docs.rs/libtmux/latest/libtmux/
[libtmux-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/README.md
[libtmux-examples]: https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux/examples
[macros-docsrs]: https://docs.rs/libtmux-macros/latest/libtmux_macros/derive.Filterable.html
[macros-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux-macros/README.md#quick-start
[workspace-cli]: https://libtmux.org/en/rs/latest/workspace/cli/
[workspace-cli-load]: https://libtmux.org/en/rs/latest/workspace/cli/load/
[workspace-cli-freeze]: https://libtmux.org/en/rs/latest/workspace/cli/freeze/
[workspace-cli-import]: https://libtmux.org/en/rs/latest/workspace/cli/import/
[workspace-api]: https://libtmux.org/en/rs/latest/workspace/reference/
[workspace-docsrs]: https://docs.rs/tmux-workspace/latest/tmux_workspace/
[workspace-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-workspace/README.md
[mcp-tools]: https://libtmux.org/en/rs/latest/mcp/tools/
[mcp-api]: https://libtmux.org/en/rs/latest/mcp/reference/
[mcp-docsrs]: https://docs.rs/tmux-mcp/latest/tmux_mcp/
[mcp-connect]: https://libtmux.org/en/rs/latest/mcp/guides/connect-client/
[mcp-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/README.md
[tmux-support]: https://libtmux.org/en/tmux/guides/tmux-support/
[tmux-rc]: https://libtmux.org/en/tmux/guides/tmux-support/#release-candidates

[Typed field handles]: https://libtmux.org/en/rs/latest/reference/libtmux_core-query-filterable-filter_fields/
[versioned JSON envelope]: https://libtmux.org/en/rs/latest/reference/libtmux_core-query-filterexpr/
[Three switches]: https://libtmux.org/en/rs/latest/concepts/transports/
[Folding]: https://libtmux.org/en/rs/latest/reference/libtmux_core-plan-planner-folding/
[`Window::refresh`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-window-refresh/
[`Window::refreshed`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-window-refreshed/
[`Session`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-session/
[`Pane`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane/
[`Planner::explain`]: https://libtmux.org/en/rs/latest/reference/libtmux_core-plan-planner-explain/
[`Planner::Sequential`]: https://libtmux.org/en/rs/latest/reference/libtmux_core-plan-planner-sequential/
[`Planner::Folding`]: https://libtmux.org/en/rs/latest/reference/libtmux_core-plan-planner-folding/
[`Planner::Marked`]: https://libtmux.org/en/rs/latest/reference/libtmux_core-plan-planner-marked/
[`ControlMode::attach`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-control-controlmode-attach/
[`ControlSender::subscribe`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-control-controlsender-subscribe/
[`Server::over_control_mode`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server-over_control_mode/
[Control mode]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-control-controlmode/
[`run_shell`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server-run_shell/
[`Filterable`]: https://libtmux.org/en/rs/latest/reference/libtmux_core-query-filterable/
[`libtmux::tokio::test::TestServer`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-test-testserver/
[`Filterable::filter_fields`]: https://libtmux.org/en/rs/latest/reference/libtmux_core-query-filterable-filter_fields/
[`Pane::send_line`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-send_line/
[`Window::split`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-window-split/
[`Pane::split`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-split/
[`Pane::kill`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-kill/
[`Pane::resize`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-resize/
[`Window::resize`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-window-resize/
[`Pane::capture`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-capture/
[`Pane::capture_with`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-pane-capture_with/
[`Window::kill`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-window-kill/
[`Session::kill`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-session-kill/
[`Server::kill`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-server-kill/
[`Session::set_option`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-session-set_option/
[`Session::typed_option`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-session-typed_option/
[`Session::set_hook`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-session-set_hook/
[`SessionTree`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-sessiontree/
[`WindowTree`]: https://libtmux.org/en/rs/latest/reference/libtmux_tokio-windowtree/
