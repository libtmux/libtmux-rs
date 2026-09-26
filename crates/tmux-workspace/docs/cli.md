# `tmux-workspace` command reference

The [README](../README.md) shows the commands. This page is the detail behind
them: how a workspace is found, what a load does when it cannot finish, what
machine output promises, and where the command departs from tmuxp. Every
switch is in `tmux-workspace <command> --help` and in the manual page:

```console
$ tmux-workspace --generate man > ~/.local/share/man/man1/tmux-workspace.1
```

## Finding a workspace

`load`, `convert` and `edit` take a WORKSPACE, which is one of:

- a file: `./dev.yaml`, `dev.yml`, `dev.json`;
- a project directory holding `.tmuxp.yaml`, `.tmuxp.yml` or `.tmuxp.json`,
  written as a path: `.`, `./api` or `api/`;
- a bare name, `dev`, looked up only in the first of these directories that
  exists: `$TMUXP_CONFIGDIR`, `$XDG_CONFIG_HOME/tmuxp` (else
  `~/.config/tmuxp`), and `~/.tmuxp`.

A bare name is never looked for in the current directory, as in tmuxp, so a
`dev.yaml` that happens to be there cannot stand in for the configured one.
Write `./dev` to mean the local file; when only a local one exists, the error
says so.

`ls` lists the project files from the current directory up to your home
directory, then the files in that workspace directory. `--tree` groups them by
directory, and `--full` includes each configuration beneath its file. Human
labels and paths escape control characters; JSON and NDJSON keep the original
values.

`search` matches regular expressions against those same files. A term written
`FIELD:PATTERN` is limited to one field -- `name`, `session` (`s`), `path`
(`p`), `window` (`w`) or `pane` -- and terms combine with AND unless `--any`
is given.

## Loading

`load` builds each WORKSPACE in order, then attaches to the last one, or
switches to it when run inside tmux. `-d` builds without attaching and `-s`
names the last session. `-a` adds the windows to the session of the pane
`load` runs in instead of creating one, and moves the client only to a window
that sets `focus: true`; `-d` wins over `-a`.

With stdin a terminal and no `-y`, `load` asks before it switches or attaches.
Inside tmux, for a session the document would create, it asks
`Already inside tmux: switch (y), load detached (n), or append (a)?`; for a
session that already exists it asks `<name> is already running. Attach?`.
Without a terminal, under `--json` or `--ndjson`, or with `-y`, it proceeds
without asking.

Run from inside tmux, `load` checks that `TMUX` names the daemon on the target
socket, that `TMUX_PANE` is a pane there, and that a client is attached to
that pane's session, and refuses with exit status 2 before building anything
if one does not hold. Aimed at a different tmux server, it refuses the same
way and names `-d`.

A new session is sized from `TMUXP_DEFAULT_COLUMNS` and `TMUXP_DEFAULT_ROWS`
(else `COLUMNS`/`ROWS`, else 80x24); the invoking terminal overrides that
when stdout is one, and `COLUMNS`/`LINES` override the terminal.
`TMUXP_DETECT_TERMINAL_SIZE` set to anything but `1` passes no size at all.

A load that cannot finish removes what it created -- the session, the windows
it built, the bootstrap window -- and reports `error`. A load that reuses a
session missing something the document asks for reports `session_mismatch`.
An append that fails names the windows it kept.

## Documents

`tmux-workspace` reads the documents tmuxp reads, with these rules:

- A key starting with `x-`, at any level, is inert: accepted, ignored at load,
  and preserved by `convert`. Every other unknown key is refused, and the
  refusal names the `x-` prefix.
- `<<: *anchor` and `<<: [*a, *b]` merge keys resolve at every level, and
  explicit keys override merged ones.
- `global_options` applies through tmux's global session table
  (`set-option -g`). An option tmux keeps only per window, such as
  `pane-base-index`, is applied to every window when listed under the
  session's `options:`.
- A window that names no `layout` is tiled. tmuxp stacks it instead.
- With no `focus`, the last pane a window builds stays active, as in tmuxp.
- `session_name` may not contain `:` or `.`, which tmux reads as window and
  pane separators in a target.
- `config` and `socket_name` in a document are refused; use `-f`, `-L` or
  `-S`.

### Pane readiness

`workspace_builder_options` accepts only `pane_readiness`; an unknown field is
a warning and the document still loads. The default, `auto`, waits for the
shell prompt only when the session's `default-shell` is zsh, whose prompt
redraw the wait exists for. `always` or `true` waits for any shell; `never` or
`false` never waits. Commands are typed either way: waiting only delays them
until the prompt has redrawn.

## Bootstrap scripts

`before_script` runs before the session is built. It is split into an
executable and arguments with shell-style quoting and no expansion. An
executable starting with `.` resolves from the workspace file's directory;
absolute paths and commands found through `PATH` keep their meaning. It runs
in the session's `start_directory` when set, else the current directory.

A missing, non-executable or failing script fails the load with code
`script_failed`, exit status 1, and removes the session `load` created for it,
never a borrowed or appended one.

