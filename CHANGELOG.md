# Changelog

Notable changes to the workspace. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crates follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Crates are versioned together except `tmux-mcp`, which moves at its own pace
because its dependencies need a newer compiler than the libraries do.

While the version is `0.1.0-alpha.*`, any release may break the API; an entry
marked **Breaking.** is one a caller has to act on. The prerelease suffix is
load-bearing rather than decorative, because Cargo will not resolve a
prerelease unless the requirement names one: a plain `0.1` requirement selects
nothing, so a caller opts in by writing the version out in full.

## Unreleased

### Added

- `plan::Pause`, a wait between a plan's operations, rendered as
  `run-shell -d SECONDS` with no command. A control-mode connection
  cannot carry one, so a plan holding a pause over
  `Server::over_control_mode` is refused before sending anything.
  (#28)

- `Server::typed_key_bindings` reads `list-keys -F` into `KeyBinding`
  values (table, key, command, note, repeat); `key_bindings` still
  gives only tmux's bare `bind-key` lines. Needs tmux 3.7. (#28)

- `Pane::get`, `Window::get`, `Session::get` and `Client::get` read
  any field a listing fetched -- `cursor_x` and `pane_mode` among
  those with no getter of their own -- without a second
  `display-message`. `Availability` says why instead: `Unsupported`,
  `Unproven` (a development build), or `Absent` (as `scroll_position`
  is outside copy mode). (#28)

- `set_typed_option` on `Server`, `Session`, `Window` and `Pane` (and
  two global variants) checks a value against tmux's option table,
  refusing a bad variant, choice or range; `set_option` still takes
  values the table doesn't list. `OptionValue` converts from `bool`,
  an integer, `&str`, `String` and `TmuxText`. (#28)

- `Server::owns_control_client` and `Client::is_own` tell this
  process's own control connections from a human's terminal in one
  call, instead of the pid arithmetic that used to take at the call
  site. (#28)

- `Server::over_control_mode` returns a handle whose typed calls
  travel down an open control connection instead of spawning `tmux`.
  A blocking command -- `wait-for`, a foreground `run-shell`, a
  channel lock or wait -- is refused up front; use the original
  server for those. (#28)

- The `tracing` feature opens one `DEBUG` span per dispatch, carrying
  `request_id`, `subcommand`, `transport` and `outcome`, where a
  dispatch used to emit unrelated events (or none, over control
  mode); no argument reaches the span. (#28)

- `Server::load_buffer`/`save_buffer` let tmux read and write a
  buffer file itself, since `set_buffer` stops at 128 KiB on Linux;
  `Server::with_channel_lock` releases the channel even when its body
  returns early, fails or panics. (#28)

- `NewSessionOptions::environment` gives a session's first process
  environment variables, as `NewWindowOptions` and `SplitOptions`
  already allow. (#28)

- `Pane::wait_until` waits on a predicate over a pane's captured
  lines and answers `Arrived`, `Dead`, or `TimedOut`, the way
  `wait_for_text` does. (#28)

- `TmuxVersion::has_behavior` is the boolean form of the rule
  `require` uses; unlike `meets`, it reads a `next-X.Y` build as the
  real release. (#28)

- `Pane::stream_output_with_limits` bounds memory on a pane with
  unbounded output, the tradeoff `ControlMode::attach_with_limits`
  already exposes. (#28)

- Smaller accessors: `Pane::session` mirrors `Window::session`;
  `Pane::left`/`Pane::top` give a pane's position in cells;
  `control::PaneOutput::sender` exposes the connection a
  `Pane::stream_output` stream reads; `TmuxText` names pass directly
  to name lookups. (#28)

- `tmux-mcp`'s `ToolError`, re-exported at the crate root, is what
  `TmuxTools`'s methods now return for a refusal, in place of
  `rmcp::model::ErrorData`. (#28)

### Changed

- **Breaking.** `Session::created`, `Session::last_attached`,
  `Window::last_activity`, `Client::created` and
  `ServerGeneration::start_time` return `SystemTime` instead of Unix
  seconds; field handles still read `i64` for query filtering. (#28)

- **Breaking.** `Server::buffer_names` returns `Vec<TmuxText>` instead
  of `Vec<String>`, since tmux before 3.7 lets a buffer name hold any
  bytes. `Server::buffer`/`delete_buffer` take `impl AsRef<[u8]>`.
  (#28)

- `Pane::respawn`/`Window::respawn` take `Respawn::{Replacing,
  OnlyIfDead}` instead of a bare `bool`, and `Server::display_menu`
  takes `MenuItem` values instead of `(String, String, String)`
  triples -- neither said what the values meant. (#28)

- `query::TextField` takes a second type parameter -- the type a
  snapshot reads the field as -- defaulting to `TmuxText`;
  `PaneFields::pane_id` and its siblings now return the typed ID.
  Naming the type as `TextField<Target>` needs the new parameter.
  (#28)

- `tmux-mcp`'s `set_mouse_enabled`, `set_history_limit` and
  `set_synchronize_panes` write through the typed setters, so a
  history limit above tmux's maximum is refused before tmux sees it.
  (#28)

- **Breaking.** `tmux-mcp`'s per-tool `_meta` capability entry no
  longer repeats the tool's own `name`, `title`, `description`,
  `annotations`, and schemas, roughly halving the `tools/list`
  response; read those from the tool or from `tmux://capabilities`.
  (#28)

- **Breaking.** `tmux-workspace`'s `ConfigError::Yaml` and
  `::Invalid` now carry `line` and `column`, where the former printed
  only "not valid YAML". Match the new fields. (#28)

- **Breaking.** `tmux-workspace` keeps a file's commands out of shell
  history unless it sets `suppress_history: false`, as tmuxp does; the
  key used to default to recording every command. (#28)

- **Breaking.** `tmux-workspace`'s `PaneConfig::shell_commands` holds
  `ShellCommand` values instead of `String`s, so tmuxp's per-command
  form (`enter`, `sleep_before`, `sleep_after`) reads instead of
  failing. Sleeps are honored as a `plan::Pause`, holding for later
  commands the way `enter` does. (#28)

- **Breaking.** `tmux-mcp`'s `wait_for_text` returns `present_at_entry`
  for a pattern already on a completed row, `pending` for one on the
  row still being typed, and `matched` for one that arrives after the
  wait attaches -- replacing a screen-snapshot check that could not
  tell those apart. (#28)

- **Breaking, runtime-only.** `ControlEvents`/`next_event` report a
  connection failure during iteration instead of ending silently (use
  `event?`); EOF without `%exit` is now one such failure -- `Closed`
  -- where `shutdown` on `ControlEvents`, `ControlMode` and
  `PaneOutput` used to return `Ok(())`. No signature changes. (#28)

- **Breaking.** `with_session`/`with_window`/`with_pane` report
  failures via `ScopeError<T, E>` (`#[non_exhaustive]`), retaining
  both errors when operation and cleanup fail; recover via
  `into_operation`/`into_value`/`tmux_error`. (#28)

- **Breaking.** `QueryIteratorExt` selects owned values without
  cloning through `matching_owned`; borrowed `matching` calls are
  unchanged. (#28)

- **Breaking.** `AccessRule::user` is renamed `AccessRule::name` and
  gains `AccessRule::principal`, since group ACLs mark every row `U`
  or `G`; an undecodable row now returns
  `Error::UnreadableAccessRule` instead of vanishing silently. (#28)

- **Breaking.** `Pane::pid` returns `Option<u32>` instead of `u32`,
  and a pane listing no longer fails outright on tmux 3.8+ once a
  `remain-on-exit` pane's process exits; check `Pane::is_dead` rather
  than inferring liveness from `pid`. (#28)

- **Breaking, runtime-only.** `Window::select_layout` refuses, before
  dispatch, a value that is not a preset name, a classic layout, or
  JSON -- tmux 3.3/3.3a exit on one they can't parse, killing every
  session on the socket. `tmux-mcp`'s `select_layout`,
  `plan::ops::SelectLayout`, and `tmux-workspace`'s `layout:` route
  through the same check; a unique prefix (`tile`, `even-h`) is
  accepted, an ambiguous one refused with the new
  `Error::AmbiguousLayout`. Only tmux 3.8's JSON layout restores each
  pane's exact cell, and `ControlMode::attach` now asks for it, so a
  layout read over a control connection agrees with a snapshot's. (#28)

- `ControlSender::unmute_pane` and `resume_pane` both send `on` and
  `continue`, so either fully recovers a pane `mute_pane` muted;
  `resume_pane` alone used to leave a pane muted on tmux 3.7+. (#28)

### Fixed

- `Server::environment_all` and `Session::environment_all` decode a
  variable whose name begins `unset ` and holds `;` and a newline
  instead of failing the whole listing, and cost one tmux command
  instead of one per variable. (#28)

- A replacing `set_hooks` clears and writes in one tmux invocation, so
  a call dropped between the two no longer leaves a hook cleared and
  unwritten. A merging `set_hooks` still sends its first entry alone.
  (#28)

- `tmux-mcp`'s `capture_since`, `wait_for_text` and `run_shell_command`
  no longer go blind after an unterminated escape string -- the
  filter now closes a string the way tmux does -- and their
  descriptions now say the returned text is that escaped stream, not
  the rendered screen; see `capture_pane` for the screen. (#28)

- A missing tmux reports as `Error::ExecutableNotFound` from
  `ControlMode::attach` too, instead of the retryable `Unreachable`
  every other call uses. (#28)

- `tmux-mcp` derives every tool's MCP annotations from its capability
  row instead of a generic default, and describes every input
  property with its own default -- `respawn_pane`'s `kill_first` and
  a `send_keys_batch` row's `enter` used to be required with nothing
  said. `set_history_limit` is now `destructiveHint: true` and moves
  from `manage` to `teardown`; each description also ends before the
  generated safety sentence runs into it. (#28)

- `tmux-mcp` gives an error raised before a tool runs -- a schema
  failure, an unknown tool -- the `kind`, `retryable` and `stale`
  fields every other error carries, reserving JSON-RPC errors for
  those; a tool's own refusal is now `isError` tool content instead.
  (#28)

- `tmux-mcp` treats an empty `TMUX`/`TMUX_PANE` as absent -- so
  `TMUX= TMUX_PANE= tmux-mcp` runs detached instead of refusing every
  `send_keys`, `paste_text` and `run_shell_command` -- and accepts a
  session's `$` id wherever a tool names a session, where only names
  matched before and an id from `list_sessions` came back
  `object_gone`. (#28)

- `tmux-mcp`'s `send_keys` with only `C-c` or `C-\` now reaches a pane
  an active `run_shell_command` reserves, so a stuck command can be
  interrupted instead of leaving the pane reserved forever, and the
  shared `libtmux-mcp` daemon no longer stops while another `tmux-mcp`
  still uses it. (#28)

- `tmux-workspace` runs a pane written as a bare command (`- vim`),
  now pressing Enter, and reads blank panes -- an empty `-`, `- pane`
  or `- blank` -- as a pane with no command, instead of typing
  `pane`/`blank` as a literal command. (#28)

- `tmux-workspace` resolves `start_directory` as tmuxp does: `~` and
  `$NAME` expand from the loading environment, a relative window
  directory joins the session's instead of replacing it, and a
  `.`-prefixed path resolves from the inherited or file's own
  directory. `~name` is refused rather than falling back to `$HOME`.
  (#28)

- `tmux_workspace::freeze` records a pane at its shell's prompt as a
  pane with no command, instead of recording the shell itself and
  starting a shell inside a shell when rebuilt. (#28)

- Documentation gains: `NewSessionOptions::size` notes tmux 3.2a
  ignores it for a session's first window; `Server::run_shell`,
  `set_hooks`, `Plan::run` and `watch_only` say what a dropped call
  leaves behind; `TmuxVersion::meets` notes it refuses a development
  build a capability it has. (#28)

- Decoding a `#{q:}` value containing `{`, `}`, a newline or a tab no
  longer fails with an invalid-escape error; tmux 3.8-rc widened its
  escape set, and `window_layout` (JSON since 3.8) is the field most
  callers hit this through. (#28)

- Three capability checks migrate from `TmuxVersion::meets` to
  `has_behavior` and stop refusing a capability a development build
  genuinely has: a pane-scoped border option write,
  `capture_last_command`'s per-line flags, and
  `ControlMode::attach_with_limits`'s back-pressure handling. (#28)

- `control::PaneOutput::next_chunk` and its `Stream` end, instead of
  waiting forever, when the watched pane is killed or its window
  closes as an unlinked window. (#28)

- A creating command (`new-session`, `new-window`, `split-window`)
  that exits 0 without creating anything -- such as a missing socket
  directory -- now fails with `Error::NoEffect` naming tmux's own
  stderr, instead of a `PartialEffect` that hid the reason. (#28)

- **Breaking, runtime-only.** `ControlSender::watch_only` lists only
  this connection's attached session instead of every session on the
  server; on tmux 3.7+, watching one pane used to mute panes in other
  sessions for every attached client. (#28)

- `tmux-mcp`'s `list_sessions`, `get_server_info`, `create_session`,
  `get_session_info` and `rename_session` no longer report a session
  as `attached` solely because this server's own control connection is
  open on it. (#28)

- `Server::lock_channel` and `Server::wait_for_channel` no longer
  wedge or lose a signal when a queued or waiting client is killed or
  times out: tmux still grants the lock, or keeps the client in line
  for the channel's next signal, instead of leaving it held by nobody
  or spending the signal on a client that is gone.
  `Server::with_channel_lock` inherits the lock fix. (#28)

- `Pane::capture_with` and `Pane::wait_until`, joining a wrapped line
  with `CaptureOptions::join_wrapped`, no longer return it padded with
  the pane's unwritten cells on tmux 3.2a: every other supported
  release already drops them, and this crate now trims the same
  padding there too. A caller matching a joined line for equality
  rather than `wait_for_text`'s substring saw the pane's width instead
  of what ran in it. (#28)

### Removed

- **Breaking.** The eleven `*_or_empty` listing twins
  (`Server::sessions_or_empty` and ten siblings) are gone: each hid a
  failed listing behind the same empty vector as "nothing is there".
  Neither `tmux-mcp` nor `tmux-workspace` called one; use
  `.unwrap_or_default()` at the call site instead. (#28)

- **Breaking.** `get_option` on `Server`, `Session`, `Window`, `Pane`
  and the two global variants is gone -- it read the same option
  `typed_option` reads, as undecoded bytes. Use `typed_option`,
  `Server::typed_global_option`, or the new
  `Server::typed_global_window_option`. (#28)

### Security

- Text tmux expands as a format is now escaped on the way out: a
  session name, window name, pane title, option name or `-c` start
  directory holding `#(command)` used to run a shell for whoever wrote
  it. These arguments now take `TmuxArg`; common string types need no
  change. (#28)

- `test::TestServer` no longer hands its tmux the test process's
  environment -- `show-environment` could read back anything a
  developer exported. The fixture now passes only `PATH`, `HOME`,
  `USER`, `LOGNAME`, `SHELL`, the locale variables and `TMUX_TMPDIR`.
  (#28)

- **Breaking.** `tmux-mcp`'s `show_environment` no longer returns
  environment values, and `get_tmux_variables` refuses a name the
  environment holds -- both used to hand over every token in the
  starting shell. `LIBTMUX_ENVIRONMENT_VALUES` lists the names an
  operator allows through. (#28)

## 0.1.0-alpha.11 - 2026-09-12

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.11;
`tmux-mcp` is 0.1.0-alpha.12, because it was already at alpha.11.

Take this one if you build tmux plans, and especially if a plan of yours sends
text: `plan::SendKeys::text` documented literal text and did not send it
literally, so text naming a tmux key was pressed rather than typed. A plan can
now reach all four sides of a split rather than two, asking a client what it is
attached to costs one tmux command instead of two, and the MCP tells an agent
that a malformed target is bad input rather than an object that went away --
which had been advice to retry something that could never resolve.

### Fixed

- **Breaking.** `plan::SendKeys::text` sends its text literally, with
  `send-keys -l`. It rendered without `-l`, so tmux resolved the argument
  against its key table first and text naming a key was pressed rather than
  typed: `Space` pressed the space bar and `BSpace` deleted a character. `--`
  does not help, because it stops a leading dash being read as a flag and says
  nothing about key lookup. Text and named keys cannot render as one
  `send-keys`, so a plan carrying both is now refused by `Plan::validate`
  instead of sent wrong; send the keys as a second operation. `.enter()` is
  folded into the payload as a carriage return, the way `Pane::send_line`
  already submitted a line. (#26)

- `Client::attached_session`, `attached_window` and `attached_pane` cost one
  tmux command each rather than two. Each read an id and then listed the object
  that owned it; tmux fills a client's session, that session's current window
  and that window's active pane into one format tree, so the whole snapshot
  arrives with the id. The objects returned are unchanged. (#26)

- The MCP reports text that is not an id as `bad_input` rather than as an
  object that went away, so an agent is no longer told to look again for
  something that can never resolve. It resolves a pane or window through
  tmux's own `-f` predicate instead of listing every object and comparing
  rendered strings, so a padded id such as `%01` now addresses `%1`; a
  well-formed id for an object that is genuinely gone still reports
  `object_gone`. (#26)

### Added

- `plan::SplitWindow::direction` takes the same `SplitDirection` the object API
  takes, so a plan can put a new pane above or to the left of the one it
  divides rather than only below or beside it. `horizontal` still means
  `Right`. The side rides on a new defaulted `before` field, so a plan recorded
  before this decodes unchanged. (#26)

- `Server::format`, `Pane::format` and `Pane::pipe` document that tmux expands
  `display-message` and `pipe-pane` templates through `strftime` before its own
  format machinery. A `%` in one of those templates is a time conversion, not a
  literal, and `%%` is how to pass one through; the conversions a platform
  leaves undefined differ between glibc and Apple's libc. (#26)

### Changed

- The MCP names the words it accepts when it rejects a split direction, as it
  already did when rejecting a resize direction. Both lists are generated from
  the libtmux enums they stand for through an exhaustive match, so a variant
  added there fails the build rather than going unadvertised. The words a
  client may send are unchanged. (#26)

## 0.1.0-alpha.10 - 2026-09-07

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.10;
`tmux-mcp` is 0.1.0-alpha.11, because it was already at alpha.10.

Take this one if you run the MCP server, and especially if you run it on
macOS, where `run_shell_command` could not deliver its completion frame at
all and every run waited out its deadline. The tool surface is now frozen
behind a typed manifest and reported by `tmux://capabilities`, so a caller
can read what it was given instead of inferring it by trying. Pane input
fails closed when it cannot prove which pane will receive it, and the
configuration swapper is a transactional Rust binary in place of a Python
script that rewrote files where they sat.

Every breaking change here is in the MCP surface. The libraries only gain:
three pane accessors, the resolved tmux executable, and operand delimiting
so a value beginning with a dash is not read as a flag.

### Added

- `LIBTMUX_TOOLSETS`, `LIBTMUX_TOOLS`, and `LIBTMUX_EXCLUDE_TOOLS` freeze an
  exact tool surface at startup. An unknown name or an empty comma-list token
  stops startup rather than being ignored, and an exclusion wins over
  toolsets, named inclusions, and aggregate nested authority.

- `tmux://capabilities` reports the surface a client actually received: the
  effective tools, the selected socket and how it was chosen, direct process
  reach, tmux effects, output classes, schema-keyed input literalization,
  future-input amplification, nested authority, and whole-call MCP
  annotations.

- `Pane::is_input_disabled`, `Pane::is_synchronized`, and `Pane::window_index`
  report whether a pane refuses input, whether its keystrokes reach the whole
  synchronized group, and which window index holds it.

- `Server::resolved_tmux_executable` answers the path the configured
  executable resolves to on the captured `PATH`, and `None` when it resolves
  to nothing. `None` is how a caller tells "tmux is missing" from "tmux
  failed".

### Changed

- **Breaking.** The MCP surface is 45 tools in the unordered `inspect`,
  `manage`, `execute`, and `teardown` toolsets. Registration, descriptions,
  schemas, selection, and capability reporting come from one typed native
  manifest, so a tool cannot be advertised with a description that no longer
  describes it.

- **Breaking.** The binary defaults to the dedicated `libtmux-mcp` socket and
  a minimal tmux configuration. A newly created default daemon enables all
  four toolsets; an existing or explicitly configured daemon requires an
  explicit selection before `teardown` is enabled.

- **Breaking.** `LIBTMUX_TMUX_CONFIG` accepts only a nonempty absolute path,
  and rejects anything else before tmux is opened. A relative path changed
  meaning with the launch directory.

- **Breaking.** Spawn tools start only their configured pane process and
  accept no command or environment payload. A caller that sent one now has it
  refused rather than run.

- The authenticated creator stops its dedicated daemon when stdio closes, so a
  client that exits leaves no server behind.

- `run_shell_command` stays request-owned through completion, timeout,
  cancellation, or a dropped request, and exposes no background-job handle.

- `set_synchronize_panes` advertises that its input reaches every pane in the
  group, so a client can ask before amplifying a keystroke.

- Pane input fails closed when an effective configured recipient is dead, in a
  human-owned mode, or may be the inherited MCP caller.

- `paste_text` remains target-only, and `run_shell_command` requires exactly
  one recipient at both pre-dispatch checks.

- `run_shell_command` rejects ASCII terminal-control bytes in its exact
  executable or socket route before any watcher is set up.

- `call_read_tools_batch` exposes an exclusion-pruned 16-name schema and caps
  its complete JSON-RPC response line, request ID and newline included, at
  1,000,000 bytes without dropping an executed row. Truncation stays explicit,
  and a serialized request ID over 512 KiB fails before dispatch.

- `capture_since`, and a read batch holding only that operation, report
  observe-only effects. Retaining a cursor does not change tmux state.

- `search_panes` uses the linear-time regex engine with a 1 MiB matching
  budget, and pane searches and waits reject oversized pattern sets.

- The configuration swapper is `tools/mcp-swap`, a Rust binary, in place of
  `scripts/mcp_swap.py`. It authenticates every selected config and recovery
  artifact before building or writing, rejects path and inode aliases, and
  commits `use` or `revert` as one rollback-safe transaction; versioned state
  binds config and backup bytes, modes, identities, and topology, so `revert`
  refuses a file a human edited or replaced. A dry run performs the same
  read-only plan without building, launching a server, or writing.

### Removed

- **Breaking.** `enter_copy_mode` and `exit_copy_mode` are no longer MCP
  tools. Capture, snapshot, search, and cursor tools observe pane output
  without taking ownership of a human-controlled mode; `Pane::copy_mode` and
  `Pane::exit_mode` remain for an application that owns the whole interaction.

- **Breaking.** `--safety`, `LIBTMUX_SAFETY`, and `TMUX_MCP_SAFETY` no longer
  select ordered tiers. Either retired environment setting stops startup with
  a migration error rather than being ignored.

- **Breaking.** `--confirm`, `--no-confirm`, and `TMUX_MCP_CONFIRM` no longer
  configure server-side approval. Remove them; a client decides whether to ask
  from each tool's MCP annotations.

- Prompts, generic plan execution, background-job tools, server-wide tools,
  generic mutating tools, and dynamic resource templates are no longer part of
  the public MCP surface.

### Security

- Tool descriptions and the capability report separate socket-scoped object
  selection, pane-process authority, output sensitivity, direct effects, and
  conservative whole-call annotations. Neither tool filtering nor an MCP
  annotation is described as authorization or operating-system confinement,
  because neither is one.

### Fixed

- A caller-supplied value beginning with a dash is no longer read as a tmux
  flag. Option terminators now cover pane input, buffers, renames, layouts and
  channels, and the four builders that construct their own command and so
  inherited nothing from that guard: `pipe-pane`, `respawn-pane`,
  `respawn-window` and `source-file`. A `pipe-pane` command starting with a
  dash could not be set at all.

- Caller and pane-input checks read the configured socket path instead of
  asking tmux for `#{socket_path}`. tmux escapes a non-printable byte in the
  path when it stores it, and releases disagree about reporting it, so the
  answer named a file that does not exist: a pane read as busy told an agent
  to wait, `retryable: true`, for a `run_shell_command` that was never
  running, and a path carrying an ASCII terminal-control byte arrived as four
  printable characters and passed a check written to refuse it. A path that is
  not valid UTF-8 also came back with replacement characters on every release.

- Pane input and teardown treat inherited `TMUX` and `TMUX_PANE` as one
  canonical identity, resolve a same-daemon caller in its claimed session, and
  fail closed on partial, stale, or inconsistent selected-daemon context. A
  complete identity on another physical socket stays foreign.

- `run_shell_command` completes normally on tmux 3.3 through 3.4, returning
  output and exit status when the command ends instead of waiting until the
  request deadline.

- `run_shell_command` completes on macOS and the BSDs. A pane accepts about
  1024 bytes of input in one burst there, against 4096 on Linux, and discards
  the rest, so the completion frame -- 2.6 KB for `sh` and 4.7 KB for `bash`
  -- never arrived whole and every run waited out its deadline for a marker
  that could not come. Sending one line at a time fails the same way, because
  the bound is the burst rather than any one line, so the frame is staged
  where the pane reads it back instead of being typed. The typed line no
  longer carries the command either, and so no longer grows with it: one over
  4 KB was truncated on Linux too.

## 0.1.0-alpha.9 - 2026-08-31

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.9;
`tmux-mcp` is 0.1.0-alpha.10, because it was already at alpha.9.

Take this one if you drive tmux from an async caller, or if you pass it names
that came from somewhere you do not control. Blocking tmux operations no
longer deadlock the runtime they are called from, and option, hook,
environment, layout, and workspace inputs are validated and separated from
tmux flags before execution; `tmux-workspace` now rejects names and start
directories that could be read as tmux formats or options.

Three exported items changed shape. `OptionSchema::scope` is replaced by
`scopes` and `accepts`, `Error::LinkGone` carries `kind` and `target`, and the
MCP `cancel_job` tool is now `forget_job`.

### Added

- `ControlSender::reply_timeout` and `ControlMode::reply_timeout` set how long
  a command waits for its result block. Attaching still uses the server
  timeout, which bounds the opening handshake; the two were one value before,
  and forking tmux and a round trip on an open connection are not comparable
  latencies.

- `Pane::wait_for_text` and `Pane::wait_for_quiet` return bounded, typed
  outcomes while preserving wrapped and scrollback content. (#16)

- `Server::wait_for_channel` and control-mode subscriptions support
  notification-driven observation without polling. (#16)

- `CaptureOptions` can preserve trailing spaces, trim blank cells, and report
  pending escape sequences; `PaneOutput::snapshot` captures a stable view.
  (#16)

- `FilterSchema`, `filter_schema`, and `OperationKind::ALL` expose supported
  query fields and operation kinds for callers that build typed plans. (#16)

- `escape_format` escapes literal values embedded in tmux format expressions.
  (#16)

- `Window::respawn`, `Client::lock`, and `Server::lock_all` expose additional
  tmux mutations through typed handles. (#16)

- `Pane::join_into` and the window layout selection APIs support typed pane
  joins and layout cycling. (#16)

- `ControlClientLimits` configures finite admission limits for persistent
  control clients. (#16)

- `Error::AfterEffect` and `ErrorKind::PartialEffect` distinguish failures that
  occur after tmux may already have changed state. (#16)

- `test::scaled` and `LIBTMUX_TEST_TIMEOUT_SCALE` let downstream integration
  tests scale timeout budgets consistently. (#16)

### Changed

- **Breaking.** `OptionSchema::scope` is replaced by `scopes` and `accepts`;
  callers must test the operation and scope together. (#16)

- **Breaking.** `Error::LinkGone` now carries `kind` and `target`; update
  pattern matches that used its former session and index fields. (#16)

- **Breaking.** The MCP `cancel_job` tool is replaced by `forget_job`.
  Forgetting stops collection and discards retained output but does not
  interrupt the command running in its pane. (#16)

- `capture_since` returns the first retained screen when the requested cursor
  predates retained history, making truncation observable without losing the
  available snapshot. (#16)

- MCP schemas now close fixed vocabularies and reject unknown fields instead
  of silently accepting unsupported input. (#16)

- `Window` display output includes enough identity to distinguish linked
  placements. (#16)

### Security

- Option, hook, environment, layout, and workspace inputs are validated and
  separated from tmux flags before execution. (#16)

- `tmux-workspace` treats names and start directories as literal data and
  rejects values that could be interpreted as tmux formats or options. (#16)

### Fixed

- Window selection, unlinking, swapping, moving, and linking preserve exact
  window-link identity and refresh the affected handles. (#16)

- Handles from another server fail with `ServerMismatch`, while containment
  checks consult live tmux state instead of stale snapshots. (#16)

- Option plans and direct writes validate array values and the operation's
  actual option scope. (#16)

- Hook writes preserve slot zero, reject invalid hook bytes, and do not
  overwrite adjacent hook slots. (#16)

- Control connections surface unread replies and terminal failures instead of
  hanging or silently losing the final response. (#16)

- Retained MCP jobs, pane tails, and streamed output honor their byte limits,
  cancellation state, and truncation boundaries. (#16)

- Async callers can use blocking tmux operations without deadlocking the
  current runtime. (#16)

- Server queries handle an empty tmux server without manufacturing a session
  or indexing an empty result. (#16)

- Unicode session and window names are validated without rejecting valid
  non-ASCII text. (#16)

- Filters preserve stable ordering for null relations, signed zero, and
  schema-derived comparisons. (#16)

- MCP rename results report the name tmux actually stored. (#16)

- Recursive query derives retain their relation metadata. (#16)

- Suspended clients and panes with `exit_mode` report their actual terminal
  state. (#16)

## 0.1.0-alpha.8 - 2026-08-22

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.8;
`tmux-mcp` is 0.1.0-alpha.9, because it was already at alpha.8.

Take this one if you run tmux below 3.7: `ControlSender::mute_pane` could kill
the tmux server outright on every supported release but the newest.

### Fixed

- `ControlSender::mute_pane` no longer kills the tmux server on releases
  before 3.7. `refresh-client -A <pane>:off` leaves the output already queued
  for that pane pointing into a buffer tmux then drains, and writing it
  segfaults the server; measured on 3.2a, 3.4, 3.5a and 3.6b. Below
  `since::CONTROL_PANE_OFF` the pane is paused instead, which discards the
  queue. A muted pane therefore reports `Event::Paused` on those releases, and
  tmux keeps draining its terminal rather than letting the write block.

- `Error::kind` reports a replaced tmux server as `ObjectGone` rather than
  `Refused`. A daemon that restarted on the same socket reissues ids from the
  start, so every handle captured from the previous one names something that
  is not there, and looking it up again is exactly the fix `Refused` said
  would not help. `Error::is_object_gone` now answers `true` for it, and
  `tmux-mcp` reports it as `"stale": true`.

- `Error::kind` no longer reports a missing tmux server as `Refused`, whose
  documented meaning is that the arguments were wrong. tmux exits 1 for a
  command it refused and for one that found no server, and separates them
  only in stderr. A caller branching on the kind was told to fix a request
  that was never the trouble; `tmux-mcp` reported the same thing on the wire
  as `"kind": "refused"`.

### Added

- `ErrorKind::ServerGone`, `Error::ServerGone` and `ServerGoneKind`, naming
  which way a server was not there: never started, unreachable on its socket,
  lost with the command in flight, or shut down with it in flight. The wording
  is pinned against every supported tmux release. `tmux-mcp` reports it as
  `"kind": "server_gone"`, and as an internal error rather than
  `invalid_params`, because the request was not what failed.

- `libtmux::test::TestServer::daemon_state` and `DaemonState`, reporting
  whether a fixture's tmux daemon is still running and, when it is not, the
  status the kernel gave. tmux's client reports a daemon that died as
  `server exited unexpectedly` and exit 1, which is the same shape as a
  command tmux rejected, so a test asserting on the reply alone blames the
  command.
- `libtmux::since::CONTROL_PANE_OFF`, the release that takes a pane out of a
  control client's stream without losing the server.

## 0.1.0-alpha.7 - 2026-08-22

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.7;
`tmux-mcp` is 0.1.0-alpha.8, because it was already at alpha.7.

No published code changed. Each crate's install instructions name the current
version, and a test that typed into a pane before its shell could read was
fixed.

## 0.1.0-alpha.6 - 2026-08-16

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.6;
`tmux-mcp` is 0.1.0-alpha.7, because it was already at alpha.6.

### Added

- Typed control-mode events. `Event` now names every notification tmux
  publishes -- windows, sessions, clients, buffers, layout, subscriptions,
  flow control -- instead of collapsing all but three into `Event::Other`.
  `Event::invalidates_listings`, `may_have_added_a_pane`, `pane` and `window`
  reduce them to the decisions a caller actually makes.
- `ControlSender::watch_only`, `mute_pane`, `unmute_pane`, `pause_after` and
  `resume_pane`. tmux sends a control client every pane on the server; one
  neighbouring `yes` moves more than 20 MB in two seconds.
- `Event::Exit` carries the reason tmux gave, such as `too far behind`.
- `Pane::capture_lines` and `CapturedLine`, which report where a shell prompt
  and its output begin. tmux records these from OSC 133; fish emits it, bash
  and zsh do not without shell integration, so an unmarked capture is the
  common case and is an answer rather than a failure. Needs tmux 3.7.
- `libtmux::since`, naming the release each version-gated capability arrived
  in, so a caller can ask before it calls rather than learning from the error.
- `pane_unseen_changes`, closing a `missing` row in the format ledger. tmux
  3.4 and newer.

- `tmux-mcp` runs commands in the background: `start_command` returns a job id
  at once, `job_status` reports the exit status and only what is new since the
  cursor it gave you, `list_jobs` and `cancel_job` manage them. A ten-minute
  build no longer costs an agent its turn, and several can run at once.
- `tmux-mcp` gains `wait_for_idle`, for when a caller cannot name what success
  looks like -- a TUI settling, a prompt glyph no regex predicts.

- `tmux-mcp` gains ten tools that reach what nothing else could:
  `list_servers` (these tools bind one socket for life, so nothing else could
  learn another exists), `expand_format` (any tmux format, so a field with no
  tool of its own is one call away), `show_environment`, `set_environment`,
  `show_hooks`, `pipe_pane`, `select_layout`, `clear_pane`, `respawn_pane`
  and `paste_text`.
- `cargo run --example budget`, measuring what a client downloads at
  `tools/list`. That budget is why adding a tool is a decision.
- `what_changed`, reporting which windows have written since the timestamp it
  handed back, so re-orienting costs one call rather than a capture per pane.
- `run_command`, `wait_for_text` and `wait_for_idle` report progress every
  five seconds to a client that asked for it. Measured before it was built:
  Codex sends a `progressToken` on every `tools/call`, so this is consumed
  rather than merely published. A client that sends no token pays nothing.

- `--confirm` and `TMUX_MCP_CONFIRM`, which ask a person before anything
  destructive and refuse when the client cannot ask. Measured first: Codex
  declares `elicitation`, so this is a question something can answer.

- `tmux_workspace::freeze` and `Workspace::to_yaml`, which turn a session
  someone built by hand back into a file. The two directions are tested
  against each other: build, freeze, render, parse, rebuild.

- `Window::last_activity`. tmux stamps it on every byte a pane writes, unlike
  `has_activity`, which needs `monitor-activity` and is off by default.

- `capture_pane` takes `last_command`, returning only what the last command
  printed instead of the screen or the whole history. It reports `marks` --
  `present`, `absent`, or `unsupported` -- because an answer that fell back
  looks exactly like a command that printed a great deal.

### Changed

- `run_command`, `wait_for_text` and `wait_for_idle` each answer with their
  own outcome vocabulary instead of a shared one. Every wait used to advertise
  `no_shell` and every run `matched`, which are answers they cannot give;
  `tools/list` also lost 2.8 KB.

### Fixed

- `Pane::stream_output` read and discarded every other pane's output. It now
  tells tmux to send only the pane asked for, and repeats that when
  `%layout-change` says a pane may have appeared -- measured at 20 MB versus
  about 100 bytes over two seconds against a flooding neighbour, on every
  supported tmux.
- Command output whose lines begin with `%` was parsed as notifications and
  dropped from the result. `list-panes -F '#{pane_id}'` returned nothing and
  reported events that never happened. tmux queues notifications while a block
  is open, so inside one every line but its terminator is now output.
- A control-mode argument starting with `%` that is not a bare pane id is a
  tmux syntax error, not a quoting nicety. `refresh-client -A %1:off` was
  rejected by tmux and the command silently did nothing.
- `Error::control_mode_unrepresentable` wore a doc comment left behind by
  `control_mode_closed`.
- `Command::arg` and `Command::target` had swapped doc comments, and
  `control_mode_line`'s was spliced between them.
- `just doc-blocks` now also fails when a doc comment sits below a non-doc
  attribute, which is the shape a split leaves when it lands on a sentence
  boundary. It found the two above on its first run.
- Three doctests exercised tmux 3.3 capabilities without guarding on the
  version, so the whole doc suite failed on tmux 3.2a -- a lane CI runs.

## 0.1.0-alpha.5 - 2026-08-16

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.5;
`tmux-mcp` is 0.1.0-alpha.6, because it was already at alpha.5.

### Added

- `OutputLimits`, `DispatchLimits`, and `ControlLimits`, with
  `ServerBuilder::output_limits`, `dispatch_limits`, and
  `ControlMode::attach_with_limits`. Reading tmux output was unbounded.
- `ServerGeneration`, `Server::generation`, `Server::require_generation`. A
  socket path does not identify a daemon across a restart.
- `Client::attached_session`, `attached_window`, `attached_pane`.
- `Error::ControlModeFrameTooLarge`, `OutputLimitExceeded`, `Overloaded`,
  `ServerGenerationChanged`, `UnreadableFormatValue`.
- `Debug` on every public type, enforced by `missing_debug_implementations`.
- A runnable example on every crate-root type, enforced by
  `just example-coverage-check`.
- A recorded public API surface: `just api`, `just api-check`.
- `just doc-blocks`, which catches a doc comment split across two items.
- Fuzz targets for the control-mode, filter-expression, and workspace-YAML
  parsers. `just fuzz <target>`; weekly in CI.
- A test that handle equality and hashing separate two servers, and that the
  four handles are `Clone + Debug + Eq + Hash + Send + Sync`.
- `crates/libtmux/docs/format-coverage.txt`, measuring the format catalog
  against tmux's own source: 178 catalogued, 80 excluded, 15 missing.
  `just format-coverage-check` fails on drift.
- `tmux-mcp` caps its tmux-side fan-out.

### Fixed

- `ServerGeneration`, `PaneDirection`, and `CommandSummary` rendered with a
  neighbouring type's summary.
- Twelve parity rows were `planned` after shipping.
- `just api-check` used a fixed `/tmp` path, shared between checkouts.

### Removed

- The `semver` recipe. `cargo-semver-checks` skips every lint on a
  prerelease-to-prerelease step and then reports success.

## 0.1.0-alpha.4 - 2026-08-15

`libtmux` and `libtmux-macros` are 0.1.0-alpha.4; `tmux-mcp` is 0.1.0-alpha.5,
because it already occupied alpha.4. Its own behaviour is unchanged since then:
it is republished to pick up the licence and repository metadata.

### Added

- `Error::OptionRejected` carries an `OptionErrorKind` saying which way tmux
  would not take an option: an unknown name, an ambiguous one, or a value the
  option will not hold. tmux exits 1 for all three and distinguishes them only
  in stderr, where it also spells a rejected value two ways.
- `Server::set_environment`, `environment`, `environment_all`, `hide_environment`,
  and `unset_environment`, so the server's own environment is reachable rather
  than only each session's. tmux keeps the two in separate stores and merges
  them when it starts a process: the session's value wins, a server-only name
  still arrives, and a hidden one is absent rather than empty.
- `SparseValues<T>`, the sparse `BTreeMap<u32, T>` behind every array option,
  with `Server::array_option`, `set_array_option`, `append_array_option`, and
  `unset_array_option`. `IndexedHooks` is now an alias for
  `SparseValues<TmuxText>`, since a hook is an array option; the hook API is
  unchanged.
- `PaneDirection` and `Window::focus_direction`, for moving focus by where a
  pane sits rather than by its index. It returns the pane instead of an
  `Option` because tmux wraps at the edge: asking to go up from the topmost
  pane lands on the bottom one rather than reporting nothing above.
- `Error::CapabilityDefective`, for a capability the running release has and
  gets wrong. `Server::run_shell` now raises it on tmux 3.3, 3.3a, and 3.4,
  which send `run-shell` output to a pane's copy-mode buffer instead of the
  client and still exit zero. It previously returned an empty listing there,
  which a caller could not tell from a command that printed nothing.
- `ServerConfigurationErrorKind::NotInsideTmux` and `MalformedTmuxVariable`,
  separating "this process is not inside tmux", an ordinary state to branch on,
  from a `TMUX` variable that is present and does not say what tmux says.

### Changed

- **Breaking.** Listing pairs swapped names: `sessions()`, `windows()`,
  `panes()`, `clients()`, `attached_sessions()`, `linked_sessions()`,
  `search_panes()`, and `search_windows()` now return `Result`, and the
  collapsing form is `*_or_empty()`. A caller reaching for the obvious name
  got a `Vec` that could not be told apart from a healthy server with nothing
  running, which for anything that reconciles state reads an outage as an
  instruction to delete everything.
- `tmux-workspace` refuses a value that is present and the wrong shape
  instead of defaulting it, and names where it happened:
  `windows[0].panes[1].enter must be a boolean, found "maybe"`. `focus: "tru"`
  used to read as `false`, which builds a workspace that is valid and not the
  one the file describes.
- Option failures are classified by the same path as every other failure. They
  previously built `Error::CommandFailed` directly and so bypassed it.
- Licensed under `MIT OR Apache-2.0` rather than MIT alone, matching Rust
  convention. Releases up to and including `0.1.0-alpha.3` were MIT only.
- `repository` points at this workspace rather than at the Python libtmux
  repository, which does not contain this code.
- Published from CI by a `v*` tag, with no stored API token: the workflow
  exchanges a GitHub OIDC identity for a short-lived crates.io token.

## tmux-mcp 0.1.0-alpha.4 - 2026-08-14

That crate alone. Version numbers diverge from here, because `tmux-mcp`
moves at its own pace.

## 0.1.0-alpha.3 - 2026-08-14

## 0.1.0-alpha.2 - 2026-08-14

## 0.1.0-alpha.1 - 2026-08-13

First published alphas. These predate this repository, which was extracted from
the Python libtmux repository without its history, so their changes are not
itemized here.
