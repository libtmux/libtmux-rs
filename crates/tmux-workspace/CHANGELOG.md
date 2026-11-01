# Changelog

Notable changes to `tmux-workspace`. The format follows [Keep a
Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Releases are numbered `0.1.N`, and each release increments `N`. The API is not
settled: any release may change or remove exported identifiers, and the
changelog marks each break. Pin an exact version to avoid one.

The crates are released together, so every release has a section here. Changes
that span crates, such as documentation, CI and release tooling, are in the
[main
changelog](https://github.com/libtmux/libtmux-rs/blob/master/CHANGELOG.md),
which a published crate cannot link by a relative path.

## Unreleased

## 0.1.1 - 2026-10-31

### Changed

- **Behaviour change.** `tmux-workspace` leaves the socket to the core after
  `-S` and `-L`: `LIBTMUX_SOCKET_PATH`, then `LIBTMUX_SOCKET`, else tmux picks.
  It no longer passes the `TMUX` socket as `-S` or falls back to `-L default`
  itself, and `--help` states the fallback beside both flags. (#119)

## 0.1.0 - 2026-10-25

No entries for `tmux-workspace`.

## 0.1.0-alpha.17 - 2026-10-24

### Changed

- `tmux-workspace` Python extensions and `shell` require
  `libtmux.common.TMUX_OPTIONAL_FEATURE_POLICY_VERSION` to be an integer of
  at least 2. Runtime discovery and builder children refuse an older backend
  before importing extension hooks. (#72)

## 0.1.0-alpha.16 - 2026-10-18

### Changed

- **Breaking.** `tmux-workspace` Python extensions and `shell` require a
  Python libtmux backend that declares tmux 3.3a or newer and checks each
  command's server. Select a compatible tmuxp 1.74.x environment through
  `TMUX_WORKSPACE_PYTHON`. (#70)
- `tmux-workspace` reports a refused tmux, old or naming no release, with the
  machine code `tmux_unsupported` instead of `tmux_failed`. (#70)

## 0.1.0-alpha.15 - 2026-09-27

No entries for `tmux-workspace`.

## 0.1.0-alpha.14 - 2026-09-27

### Added

- `tmux-workspace load --dry-run` prints the sessions, windows, panes,
  directories and commands a load would build, and names each environment
  variable it would set, without asking tmux anything or running
  `before_script`. `--json` prints the same as one document. (#33)
- `Workspace::from_yaml_strict` and `Workspace::from_file_strict` refuse a key
  that neither this parser nor tmuxp reads, unless it starts with `x-`, as the
  `tmux-workspace` command does. `from_yaml` and `from_file` still accept such
  a key and record it in `unsupported_keys`. (#33)
- **Breaking.** `PaneConfig` gains `shell`, which overrides `window_shell` for
  that pane, and `shell_command_before`, typed after the workspace's and the
  window's and before the pane's own commands, as tmuxp reads them. A struct
  literal that builds a `PaneConfig` needs `..PaneConfig::default()` to
  compile. (#33)
- `tmux-workspace` ships runnable examples: `build` parses a workspace, lists
  its plan, builds it on an isolated tmux and freezes it back to YAML, and
  `strict` reads one document leniently and strictly. (#33)

### Changed

- **Breaking.** `tmux-workspace load` leaves `$NAME` in a command or `shell`
  for the pane's shell to expand, and adds the loader's value of each named
  variable to that pane's environment. It had pasted the value into the
  command text, as tmuxp does, so a value holding `;`, `|` or `$(...)`, such
  as `LS_COLORS`, ran as a command of its own. `'$NAME'` in single quotes now
  stays literal, `$PWD` is the pane's directory, and a program other than a
  shell sees the variable unexpanded. (#33)
- **Breaking.** `WorkspaceBuilder` adds the loader's value of each `$NAME` a
  pane's commands, `window_shell` or `shell` name to that pane's environment,
  unless the document sets it, as `tmux-workspace load` does. (#33)
- **Breaking.** `tmux-workspace` looks up a bare name such as `dev`, with no
  directory and no extension, only in the workspace directory, as tmuxp does,
  for `load`, `convert`, `edit` and the importers. A `dev.yaml` or `dev/` in
  the current directory no longer answers to `dev`; write `./dev`, which the
  error suggests when only a local file exists. (#33)
- `tmux-workspace --help`, each command's help and `--generate man` describe
  every argument and end with examples. `import tmuxinator` and `import
  teamocil` describe their own source format. (#33)
- `tmux-workspace load -8` is hidden from help and shell completions. It is
  still refused with its reason, and `--generate schema` lists it as hidden.
  (#33)
- `tmux-workspace` builds its command by default: `cli` is a default feature,
  so `cargo install tmux-workspace` works where it had failed with "no
  binaries are available". A library dependent that wants none of the
  command's dependencies sets `default-features = false`. (#33)
- **Breaking.** `Workspace::from_yaml` and `from_file` refuse a document whose
  `windows` key is missing or null, as tmuxp's schema validation does. An
  explicit `windows: []` still loads and keeps the window tmux made, and
  `Workspace::to_yaml` writes an empty workspace that way. (#33)
- **Breaking.** `PaneConfig::environment` is `Option<Vec<(String, String)>>`:
  `None` inherits the window's environment, and `Some`, even empty, replaces
  it, as tmuxp's builder does, where the two had been merged. A pane that sets
  its own variables and relies on a window variable must now name that
  variable too. (#33)
- **Breaking.** `WorkspaceBuilder` applies `window_shell` to every pane it adds
  to a window, as tmuxp's `get_pane_shell` does, where it had applied only to
  the window's first pane. A pane's own `shell` overrides it. (#33)
- **Breaking.** `Workspace::from_yaml` resolves YAML merge keys, `<<: *anchor`
  and `<<: [*a, *b]`, at every level, as PyYAML's safe loader does. `<<` had
  been kept as an unsupported key. (#33)

### Fixed

- `tmux-workspace freeze` without a session name, run from a pane on one tmux
  server and aimed at another with `-L` or `-S`, captured whichever session
  owned the same pane ID there. It now uses the pane's session only when
  `TMUX` names the selected server, and otherwise asks for a name, as it does
  outside tmux. (#33)
- `tmux-workspace import` without `--save-to` offers to save into the
  workspace directory, `~/.config/tmuxp` unless configured otherwise, and
  creates it, so `load <name>` finds the result. It had offered the source's
  own directory, such as `~/.tmuxinator`, where nothing looks for a workspace.
  (#33)

## 0.1.0-alpha.13 - 2026-09-26

### Added

- `tmux-workspace` is a native binary for tmuxp workspaces: discovery, loading,
  capture, conversion and imports, with JSON and NDJSON output, terminal load
  progress, generated manuals and shell completions. (#27)
- `tmux-workspace freeze` against a socket whose server has not started reports
  `session_not_found`, the same answer a name that is not running gets, whether
  or not a name was supplied. (#27)
- `tmux-workspace freeze <name>` refuses a name containing `:` or `.` on the
  name alone, before asking tmux whether a session by it exists: `load` would
  refuse the same document regardless, and asking tmux first would answer
  `session_not_found` for a name that could never resolve as typed either way.
  (#27)
- `tmux-workspace freeze` writes only where `--save-to` names it to, and returns
  the captured document on stdout under `--json` or `--ndjson`. No destination
  is derived from the session, because tmux allows a session name to hold path
  separators, so human capture without `--save-to` is a usage error rather than
  a file somewhere the name implied. (#27)
- `tmux-workspace load`, run from inside tmux, resolves its own invoking pane
  before building anything: `TMUX` must parse and name the daemon on the target
  socket, `TMUX_PANE` must be a pane there, and a client must be attached to
  that pane's session, each a usage error at exit 2 with nothing created if it
  fails. Aimed at a tmux server other than the invoking pane's, it refuses the
  same way, naming `-d` as the way to load without attaching. An attached load
  ends in `switch-client`, which needs no terminal, so it can run from a
  `run-shell` key binding whose pane `TMUX_PANE` does not identify; outside
  tmux, or without a client, it still needs one. (#27)
- `tmux-workspace load`, with stdin a terminal and no `-y`/`--yes`, asks before
  switching or attaching: inside tmux, for a session the document would create,
  `Already inside tmux: switch (y), load detached (n), or append (a)? [y/n/a]`;
  for a session that already exists, inside or outside tmux, `<name> is already
  running. Attach? [Y/n]`. A multi-input load asks once, for the input it will
  switch to or attach. Without a terminal, under `--json`/`--ndjson`, or with
  `-y`/`--yes`, it proceeds without asking. Declining `tmux-workspace convert`'s
  `Save <path>?` prompt is a normal outcome, exit 0, that saves nothing. (#27)
- `tmux-workspace load --append` moves the client to an appended window only
  when that window sets `focus: true`; `-d` wins over `--append`, building a new
  detached session and ignoring it; and `--append` with no invoking pane is a
  usage error, exit 2. The client a load hands over to starts with the same `-2`
  and `-f` flags `load` itself used, and its human summary after appending says
  `Appended <session>`, naming the session that received the windows. (#27)
- `tmux-workspace load` sizes a session it creates from the terminal it runs in:
  `TMUXP_DEFAULT_COLUMNS`/`TMUXP_DEFAULT_ROWS` (else `COLUMNS`/`ROWS`, else
  80x24) set the starting size, the invoking terminal overrides it when stdout
  is one, and `COLUMNS`/`LINES` override the terminal;
  `TMUXP_DETECT_TERMINAL_SIZE` set to anything but `1` disables detection and
  passes no explicit size at all. (#27)
- `tmux-workspace load` refuses a `session_name` containing `:` or `.`. With no
  `start_directory` anywhere in a document, a pane starts in the invocation
  directory; `start_directory: ~` is read as absent, and one that does not exist
  is reported before anything is built. A window's panes build in the order the
  document gives them, and one in which no pane sets `focus` is left on the last
  pane built. A pane's command waits for that pane's shell whatever the shell
  is, before it is sent. (#27)
- `<<: *anchor` and `<<: [*a, *b]` merge keys resolve at every mapping level a
  workspace document uses them, with explicit keys overriding merged ones. A key
  starting with `x-`, at any level, is inert: accepted, ignored at load, and
  preserved by `convert`. An option tmux keeps only at window scope, such as
  `pane-base-index`, is applied to a document's windows when listed under the
  session's `options:`. `global_options` applies through tmux's global session
  table (`set-option -g`). An unrecognized `workspace_builder_options` field is
  a warning, and the document still loads. (#27)
- `tmux-workspace load`'s machine output reports a fixed set of conditions: a
  malformed document, an invalid value, or an input tmux's own client rejected
  before sending a command, such as an invalid layout name, is
  `invalid_workspace`; everything else tmux refused is `tmux_failed`; the
  append-identity, colour-mode, confirmation and session-selection refusals are
  `usage`; a missing workspace file or name is `workspace_not_found`; a refused
  unknown or unsupported key is `unsupported_key`; a missing tmux executable or
  unreachable server is `tmux_unavailable`; and a destination that exists
  without `--force` is `destination_exists`. A load that cannot finish removes
  everything it created -- the session, the windows it managed to build, the
  bootstrap window -- and reports `error` with nothing retained. A load that
  reuses an existing session missing something the document asks for reports
  `session_mismatch`, `status: error`, and names the first thing absent; an
  append that fails names the windows it kept. (#27)
- A missing or non-executable `before_script` is a failure exactly like a
  nonzero exit, matching tmuxp's `BeforeLoadScriptNotExists`: code
  `script_failed`, exit 1, and the session `load` created for it removed, never
  a borrowed or appended one. A `load` failure's `results[]` carries a record
  for the input that failed too, with the same minimum fields (`input`,
  `input_index`, `session_id`, `session_name`, `reused`) a completed input's
  record carries; `status` is `error` rather than `partial` when a single
  input's owned session was removed, since nothing was retained, and stays
  `partial` when an earlier input completed or a failed input's borrowed or
  appended session was left standing. `SIGINT`/`SIGTERM` retains acknowledged
  effects and completed inputs; an interrupted mutation reports an unknown
  outcome without inventing session IDs or implying rollback, and its raw
  retained-state record is the one case a human-mode failure still prints. (#27)
- `tmux-workspace load --ndjson` emits `window-completed` and `pane-completed`
  alongside `window-created` and `pane-created`; `pane-created`/`pane-completed`
  carry `session_id`, `window_id` and `pane_index`, and
  `window-created`/`window-completed` carry `session_id`. It brackets a
  `before_script`'s output with `script-started` and `script-completed`
  (`input_index`, and on completion `child_status`/`truncated`), matching go;
  `script-output` carries `input_index` alongside its
  `stream`/`text`/`encoding`. (#27)
- A workspace naming a Python plugin or builder runs it through a bridge that
  accepts any `1.74.x` tmuxp, not `1.74.0` alone; the bridge's request travels
  in an environment variable rather than on the child's command line, which is
  readable by any local user through `/proc/<pid>/cmdline`. An empty or absent
  plugin or builder selection keeps the native loader; an invalid selection is
  refused before any workspace runs scripts or changes tmux. (#27)
- `tmux-workspace freeze` refuses to capture a session whose name holds `.` or
  `:`, which `tmux-workspace load` could not read back, and writes no file. It
  omits the caller's process environment and the capturing terminal's
  `default-size` from the captured document: a session inherits its environment
  whole at creation, indistinguishably from anything the workspace itself set,
  and `default-size` is the capturing terminal's size, not anything the
  workspace asked for. An explicit `--save-to` destination is written without
  asking for confirmation; `--force` governs replacing a file already there.
  (#27)
- `tmux-workspace import tmuxinator` refuses unexpanded ERB markup (`<%= %>`) in
  a document's keys and values before conversion; tmuxinator expands ERB through
  Ruby before parsing and no native reader does, so the markup would otherwise
  reach the shell as a literal. `import teamocil` sources, which use no template
  engine, keep such text as written, and derive `session_name` from the imported
  file's stem when the document has none, the way teamocil itself names the
  session. Both importers preserve a source's command grouping, pane order,
  focus and directory context, and validate a native document before writing it.
  (#27)
- `tmux-workspace --generate` renders one manual covering every command, and
  exports each command's positional arity, groups, conflicts, overrides and
  numeric bounds; its JSON and NDJSON output wrap the exact generated text, and
  human help and shell completions keep their own format. `--generate bash`
  reaches every subcommand's own options, including `tmux-workspace`'s own
  hyphenated name. `tmux-workspace ls --tree` groups discovered files by
  directory and nests `--full` configuration output; human labels escape control
  characters, and machine records keep their original values. (#27)
- Installing the `tmux-workspace` binary needs `cargo install tmux-workspace
  --features cli` (or `--all-features`); a plain library dependant pulls none of
  clap, tokio's process driver, or the other CLI-only dependencies. (#27)

### Fixed

- `WorkspaceBuilder` rebalances a window between splits, so a window of five or
  more panes builds at a default terminal size instead of failing with "no space
  for a new pane". (#27)
- `WorkspaceBuilder::plan()` places panes in the order a workspace gives them.
  Every split after the first targeted the window, which always resolves to
  whichever pane a detached split leaves active, so each new pane landed after
  the same one instead of after the last: a 4-pane window came out A, D, C, B.
  (#27)
- **Breaking.** `Workspace::from_yaml` refuses a `session_name` containing `:`
  or `.`. Those characters are the window and pane separators in a `-t`
  target: some tmux releases silently rewrite one out of the name at session
  creation, one refuses the name outright, and releases that keep it verbatim
  still misread an ordinary `-t name` target as a window or pane rather than
  the session -- reaching it afterward needs a `name:` terminator, which
  nothing that types a plain target uses. (#27)
- `Workspace::to_yaml()` always writes a pane's `shell_command` as a YAML
  list, including a single command. (#27)

## 0.1.0-alpha.12 - 2026-09-19

### Fixed

- `tmux-workspace` executes tmuxp's command shorthand and creates empty
  panes from its blank-pane forms. (#28)
- `tmux-workspace` resolves inherited and relative start directories and
  expands environment variables as tmuxp does. (#28)
- `tmux_workspace::freeze` records an idle shell as an empty pane, so
  rebuilding it does not start a nested shell. (#28)

### Changed

- **Breaking.** `tmux-workspace` configuration errors include line and
  column fields; update matches on `ConfigError`. (#28)
- **Breaking.** `tmux-workspace` suppresses shell history by default; set
  `suppress_history: false` to record commands. (#28)
- **Breaking.** `PaneConfig::shell_commands` uses `ShellCommand` values to
  support tmuxp's per-command Enter and delay settings. (#28)

## 0.1.0-alpha.11 - 2026-09-12

No entries for `tmux-workspace`.

## 0.1.0-alpha.10 - 2026-09-07

No entries for `tmux-workspace`.

## 0.1.0-alpha.9 - 2026-08-31

### Security

- `tmux-workspace` treats names and start directories as literal data and
  rejects values that could be interpreted as tmux formats or options. (#16)

## 0.1.0-alpha.8 - 2026-08-22

No entries for `tmux-workspace`.

## 0.1.0-alpha.7 - 2026-08-22

No entries for `tmux-workspace`.

## 0.1.0-alpha.6 - 2026-08-16

### Added

- `tmux_workspace::freeze` and `Workspace::to_yaml`, which turn a session
  someone built by hand back into a file. The two directions are tested
  against each other: build, freeze, render, parse, rebuild.

## 0.1.0-alpha.5 - 2026-08-16

No entries for `tmux-workspace`.

## 0.1.0-alpha.4 - 2026-08-15

### Changed

- `tmux-workspace` refuses a value that is present and the wrong shape
  instead of defaulting it, and names where it happened:
  `windows[0].panes[1].enter must be a boolean, found "maybe"`. `focus: "tru"`
  used to read as `false`, which builds a workspace that is valid and not the
  one the file describes.

## 0.1.0-alpha.3 - 2026-08-14

No entries for `tmux-workspace`.

## 0.1.0-alpha.2 - 2026-08-14

No entries for `tmux-workspace`.

## 0.1.0-alpha.1 - 2026-08-13

No entries for `tmux-workspace`.
