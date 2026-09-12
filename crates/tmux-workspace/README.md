# tmux-workspace

Build tmux workspaces from [tmuxp](https://tmuxp.git-pull.com/)-style YAML,
using [libtmux](https://docs.rs/libtmux).

It is a library, with no command to run: a program reads the file and builds
it. It reads tmuxp's own example files, and ignores five of tmuxp's keys;
[Reading a tmuxp file](#reading-a-tmuxp-file) names them and says where the
two disagree.

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
    // Your own code reads the file, so a `./` start directory is the file's,
    // and uses `libtmux::Server::new()?`:
    //   let workspace = Workspace::from_file("dev.yaml")?;
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

## Reading a tmuxp file

A file written for tmuxp builds the session tmuxp would build, including
where tmuxp's behaviour is surprising:

- A lone `pane`, `blank` or empty `-` is a pane with no command. Among other
  commands the word is typed.
- Commands are typed after a space, which keeps them out of history in a
  shell set to ignore such lines, unless `suppress_history: false`. The space
  reaches whatever the pane runs, a program as much as a shell.
- `enter: false` on a command holds for the commands after it in that pane,
  so the next one is typed onto the same line. A pane's `enter: false` covers
  its `shell_command_before` commands, which are typed first.
- `sleep_before` and `sleep_after` hold the same way, and are waited for in
  tmux: each is a `libtmux::plan::Pause` between the commands it separates.
- `~` and `$NAME` or `${NAME}` expand from the loading process's environment
  in names, start directories, and `environment` and option values. An unset
  variable stays as written, and there is no escape: a frozen name holding
  `$HOME` reads back as the home directory.
- A window's relative `start_directory` joins the session's. One starting
  with `.` starts from the directory it would inherit, else from the file's.
  tmuxp's `start-directory.yaml` names a window for the file's directory that
  tmuxp's loader, and this crate, put in the session's. A pane's other
  relative path is not joined to its window's: it starts from the current
  directory, as tmux would start it.

It differs where following tmuxp would be unsafe or impossible:

- Commands are typed as written, for the pane's shell to expand. tmuxp
  expands variables in them first, which reads a variable's value as shell
  code.
- `~name` in a start directory is refused, not looked up; elsewhere it stays
  as written.
- A `.` path with nothing to inherit, and a null among commands, crash
  tmuxp. Here the first starts from the file's directory and the second is
  an error naming its line.
- `window_shell` starts the window's first pane only, and a pane's
  `environment` adds to its window's rather than replacing it.

Five of tmuxp's keys are not acted on, and are listed in `unsupported_keys`
along with any key tmuxp does not have: `before_script`, `plugins`,
`options_after`, and a pane's `shell` and `shell_command_before`.

## Install

```console
$ cargo add tmux-workspace@0.1.0-alpha.12
```

<details>
<summary>Cargo.toml</summary>

```toml
[dependencies]
tmux-workspace = "0.1.0-alpha.12"
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
