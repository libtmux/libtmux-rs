# tmux-workspace

Build tmux workspaces from [tmuxp](https://tmuxp.git-pull.com/)-style YAML,
using [libtmux](https://docs.rs/libtmux).

> **Alpha.** The API changes between releases, including in ways that will not
> be called out as breaking, because nothing here is stable yet. Cargo will not
> resolve a prerelease unless the requirement names one, so a plain `0.1`
> requirement does not pick this up: depend on the exact version below, and
> expect to edit it.

Describe the workspace:

```yaml
session_name: dev
windows:
  - window_name: editor
    panes:
      - vim
      - htop
```

Freeze a session someone built by hand back into one:

```rust
use libtmux::test::TestServer;
use tmux_workspace::{freeze, Workspace};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Runs for real, against an isolated tmux under `/tmp/libtmux-rs-test/`.
    // Your own code reaches a session someone left running through
    // `libtmux::Server::new()?`.
    let guard = TestServer::new().await?;
    let session = guard.server().new_session("dev").await?;

    let workspace = freeze(&session).await?;
    let yaml = workspace.to_yaml();

    // Keep it wherever the project keeps them:
    //   std::fs::write("dev.yaml", &yaml)?;
    assert_eq!(Workspace::from_yaml(&yaml)?, workspace);

    guard.shutdown().await?;
    Ok(())
}
```

What freezing recovers is the shape -- windows, panes, working directories,
which is focused. What it cannot is history: tmux remembers what a pane is
running, not the command someone typed to start it.

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
    assert_eq!(session.windows().await?.len(), 1);

    guard.shutdown().await?;
    Ok(())
}
```

Workspace builds and native CLI loads validate every configured layout before
scripts run or sessions change. Unique name abbreviations use the running
daemon's version; a cold endpoint uses the selected client. Custom layouts need
a checksum, a nonempty tree and enough pane cells, with at most 256 nested
groups. Geometry correction and pruning remain tmux's responsibility. An empty
layout string leaves the default arrangement in place.

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
    assert_eq!(guard.server().sessions().await?.len(), 0, "nothing ran");

    guard.shutdown().await?;
    Ok(())
}
```

## Install

```console
$ cargo add tmux-workspace@0.1.0-alpha.11
```

<details>
<summary>Cargo.toml</summary>

```toml
[dependencies]
tmux-workspace = "0.1.0-alpha.11"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

</details>

## Why this crate exists

It exists to drive the `libtmux` public API from outside the crate that
defines it. Tests written inside `libtmux` can reach `pub(crate)` items and
can be written around whatever shape the internals happen to have; a separate
crate cannot. So this one builds something real — sessions, windows, panes,
splits, and the layout a workspace file describes — using only what a
published consumer can see. An API that is awkward to use from here is
awkward for everyone, and that shows up as a compile error rather than as a
review comment.

Being published is part of that rather than beside it. A crate that is only
ever built inside its own workspace never proves its dependency requirements
resolve, and this one could not have been published at all until `libtmux`
shipped the `plan` feature it asks for — which is exactly the kind of thing
that is invisible from inside the tree.

## CLI bootstrap

`before_script` parses executable and arguments with shell-style quoting.
An executable starting with `.` resolves from the workspace file's directory;
absolute executables and commands found through `PATH` retain their meaning.
The child runs in the session's `start_directory` when set, or the caller's
working directory otherwise. Arguments retain spaces and empty values, with
no shell expansion.

## CLI logging

`tmux-workspace load --log-file PATH` appends JSON records to a regular file.
New files have owner-only permissions; existing contents and permissions are
preserved. Invalid destinations, including symlinks, are rejected before tmux
or Python runs.

`--log-level info` includes load events. `debug` also records child output,
limited to 1 MiB of decoded UTF-8 per stream per child. Lifecycle records omit
captured streams, including nested result captures; diagnostic messages are
preserved. The default is `warning`.
The level never suppresses command errors or changes their exit status. A later
file-write failure disables logging and reports one warning after the primary
result or error, unless the chosen level suppresses warnings.

## Development

Format edits confined to this package with:

```console
$ cargo fmt --package tmux-workspace
```

The full gate checks formatting across the Cargo workspace.

```console
$ cargo test -p tmux-workspace
```

The tests drive a real tmux on an isolated socket, so tmux must be on `$PATH`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE)
or [MIT license](LICENSE-MIT) at your option.
