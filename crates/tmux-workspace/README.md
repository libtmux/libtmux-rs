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

## CLI listing

`tmux-workspace ls --tree` groups discovered files by their directory, keeping
local ancestors before the selected global directory. `--full` includes each
configuration beneath its file. Human labels and paths escape control
characters; JSON and NDJSON retain their original values and record shapes.

## CLI extensions

`plugins` accepts a list of strings; an absent or empty list keeps the native
builder. An absent, null, or empty `workspace_builder` also keeps the native
builder. A nonempty plugin list or builder string selects the explicit Python
bridge, which requires tmuxp 1.74.0 and accepts `TMUX_WORKSPACE_PYTHON`.
Other value types are rejected before any input runs scripts or changes tmux.

Native `workspace_builder_options` accepts only `pane_readiness`; unknown fields
fail before any input runs scripts or changes tmux. The default is `auto`;
`always` or `true` waits for readiness, while `never` or `false` disables that
wait. An absent or null value also selects `auto`. Documents delegated to Python
retain their additional builder options, and conversion preserves arbitrary
fields.

## CLI imports

`import tmuxinator` and `import teamocil` validate the converted native workspace
shape before returning or saving it. Unknown fields, invalid types, conflicting aliases,
and unsupported behavior fail without replacing an existing destination. Generic
`convert` still preserves arbitrary document fields.

Tmuxinator imports retain ordered windows and panes, directories, layouts and
sequential command arrays. A window command array stays in one pane. `pre_window`
lists keep their `; ` grouping; per-window `pre` lists keep their `&&` grouping
and require explicit panes. `synchronize: after` enables synchronization after
command delivery.
Project lifecycle hooks, endpoint/runtime settings, named panes and synchronization
before pane creation require the source tool and are refused. So is ERB markup:
tmuxinator expands it through Ruby before parsing, and no native reader does,
so an unexpanded template fails before output or overwrite.

Teamocil imports accept a named session, ordered windows, directories, layouts,
window options, pane commands and focus. `commands` lists retain their `; ` grouping;
legacy `splits` and `cmd` are also accepted. Legacy filters, `clear`, pane widths,
and active `synchronize-panes` options are refused. Teamocil evaluates no
templates, so `<%` in a Teamocil source is ordinary text and is preserved. Both
formats select the first pane by default; Teamocil's first explicit focus takes
precedence.

The imported root is absolute, anchored to the import invocation's directory,
including when the source omits it. Relative window directories resolve against
that root, so saving elsewhere keeps the working directory. Import does not check
directory existence or require tmux or Python. The selected tmux validates layout
compatibility during load. Native loads reject document
`config` and `socket_name` before any input runs scripts or changes tmux; use the
CLI endpoint flags. Explicit Python extensions retain their existing route.

## CLI bootstrap

`before_script` parses executable and arguments with shell-style quoting.
An executable starting with `.` resolves from the workspace file's directory;
absolute executables and commands found through `PATH` retain their meaning.
The child runs in the session's `start_directory` when set, or the caller's
working directory otherwise. Arguments retain spaces and empty values, with
no shell expansion.

## CLI cancellation

`SIGINT` and `SIGTERM` interrupt asynchronous CLI work with exit status 130.
An interrupted load reports completed inputs and acknowledged session,
window, and pane IDs in its retained state. JSON and NDJSON include the same
state as the error diagnostic. Changes remain applied; cancellation does not
roll them back. After a mutating phase begins, `outcome_unknown: true` warns
that additional effects may have applied without an acknowledged receipt.

Before-script output collects up to 1 MiB per stream, but enters the retained
state only when the script finishes. Interruption can omit that unfinished
capture; output already streamed or logged keeps its original destination.
Captured children run in an owned process group. Cancellation stops that group,
including descendants that remain in it. Human bootstrap scripts retain terminal
stdin and temporarily own its foreground group; exit, failure and cancellation
restore the original foreground group and terminal settings. Ctrl-Z suspends the
CLI job after restoring the terminal. Resume it with `fg`; `bg` leaves it stopped
until its job owns the foreground again. Machine bootstrap stdin remains closed.
A successful child may leave a background service running once it closes both
captured streams. Children that deliberately leave the owned group are outside
this cleanup boundary.

The terminal lifecycle regression runs on Linux; interactive macOS behavior
has not been executed there. On Unix targets whose current bindings lack the
required safe, non-reaping child observer, including Cygwin, NetBSD and OpenBSD,
scripted loads and Python extension loads fail during input preflight, before
target lookup or mutation. Other captured commands fail before spawning.
Nonscripted operations retain their existing platform support. This is a
binding limitation, not a claim that those operating systems lack `waitid`.

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

## CLI generation

`--generate schema` exports the command graph, including positional indices,
arity, aliases, groups, conflicts, and overrides. Schema version 1 retains its
existing fields. Numeric bounds and overrides come from the same declarations
that configure the parser; other argument fields use clap reflection. Runtime
environment rules are labeled separately from parser bindings; current
environment values are not included in the metadata.

`--generate man` writes one roff manual containing every command and nested
importer. Completion formats remain `bash`, `zsh`, `fish`, `powershell`, and
`elvish`. Generation does not require tmux or Python.

With `--json`, generation returns a successful `generate` result containing
`artifact.format`, `artifact.encoding`, and `artifact.content`. With `--ndjson`,
it emits one `completed` event carrying that artifact. Content retains the
exact UTF-8 bytes of human generation, including its final newline. Help
remains human text with either machine flag.

## Inspect a loaded workspace with MCP

Save the opening YAML example as `dev.yaml`. From the repository root,
install the workspace CLI:

```console
$ cargo install --locked --path crates/tmux-workspace
```

Install the MCP executable:

```console
$ cargo install --locked --path crates/tmux-mcp
```

Load a workspace on a named tmux endpoint:

```console
$ tmux-workspace load dev.yaml -d -L dev
```

Configure your MCP client to launch the server on that endpoint:

```console
$ LIBTMUX_TOOLSETS=inspect tmux-mcp -L dev
```

For a socket path, pass the same `-S PATH` to both commands. The MCP server
also accepts `LIBTMUX_SOCKET` for a name or `LIBTMUX_SOCKET_PATH` for a path.
Select the tmux executable through `PATH` for both processes.

Discover tools with `tools/list` and read `tmux://capabilities` to confirm the
resolved socket and enabled tools. `list_sessions` and `list_windows` return
native IDs; `list_panes` supplies pane IDs for subsequent calls.
`capture_pane` accepts `{"pane":"%1"}`; `snapshot_pane` adds optional
`max_lines`. Use an ID returned by discovery. `wait_for_text` accepts `pane`,
`patterns` and `seconds`; inspection and ping remain responsive during a wait.
Close the client connection to release MCP resources; the loaded workspace
remains running. See the [MCP guide][workspace-mcp-guide] for tool contracts,
cancellation and connection settings.

[workspace-mcp-guide]: https://github.com/libtmux/libtmux-rs/blob/d746b9a5506bb3a9f42cb1401f4568a5d08e77b7/crates/tmux-mcp/README.md

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
