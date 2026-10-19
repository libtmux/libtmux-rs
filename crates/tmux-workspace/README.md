<!-- libtmux-logo -->
<div align="center">
  <picture>
    <source srcset="https://raw.githubusercontent.com/libtmux/libtmux-rs/HEAD/crates/tmux-workspace/assets/logo.svg" type="image/svg+xml">
    <img src="https://raw.githubusercontent.com/libtmux/libtmux-rs/HEAD/crates/tmux-workspace/assets/logo.png" width="128" height="128" alt="libtmux for Rust workspace">
  </picture>
</div>
<!-- /libtmux-logo -->

<div align="center">

# tmux-workspace

Load tmux sessions from YAML, and save running ones back to it. A native
[tmuxp](https://tmuxp.git-pull.com/): it reads tmuxp's workspace files, finds
them where tmuxp does and takes tmuxp's flags, as one binary with no Python.

</div>

[Load][cli-load] a session from a file, [freeze][cli-freeze] a running one
back into one, [import][cli-import] a tmuxinator or teamocil project, and
[search][cli-search] the workspaces you have. The same package is a
[library](#using-it-from-rust).

> **Alpha.** Commands and flags can change between releases, and changes are
> listed in the [changelog][changelog] rather than flagged as breaking.
> `cargo install` skips prereleases unless asked, so the command below names
> the version.

**Contents** — [Quick start](#quick-start) ·
[Load sessions](#load-sessions) ·
[Save, convert, and import](#save-convert-and-import) ·
[Find workspaces](#find-workspaces) · [Shell setup](#shell-setup) ·
[Coming from tmuxp](#coming-from-tmuxp) · [Scripting](#scripting) ·
[Let an agent watch it](#let-an-agent-watch-it) ·
[Using it from Rust](#using-it-from-rust) · [Packages](#packages) ·
[What it leaves out](#what-it-leaves-out) ·
[Support and status](#support-and-status) · [Documentation](#documentation)

## Quick start

Install the command. It needs tmux 3.3a or newer on `PATH`, and Rust 1.85 to
build:

```console
$ cargo install tmux-workspace \
    --version 0.1.0-alpha.16 \
    --locked
```

[`cargo binstall`](https://github.com/cargo-bins/cargo-binstall) fetches a
prebuilt binary for Linux or macOS when the release carries one, and builds
from source when it does not:

```console
$ cargo binstall tmux-workspace --version 0.1.0-alpha.16
```

Describe a session in `dev.yaml`:

```yaml
session_name: dev
windows:
  - window_name: editor
    layout: main-vertical
    panes:
      - vim
      - git status
  - window_name: monitor
    panes:
      - top
```

Then load it:

```console
$ tmux-workspace load ./dev.yaml
```

That builds the session and attaches to it. Run inside tmux, it asks whether
to switch to the new session, keep it in the background, or add its windows
to the session you are in.

## Load sessions

### [`tmux-workspace load`][cli-load]: build a workspace

[`load`][cli-load] builds a workspace, and attaches to it unless told not to.
Build the [`.tmuxp.yaml`] in a project directory:

```console
$ tmux-workspace load .
```

Keep workspaces in `~/.config/tmuxp/` and load them by name from anywhere:

```console
$ tmux-workspace load dev
```

See what one would build, with sessions, windows, panes, directories and
commands, without touching tmux or running its [`before_script`]:

```console
$ tmux-workspace load --dry-run dev
```

Load several, attaching to the last:

```console
$ tmux-workspace load api web
```

Build without attaching:

```console
$ tmux-workspace load -d dev
```

Build under a different session name:

```console
$ tmux-workspace load -s scratch dev
```

Add the windows to the session you are in:

```console
$ tmux-workspace load -a tools
```

Build on a separate tmux server, named or by socket path:

```console
$ tmux-workspace load -L work dev
```

## Save, convert, and import

### [`tmux-workspace freeze`][cli-freeze]: save a running session

```console
$ tmux-workspace freeze dev -o dev.yaml
```

Freezing recovers windows, panes, layouts and working directories. It cannot
recover the command someone typed to start a pane, only what the pane is
running now, so read the file before relying on it.

### [`tmux-workspace convert`][cli-convert]: change a file's format

It converts between YAML and JSON, shows the destination, and asks first:

```console
$ tmux-workspace convert dev.yaml
```

### [`tmux-workspace import`][cli-import]: bring in a tmuxinator or teamocil project

[`import tmuxinator`][cli-import-tmuxinator] and
[`import teamocil`][cli-import-teamocil] ask, then save the project where
[`load`][cli-load] finds it by name. Here that is `~/.config/tmuxp/api.yaml`,
so [`tmux-workspace load api`][cli-load] builds it next:

```console
$ tmux-workspace import tmuxinator api
```

```console
$ tmux-workspace import teamocil dev
```

### [`tmux-workspace edit`][cli-edit]: open a workspace in `$EDITOR`

```console
$ tmux-workspace edit dev
```

## Find workspaces

[`ls`][cli-ls] and [`search`][cli-search] read the same places [`load`][cli-load] does.

### [`tmux-workspace ls`][cli-ls]: list what it can find

It lists every workspace it can find, grouped by directory:

```console
$ tmux-workspace ls --tree
```

### [`tmux-workspace search`][cli-search]: find a workspace by what it holds

`window:`, `session:`, `pane:` and `path:` limit a pattern to one field:

```console
$ tmux-workspace search window:logs
```

## Shell setup

### [`tmux-workspace --generate`][cli-completion]: completions and the manual page

The binary generates both. For zsh, with `~/.zfunc` on `fpath`:

```console
$ tmux-workspace --generate zsh > ~/.zfunc/_tmux-workspace
```

```console
$ tmux-workspace --generate man > ~/.local/share/man/man1/tmux-workspace.1
```

bash, fish, PowerShell and elvish are in the
[command reference](docs/cli.md#generating-completions-and-the-manual). Every
command has examples under `--help`.

### [`tmux-workspace debug-info`][cli-debug-info]: attach this to a bug report

```console
$ tmux-workspace debug-info
```

## Coming from tmuxp

Your workspace files load unchanged: the same keys, the same `.tmuxp.yaml`
project files, the same directories (`$TMUXP_CONFIGDIR`, `~/.config/tmuxp`,
`~/.tmuxp`) and the same [`load`][cli-load] flags (`-d`, `-s`, `-a`, `-L`,
`-S`, `-f`, `-2`, `-y`). Where it differs:

- An unknown key is refused rather than ignored. Prefix a key with `x-` to
  carry your own fields; [`convert`][cli-convert] preserves them.
- A window that names no [`layout`] is tiled rather than stacked.
- A [`session_name`] containing `:` or `.` is refused: tmux reads those as
  window and pane separators.
- `$VAR` in a command is expanded by the pane's shell, with your value passed
  to it, rather than pasted into the command text: a value holding `;` stays
  a value.
- [`freeze`][cli-freeze] saves only where `-o` says, or prints with
  [`--json`].
- [`plugins`] and custom `workspace_builder`s run through tmuxp itself, so they
  need tmuxp 1.74.x and a Python libtmux that requires tmux 3.3a or newer.
  Select that environment with [`TMUX_WORKSPACE_PYTHON`].

[The command reference](docs/cli.md) has the complete list, and its
[Python extensions](docs/cli.md#python-extensions) section names the Python
libtmux change they depend on.

## Scripting

### [`--json` and `--ndjson`][output]: output a program can read

[`--json`] prints one JSON document, and [`--ndjson`] streams one event per
line as windows and panes are created. A load under either needs `-d`, because
nothing is attached:

```console
$ tmux-workspace --ndjson load -d api web
```

A failure writes `{"schema_version":1,"code":"...","message":"..."}` to
stderr and exits 1, or 2 when the command line itself is wrong. The codes and
events are in the [command reference](docs/cli.md#machine-output).

## Let an agent watch it

[`tmux-mcp`](https://crates.io/crates/tmux-mcp) gives an MCP client typed,
read-only access to the sessions a workspace builds. Load on a named server
and point the server at the same one:

```console
$ tmux-workspace load -d -L agents dev
```

```console
$ LIBTMUX_TOOLSETS=inspect tmux-mcp -L agents
```

Configure your MCP client to launch that second command. The workspace keeps
running after the client disconnects. The
[tmux-mcp README][mcp-guide] covers installing it and wiring clients.

## Using it from Rust

The same package is a library, through [libtmux](https://docs.rs/libtmux). It
reads a document the way the command does, tmuxp's way, and the command
reference lists [where the two differ](docs/cli.md#where-the-command-and-the-library-differ).

```console
$ cargo add tmux-workspace@0.1.0-alpha.16 --no-default-features
```

`--no-default-features` leaves the command and its dependencies out. The
server handle below comes from `libtmux`, so add that too:

```console
$ cargo add libtmux@0.1.0-alpha.16
```

### [`Workspace::from_yaml`]: read a workspace

It parses a document and records the keys it does not model in
[`unsupported_keys`]. [`WorkspaceBuilder::build`] turns the result into a
session, and [`freeze`] goes the other way.

### [`WorkspaceBuilder::plan`]: see the commands first

It previews what loading a workspace would send to tmux, and sends nothing:

```rust
use tmux_workspace::{Workspace, WorkspaceBuilder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = Workspace::from_yaml(
        "
session_name: dev
windows:
  - window_name: editor
    panes: [vim, htop]
",
    )?;

    // Building a handle starts nothing; `plan` is where the commands appear.
    let server = libtmux::Server::new()?;
    let plan = WorkspaceBuilder::new(&server).plan(&workspace);

    let commands: Vec<String> = plan
        .preview()
        .into_iter()
        .flatten()
        .map(|command| command.summary().to_string())
        .collect();

    assert!(commands[0].contains("new-session"));
    Ok(())
}
```

The [API reference][api-reference] has the same items on libtmux.org, and
[docs.rs][docs-rs] has the rest.

## Packages

### [`tmux-workspace`](https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-workspace): the command and the library

This crate. [`tmux-workspace`][cli-manual] is the command, and
[`Workspace`] and [`WorkspaceBuilder`] are the library.

### [`libtmux`](https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux): the client under it

The async tmux client the builder drives. [Its README][libtmux-guide] covers
filtering, waiting, and control mode.

### [`tmux-mcp`](https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-mcp): a server for agents

Lets an MCP client read the sessions a workspace built, as
[above](#let-an-agent-watch-it).

## What it leaves out

- **Silent unknown keys.** A key it does not know is refused, unless it starts
  with `x-`.
- **The command typed to start a pane.** [`freeze`][cli-freeze] recovers what a
  pane is running now, because tmux keeps no history of what was typed.
- **Python without tmuxp.** [`plugins`] and `workspace_builder` need a tmuxp
  environment. Every other command runs without Python.
- **Native Windows.** tmux is unavailable there; WSL works.

## Support and status

- **Alpha.** Commands and flags can change between releases. Pin an exact
  version.
- **tmux 3.3a or newer**, on `PATH`, and **Rust 1.85** to build.
- **Linux, including WSL.** CI runs the workspace's tests on Linux. On macOS,
  CI runs them against Homebrew's current tmux on pushes to `master` and manual
  runs, and not on pull requests.
- **Prebuilt binaries** for Linux and macOS come with a release when it
  carries them.

## Documentation

| | |
|---|---|
| [Command reference][cli-manual] | Every command and flag, and the machine-readable output |
| [`docs/cli.md`](docs/cli.md) | How a workspace is found, what a load does when it cannot finish, and where the command departs from tmuxp |
| [API reference][api-reference] | The library on libtmux.org |
| [docs.rs][docs-rs] | The library as rustdoc |
| [Changelog][changelog] | What each release changed |

## Development

The tests drive a real tmux on an isolated socket, so tmux must be on `$PATH`:

```console
$ cargo test -p tmux-workspace --all-features
```

They parse every command on this page against the real argument parser, and
load every workspace file it shows.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE)
or [MIT license](LICENSE-MIT) at your option.

[changelog]: https://github.com/libtmux/libtmux-rs/blob/master/CHANGELOG.md
[cli-manual]: https://libtmux.org/en/rs/latest/workspace/cli/
[cli-load]: https://libtmux.org/en/rs/latest/workspace/cli/load/
[cli-freeze]: https://libtmux.org/en/rs/latest/workspace/cli/freeze/
[cli-convert]: https://libtmux.org/en/rs/latest/workspace/cli/convert/
[cli-import]: https://libtmux.org/en/rs/latest/workspace/cli/import/
[cli-import-tmuxinator]: https://libtmux.org/en/rs/latest/workspace/cli/import-tmuxinator/
[cli-import-teamocil]: https://libtmux.org/en/rs/latest/workspace/cli/import-teamocil/
[cli-edit]: https://libtmux.org/en/rs/latest/workspace/cli/edit/
[cli-ls]: https://libtmux.org/en/rs/latest/workspace/cli/ls/
[cli-search]: https://libtmux.org/en/rs/latest/workspace/cli/search/
[cli-completion]: https://libtmux.org/en/rs/latest/workspace/cli/completion/
[cli-debug-info]: https://libtmux.org/en/rs/latest/workspace/cli/debug-info/
[api-reference]: https://libtmux.org/en/rs/latest/workspace/reference/
[docs-rs]: https://docs.rs/tmux-workspace/latest/tmux_workspace/
[libtmux-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/README.md
[mcp-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/README.md
[output]: https://libtmux.org/en/rs/latest/workspace/reference/output/

[`.tmuxp.yaml`]: https://libtmux.org/en/rs/latest/workspace/configuration/directories/
[`before_script`]: https://libtmux.org/en/rs/latest/workspace/configuration/hooks/
[`layout`]: https://libtmux.org/en/rs/latest/workspace/configuration/layouts/
[`session_name`]: https://libtmux.org/en/rs/latest/workspace/configuration/session/
[`plugins`]: https://libtmux.org/en/rs/latest/workspace/configuration/hooks/
[`--json`]: https://libtmux.org/en/rs/latest/workspace/reference/output/
[`--ndjson`]: https://libtmux.org/en/rs/latest/workspace/reference/output/
[`TMUX_WORKSPACE_PYTHON`]: https://libtmux.org/en/rs/latest/workspace/reference/compatibility/
[`Workspace`]: https://libtmux.org/en/rs/latest/workspace/reference/config-workspace/
[`Workspace::from_yaml`]: https://libtmux.org/en/rs/latest/workspace/reference/config-workspace-from_yaml/
[`unsupported_keys`]: https://libtmux.org/en/rs/latest/workspace/reference/config-workspace-unsupported_keys/
[`WorkspaceBuilder`]: https://libtmux.org/en/rs/latest/workspace/reference/src-workspacebuilder/
[`WorkspaceBuilder::plan`]: https://libtmux.org/en/rs/latest/workspace/reference/src-workspacebuilder-plan/
[`WorkspaceBuilder::build`]: https://libtmux.org/en/rs/latest/workspace/reference/src-workspacebuilder-build/
[`freeze`]: https://libtmux.org/en/rs/latest/workspace/reference/freeze-freeze/
