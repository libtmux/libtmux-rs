# Changelog

Notable changes to `libtmux`. The format follows [Keep a
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

- **Behaviour change.** `Server::new` and `Server::builder` now pass tmux
  neither `-L` nor `-S` when no socket is named, where they passed the `TMUX`
  socket as `-S`, else `-L default`. They read `LIBTMUX_SOCKET_PATH` (as `-S`),
  then `LIBTMUX_SOCKET` (as `-L`), and with neither set tmux picks the server:
  the one `TMUX` names inside tmux, else its default socket. A variable that is
  empty or only whitespace counts as unset, and `LIBTMUX_SOCKET_PATH` wins when
  both are set. A socket named in code leaves both unread, and
  `Server::from_env` still names the `TMUX` socket as `-S`. A value that
  cannot name a socket, such as `LIBTMUX_SOCKET=a/b`, now makes `Server::new`
  and `ServerBuilder::build` return `Error::InvalidServerConfiguration`. To pin
  a server, name its socket in code or set one of the variables. (#119)
- JSON layouts need tmux 3.8 or newer, the first release that accepts
  them. On an older server, applying one fails before it reaches tmux with
  `Error::UnsupportedCapability`, naming 3.8; upgrade to tmux 3.8 to apply
  a JSON layout. (#110)

## 0.1.0 - 2026-10-25

No entries for `libtmux`.

## 0.1.0-alpha.17 - 2026-10-24

### Changed

- Named-buffer deletion, rename and managed paste staging require tmux 3.4.
  Command chains refuse `break-pane` on exactly 3.7, `split-window -p` on 3.4
  and a layout operand tmux 3.3a cannot parse before any command runs; typed
  percentage splits keep using `-l N%`. (#72)
- Captured `run-shell` output requires tmux 3.5, and a literal
  `capture-pane -T` or `-F` requires 3.4 or 3.7. A daemon below the floor
  refuses the call before dispatch. (#72)
- A session or window name holding `:` or `.` is refused on exactly tmux 3.7,
  which fails the command after it has partly run. 3.7a and every other
  release accept or refuse it cleanly. (#72)
- A command chain that sends `refresh-client -l` is refused below tmux 3.6a:
  on 3.3a, asking an attached terminal for its clipboard stops the server,
  and on 3.4 through 3.6 the server stops when that client leaves before its
  terminal answers. `since::CLIPBOARD_QUERY` names the release, and the
  registry lists it as `clipboard-query`. (#72, #94)

### Fixed

- `Pane::wait_for_text` and the other pane waits answer `PaneWait::Dead` when
  tmux removes the pane or the server exits mid-wait, where they returned two
  different errors. The answer carries no output. (#72)
- Control acquisition no longer runs a user's `after-display-message` hook
  while it reads the daemon's identity. (#72)
- A file at the socket path that is not a socket reads as an absent server on
  macOS, as it already did elsewhere. (#72)

## 0.1.0-alpha.16 - 2026-10-18

### Added

- `Server::daemon_version` returns the version of the daemon a handle reaches
  once it has passed the minimum check, so a caller needs no probe of its own.
  `tmux-workspace` uses it for the terminal handoff. (#70)
- `Error::ServerReplaced` reports a daemon replaced between its version check
  and the request. Nothing ran, and repeating the call checks the new server.
  `Error::Timeout` gains `before_dispatch`, true when the deadline passed
  during the version check, before the request was sent. (#70)

### Changed

- **Breaking.** A version report that is well framed but names no release,
  such as `master`, `openbsd-*` or `3.3a-dev`, raises
  `Error::UnrecognizedTmuxVersion` instead of the minimum-version error, for a
  running server and for `tmux -V` alike. Output that is not a version report
  stays `Error::InvalidVersionOutput`, whose `output_len` counts the bytes the
  check read: stderr for the server probe, stdout for `tmux -V`. (#70)
- JSON layouts require tmux 3.8-rc2 or newer, the earliest build that emits
  them; the `3.8-rc` tag no longer exists upstream. (#70)

### Removed

- **Breaking.** Remove `since::SERVER_ACCESS`, `since::PROMPT_HISTORY` and
  `since::PANE_BORDER_FORMAT_PER_PANE`. Every supported tmux has these
  features, so drop the checks that used them. (#70)

### Fixed

- Reading the server's version no longer runs a user's
  `after-display-message` hook, and `Connection refused` on the socket reads
  as an absent server rather than an error. (#70)
- `next-X.Y` development builds rank below `X.Y-rc` and `X.Y`, so they no
  longer pass gates for the release they precede. A `next-X.Y` build can sit
  on either side of a change in tmux's command-output encoding, which its
  version cannot tell: `next-3.4` is read as the raw dialect and `next-3.6` as
  the visual one. (#70)

## 0.1.0-alpha.15 - 2026-09-27

No entries for `libtmux`.

## 0.1.0-alpha.14 - 2026-09-27

### Added

- `libtmux::test::install_executable` writes an executable script and returns
  once it can run, for a wrapper passed to
  `TestServerBuilder::tmux_executable`. A script written by hand failed
  intermittently with `ETXTBSY` while other tests forked. (#33)

### Fixed

- `cargo doc` for a crate depending on `libtmux` with default features no
  longer warns about links to items behind the `blocking`, `control-mode`,
  `plan` and `schema` features. (#33)

## 0.1.0-alpha.13 - 2026-09-26

### Added

- `plan::SplitTarget` and `SplitWindow::from_pane` let a plan split the pane an
  earlier step created, not only the window it lives in. `SplitWindow::new` is
  unchanged, and a plan recorded before this addition still decodes. (#27)
- `Server::validate_layouts` checks classic layouts for checksums, nonempty
  trees and enough pane cells before mutations. Names accept unique
  abbreviations and use the daemon's version. JSON layouts require tmux 3.8-rc
  or later; tmux validates their contents. Workspace builds, plans and
  window layout selection share the guard. (#27)

### Removed

- **Breaking.** `ServerBuilder::colors` no longer accepts `88`, so
  `Server::builder().colors(88).build()` returns
  `ServerConfigurationErrorKind::InvalidColorMode` rather than starting tmux
  with `-8`. No supported tmux release accepts that option. Pass `256` for
  tmux's `-2`, or omit the override. (#27)

### Fixed

- `Window::select_layout` checks a classic layout string's checksum and
  requires its parsed tree to be nonempty before dispatch, rather than
  forwarding a string that merely looks like one straight to tmux. tmux 3.3
  and 3.3a exit their whole daemon on a layout string they cannot parse. (#27)
- `libtmux::Error::CommandFailed`'s message states a rejected command's exit
  status plainly (`exit status 1`), not `Option`'s `Debug` (`exit Some(1)`).
  (#27)

## 0.1.0-alpha.12 - 2026-09-19

### Fixed

- Creation commands and plans, shell commands, messages, key bindings and
  access rules treat caller text beginning with `-` as a value instead of a
  tmux flag. (#28)
- `PaneOutput` ends when its pane or window closes. (#28)
- `Pane::stream_output` preserves frame-limit errors while opening its
  output stream. (#28)
- `ControlSender::resume_pane` and `unmute_pane` restore output after
  muting. (#28)
- `ControlSender::watch_only` leaves panes in other sessions unchanged. (#28)
- `Server::lock_channel` and `wait_for_channel` preserve locks and signals
  when a waiting call is cancelled or times out. (#28)
- `Pane::capture_with` trims unwritten padding from joined lines on tmux
  3.2a, so equality checks see the text the pane printed. (#28)
- `Server::environment_all` and `Session::environment_all` accept variable
  names and values containing shell punctuation or newlines. (#28)
- Replacing `set_hooks` no longer leaves a hook cleared but unwritten when
  the call is cancelled. (#28)
- Object listings decode the additional format escapes emitted by newer
  tmux builds. (#28)

### Added

- `Server::over_control_mode` routes typed operations through an existing
  control connection; blocking operations require the original server. (#28)
- `Server::owns_control_client` and `Client::is_own` identify this process's
  control clients. MCP attachment reports exclude those clients. (#28)
- `plan::Pause` delays later operations in a plan. Control-mode plans
  refuse pauses before running any operations. (#28)
- `Server::typed_key_bindings` reads key bindings as structured values on
  tmux 3.7 and newer. (#28)
- `Pane::get`, `Window::get`, `Session::get` and `Client::get` read fields
  already fetched by a listing, including fields without named getters. (#28)
- `set_typed_option` validates option values before writing them. (#28)
- `Server::load_buffer` and `save_buffer` transfer buffer contents through
  files, including buffers too large for command arguments. (#28)
- `Server::with_channel_lock` releases its lock when the operation ends,
  including failure or cancellation. (#28)
- `NewSessionOptions::environment` sets environment variables for a
  session's first process. (#28)
- `Pane::wait_until` waits for a predicate over captured lines and reports
  arrival, pane death or timeout. (#28)
- `TmuxVersion::has_behavior` checks capabilities on development builds
  using the same rules as `Server::require`. (#28)
- `Pane::stream_output_with_limits` lets callers bound streamed output.
  (#28)

### Changed

- **Breaking.** `Session`, `Window`, `Client` and `ServerGeneration`
  timestamp accessors return `SystemTime`; convert through `UNIX_EPOCH`
  when Unix seconds are needed. (#28)
- **Breaking.** Buffer names use `TmuxText` to preserve arbitrary bytes;
  pass listed names directly to buffer reads and deletion. (#28)
- **Breaking.** Pane and window respawning use `Respawn::Replacing` or
  `Respawn::OnlyIfDead` in place of a boolean. (#28)
- **Breaking.** `Server::display_menu` accepts `MenuItem` values in place
  of tuples. (#28)
- **Breaking.** ID field handles carry their ID type; update explicit
  `TextField` annotations as shown in the migration guide. (#28)
- **Breaking.** `ControlEvents` reports stream failures during iteration.
  Handle each event's error before reading the event. (#28)
- **Breaking.** `with_session`, `with_window` and `with_pane` report
  `ScopeError`, preserving both operation and cleanup failures. (#28)
- **Breaking.** Query iterators support owned values through
  `matching_owned`; update explicit trait bounds using the migration guide.
  (#28)
- **Breaking.** `AccessRule::name` replaces `user`, and `principal`
  distinguishes users from groups. (#28)
- **Breaking.** `Pane::pid` can be absent for a dead pane; use `is_dead`
  to check liveness. (#28)

### Removed

- **Breaking.** The `*_or_empty` listing helpers are removed. Handle
  listing errors or apply `unwrap_or_default` at the call site. (#28)
- **Breaking.** `get_option` and its global variants are removed; use
  `typed_option` and the corresponding typed global readers. (#28)

### Security

- Names, titles and start directories are literal text by default. Drop
  manual format escaping; use `TmuxArg::format` for intentional templates.
  (#28)
- `test::TestServer` limits inherited environment variables so tests do not
  expose unrelated exported values through tmux. (#28)

## 0.1.0-alpha.11 - 2026-09-12

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

## 0.1.0-alpha.10 - 2026-09-07

### Added

- `Pane::is_input_disabled`, `Pane::is_synchronized`, and `Pane::window_index`
  report whether a pane refuses input, whether its keystrokes reach the whole
  synchronized group, and which window index holds it.
- `Server::resolved_tmux_executable` answers the path the configured
  executable resolves to on the captured `PATH`, and `None` when it resolves
  to nothing. `None` is how a caller tells "tmux is missing" from "tmux
  failed".

### Fixed

- A caller-supplied value beginning with a dash is no longer read as a tmux
  flag. Option terminators now cover pane input, buffers, renames, layouts and
  channels, and the four builders that construct their own command and so
  inherited nothing from that guard: `pipe-pane`, `respawn-pane`,
  `respawn-window` and `source-file`. A `pipe-pane` command starting with a
  dash could not be set at all.

## 0.1.0-alpha.9 - 2026-08-31

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
- `Window` display output includes enough identity to distinguish linked
  placements. (#16)

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
- Async callers can use blocking tmux operations without deadlocking the
  current runtime. (#16)
- Server queries handle an empty tmux server without manufacturing a session
  or indexing an empty result. (#16)
- Unicode session and window names are validated without rejecting valid
  non-ASCII text. (#16)
- Filters preserve stable ordering for null relations, signed zero, and
  schema-derived comparisons. (#16)
- Suspended clients and panes with `exit_mode` report their actual terminal
  state. (#16)

## 0.1.0-alpha.8 - 2026-08-22

### Fixed

- `ControlSender::mute_pane` no longer kills the tmux server on releases
  before 3.7. `refresh-client -A <pane>:off` leaves the output already queued
  for that pane pointing into a buffer tmux then drains, and writing it
  segfaults the server; measured on 3.2a, 3.4, 3.5a and 3.6b. Below
  `since::CONTROL_PANE_OFF` the pane is paused instead, which discards the
  queue. A muted pane therefore reports `Event::Paused` on those releases, and
  tmux keeps draining its terminal rather than letting the write block.

### Added

- `libtmux::test::TestServer::daemon_state` and `DaemonState`, reporting
  whether a fixture's tmux daemon is still running and, when it is not, the
  status the kernel gave. tmux's client reports a daemon that died as
  `server exited unexpectedly` and exit 1, which is the same shape as a
  command tmux rejected, so a test asserting on the reply alone blames the
  command.
- `libtmux::since::CONTROL_PANE_OFF`, the release that takes a pane out of a
  control client's stream without losing the server.

## 0.1.0-alpha.7 - 2026-08-22

No entries for `libtmux`.

## 0.1.0-alpha.6 - 2026-08-16

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
- `Window::last_activity`. tmux stamps it on every byte a pane writes, unlike
  `has_activity`, which needs `monitor-activity` and is off by default.

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

## 0.1.0-alpha.5 - 2026-08-16

### Added

- `OutputLimits`, `DispatchLimits`, and `ControlLimits`, with
  `ServerBuilder::output_limits`, `dispatch_limits`, and
  `ControlMode::attach_with_limits`. Reading tmux output was unbounded.
- `ServerGeneration`, `Server::generation`, `Server::require_generation`. A
  socket path does not identify a daemon across a restart.
- `Client::attached_session`, `attached_window`, `attached_pane`.
- `Error::ControlModeFrameTooLarge`, `OutputLimitExceeded`, `Overloaded`,
  `ServerGenerationChanged`, `UnreadableFormatValue`.

### Fixed

- `ServerGeneration`, `PaneDirection`, and `CommandSummary` rendered with a
  neighbouring type's summary.

## 0.1.0-alpha.4 - 2026-08-15

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
- Option failures are classified by the same path as every other failure. They
  previously built `Error::CommandFailed` directly and so bypassed it.

## 0.1.0-alpha.3 - 2026-08-14

No entries for `libtmux`.

## 0.1.0-alpha.2 - 2026-08-14

No entries for `libtmux`.

## 0.1.0-alpha.1 - 2026-08-13

No entries for `libtmux`.
