# tmux-workspace

Build tmux workspaces from [tmuxp](https://tmuxp.git-pull.com/)-style YAML,
using [libtmux](https://docs.rs/libtmux).

> **Alpha, and not published.** This crate is `publish = false` on purpose —
> see [Why this is not on crates.io](#why-this-is-not-on-cratesio). Use it by
> path, or read it as a worked example of the `libtmux` API.

Describe the workspace:

```yaml
session_name: dev
windows:
  - window_name: editor
    panes:
      - vim
      - htop
```

Build it:

```rust
use libtmux::test::TestServer;
use tmux_workspace::{Workspace, WorkspaceBuilder};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Runs for real, against an isolated tmux under `/tmp/libtmux-rs-test/`.
    // Your own code reads the file and uses `libtmux::Server::new()?`:
    //   let source = std::fs::read_to_string("dev.yaml")?;
    let source = "
session_name: dev
windows:
  - window_name: editor
    panes: [/bin/sh, /bin/sh]
";
    let workspace = Workspace::from_yaml(source)?;

    let guard = TestServer::new().await?;
    let session = WorkspaceBuilder::new(guard.server()).build(&workspace).await?;

    assert_eq!(session.name().to_string_lossy(), "dev");
    assert_eq!(session.try_windows().await?.len(), 1);

    guard.shutdown().await?;
    Ok(())
}
```

## See what it would do first

`plan` returns the work without doing any of it, so a caller can print it,
count it, or decide against it:

```rust
use libtmux::test::TestServer;
use tmux_workspace::{Workspace, WorkspaceBuilder};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = Workspace::from_yaml(
        "
session_name: dev
windows:
  - window_name: editor
    panes: [/bin/sh, /bin/sh]
",
    )?;

    let guard = TestServer::new().await?;
    let plan = WorkspaceBuilder::new(guard.server()).plan(&workspace);

    // Nothing has reached tmux, but every command is already known.
    let commands: Vec<_> = plan
        .preview()
        .into_iter()
        .flatten()
        .map(|command| command.summary().to_string())
        .collect();

    assert!(commands[0].contains("new-session"));
    assert_eq!(guard.server().try_sessions().await?.len(), 0, "nothing ran");

    guard.shutdown().await?;
    Ok(())
}
```

## Use it

Not on crates.io, so depend on it by path:

```toml
[dependencies]
tmux-workspace = { path = "crates/tmux-workspace" }
```

## Why this is not on crates.io

`publish = false` is the point of this crate rather than an oversight.

It exists to drive the `libtmux` public API from outside the crate that
defines it. Tests written inside `libtmux` can reach `pub(crate)` items and
can be written around whatever shape the internals happen to have; a separate
crate cannot. So this one builds something real — sessions, windows, panes,
splits, and the layout a workspace file describes — using only what a
published consumer can see. An API that is awkward to use from here is
awkward for everyone, and that shows up as a compile error rather than as a
review comment.

That job does not need a crates.io listing, so it does not have one. The
publishing metadata the sibling crates carry — keywords, categories, an
`include` list, a docs.rs section — is deliberately absent, because none of it
would mean anything for a crate that is never uploaded.

## Development

```console
$ cargo test -p tmux-workspace
```

The tests drive a real tmux on an isolated socket, so tmux must be on `$PATH`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE)
or [MIT license](LICENSE-MIT) at your option.