Output collects up to 1 MiB per stream. A human load gives the script the
terminal's stdin and foreground process group, and restores both on exit,
failure or cancellation. Ctrl-Z suspends the whole job; resume it with `fg`.
Machine loads close the script's stdin.

Scripted loads need a safe, non-reaping child observer. On Unix targets whose
bindings lack one -- Cygwin, NetBSD and OpenBSD among them -- scripted and
Python-extension loads fail before touching tmux; other commands work.

## Python extensions

`plugins` and `workspace_builder` run through a bridge to tmuxp: a nonempty
plugin list or builder string needs tmuxp 1.74.x, found through
`TMUX_WORKSPACE_PYTHON` or `PATH`. An absent or empty value keeps the native
builder. The bridge's request travels in an environment variable, not on the
child's command line. `shell` also runs tmuxp's Python shell.

## Imports

`import tmuxinator` and `import teamocil` convert a project into a workspace
and validate it before printing or saving. A SOURCE is a file, or a bare name
found only in `$TMUXINATOR_CONFIG` (else `~/.tmuxinator`) or `~/.teamocil`.
Without `--save-to`, a human import asks, then saves `<name>.yaml` in the
workspace directory, creating it, so `load <name>` finds it next. Under
`--json` it prints the document instead.

tmuxinator imports keep ordered windows and panes, directories, layouts and
command arrays. `pre_window` lists keep their `; ` grouping and per-window
`pre` lists their `&&` grouping. Project lifecycle hooks, endpoint settings,
named panes and early synchronization are refused, and so is unexpanded ERB:
tmuxinator expands `<%= %>` through Ruby first, and no native reader does.

teamocil imports keep the session name (else the file's stem), windows,
directories, layouts, options, pane commands and focus. Legacy filters,
`clear`, pane widths and active `synchronize-panes` are refused; `<%` is
ordinary text.

The imported root is absolute, anchored to where the import ran. Import does
not need tmux or Python.

## Machine output

`--json` writes one JSON document to stdout; `--ndjson` streams one event per
line and takes precedence. Machine output never contains ANSI escapes, and a
load under either needs `-d` unless it appends. Every record carries
`schema_version`. A failure writes
`{"schema_version":1,"code":"...","message":"..."}` to stderr, and stdout
stays empty:

| Exit | `code` | Meaning |
| --- | --- | --- |
| 0 | | Finished |
| 1 | `invalid_workspace`, `unsupported_key` | The document is malformed or asks for something refused |
| 1 | `workspace_not_found`, `session_not_found` | Nothing by that name |
| 1 | `session_mismatch`, `destination_exists` | Something already there does not match, or would be replaced |
| 1 | `tmux_failed`, `tmux_unavailable` | tmux refused, or is not running or installed |
| 1 | `script_failed` | `before_script` failed |
| 2 | `usage` | The invocation itself is wrong; nothing ran |
| 130 | `interrupted` | `SIGINT` or `SIGTERM` |

`load --ndjson` emits `session-created`, `window-created`, `pane-created`,
`window-completed`, `pane-completed`, `script-started`, `script-output` and
`script-completed`. An interrupted load reports the inputs it completed and
the session, window and pane IDs tmux acknowledged; it does not roll them
back, and after a mutation has begun `outcome_unknown: true` warns that more
may have applied.

## Logging

`load --log-file PATH` appends JSON records to a regular file, created
owner-only; symlinks and other invalid destinations are refused before tmux
runs. `--log-level info` adds load events and `debug` adds child output, up
to 1 MiB per stream. The default is `warning`, and the level never changes an
exit status.

## Generating completions and the manual

`--generate` writes `bash`, `zsh`, `fish`, `powershell`, `elvish`, `man` or
`schema` without needing tmux or Python:

```console
$ tmux-workspace --generate zsh > ~/.zfunc/_tmux-workspace
```

```console
$ tmux-workspace --generate bash > ~/.local/share/bash-completion/completions/tmux-workspace
```

```console
$ tmux-workspace --generate fish > ~/.config/fish/completions/tmux-workspace.fish
```

`schema` exports the command graph: positional arity, aliases, groups,
conflicts, overrides and numeric bounds. Under `--json` or `--ndjson` the
artifact's exact bytes arrive in `artifact.content`.

## The command and the library read documents differently

The package ships a library and the command, and they are two readers: the
command has its own parser and builder. A file that loads through one is not
guaranteed to mean the same thing through the other.

| | Library | `tmux-workspace` command |
| --- | --- | --- |
| An unknown key | recorded in `unsupported_keys`, the file still loads | refused, unless it starts with `x-` |
| No `windows`, or an empty list | keeps the window tmux made | refused |
| A pane's `environment` | merged with the window's | replaces the window's |
| `window_shell` | the window's creation command only | the default shell for every pane in the window |
| `~` and `$VAR` in shell commands | retained for the shell | expanded against the loader environment |
| YAML merge keys (`<<:`) | not resolved | resolved |
| `before_script`, `plugins`, `options_after`, `workspace_builder_options` | not modelled | modelled |
