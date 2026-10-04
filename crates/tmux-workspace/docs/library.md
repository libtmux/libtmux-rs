Build tmux workspaces from [tmuxp](https://tmuxp.git-pull.com/)-style YAML,
using [libtmux](https://docs.rs/libtmux).

This is the library. The package also ships the `tmux-workspace` command, a
native tmuxp: `cargo install tmux-workspace --version 0.1.0-alpha.15`, and
see its [README](https://crates.io/crates/tmux-workspace) and its
[reference](crate::command).
The library reads tmuxp's own example files and, by default, records rather
than acts on a handful of tmuxp's keys;
[Reading a tmuxp file](#reading-a-tmuxp-file) names them and says where it
still departs from tmuxp.

> **Alpha.** The API changes between releases, including in ways that will not
> be called out as breaking, because nothing here is stable yet. Cargo will not
> resolve a prerelease unless the requirement names one, so a plain `0.1`
> requirement does not pick this up: depend on the exact version, and expect
> to edit it.

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
    let session =
        WorkspaceBuilder::new(guard.server()).build(&workspace).await?;

    assert_eq!(session.name().to_string_lossy(), "dev");
    assert_eq!(session.windows().await?.len(), 1);

    guard.shutdown().await?;
    Ok(())
}
```

Builds validate every configured layout before sessions change. Unique name
abbreviations use the running daemon's version; a cold endpoint uses the
selected client. Custom layouts need a checksum, a nonempty tree and enough
pane cells, with at most 256 nested groups. Geometry correction and pruning
remain tmux's responsibility. An empty layout string leaves the default
arrangement in place.

`cargo run --example build` does all of this end to end: parse, list the
plan's steps, build, and freeze the result back to YAML.

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
- A window that names no `layout` is tiled (an even grid). tmuxp stacks it
  instead, halving each split from the last. With no explicit `focus`, the
  last pane a window builds stays active, as tmuxp leaves it.
- `window_shell` is the default shell for every pane in the window, not only
  the one it comes with, as tmuxp's own builder does (`get_pane_shell`); a
  pane's own `shell` overrides it for that pane.
- A pane's own `environment` replaces its window's, rather than adding to
  it, as tmuxp's builder does.
- `shell_command_before` trickles down through the workspace, the window
  and the pane, in that order, ahead of the pane's own `shell_command`.
- `<<: *anchor` and `<<: [*a, *b]` merge keys resolve at every level, as
  `PyYAML`'s safe loader does.
- A missing or null `windows` key is refused, as tmuxp's own schema
  validation requires; an empty list is not, and keeps the window tmux made.

It differs where following tmuxp would be unsafe or impossible:

- Commands are typed as written, for the pane's shell to expand, and the
  loader's value of each variable a command, `window_shell` or a pane's own
  `shell` names is added to that pane's environment unless the document sets
  it. tmuxp pastes the value into the command instead, which reads a
  variable's value as shell code.
- `~name` in a start directory is refused, not looked up; elsewhere it stays
  as written.
- A `.` path with nothing to inherit, and a null among commands, crash
  tmuxp. Here the first starts from the file's directory and the second is
  an error naming its line.

Three of tmuxp's keys are not acted on, and are listed in `unsupported_keys`
along with any key tmuxp does not have: `before_script`, `plugins`, and a
window's `options_after`.

[`Workspace::from_yaml`] and [`Workspace::from_file`] record a key they do
not act on in `unsupported_keys` rather than refusing it, so a richer tmuxp
file still loads: the three above, plus `workspace_builder`,
`workspace_builder_options`, `config` and `socket_name` on the workspace.
[`Workspace::from_yaml_strict`] and [`Workspace::from_file_strict`] refuse
any other unrecognized key instead, unless it starts with `x-`, which is the
`tmux-workspace` command's own policy. `cargo run --example strict` reads one
document both ways.

The `tmux-workspace` command has its own parser and builder but reads a
document the same way, tmuxp's; the last section of
[its reference](crate::command) lists where it differs.

## Install

The command is a default feature; a library dependent leaves it, and clap
with it, out:

```console
$ cargo add tmux-workspace@0.1.0-alpha.15 --no-default-features
```

<details>
<summary>Cargo.toml</summary>

```toml
[dependencies]
tmux-workspace = { version = "0.1.0-alpha.15", default-features = false }
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
