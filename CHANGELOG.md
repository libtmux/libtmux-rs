# Changelog

Notable changes to the workspace. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crates follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Crates are versioned together except `tmux-mcp`, which moves at its own pace
because its dependencies need a newer compiler than the libraries do.

While the version is `0.1.0-alpha.*`, any release may break the API, and
breaking changes are not called out as such: nothing here is stable yet. The
prerelease suffix is load-bearing rather than decorative, because Cargo will
not resolve a prerelease unless the requirement names one: a plain `0.1`
requirement selects nothing, so a caller opts in by writing the version out in
full.

## Unreleased

### Fixed

- Document that tmux may not store the session name it was given. Releases
  through 3.6b rewrite `:` and `.` to `_` silently, 3.7 refuses such a name,
  and 3.7a keeps it, so `new_session("a:b")` hands back a session called `a_b`
  on most supported releases. `Session::name` always reports what tmux stored;
  it is the request that can differ from it.

- A control-mode reply no longer waits on the caller draining events. The
  connection stopped reading when nobody took its events, and a reply arrives
  on the connection that stopped, so a caller awaiting one deadlocked: no
  error, no timeout, and `ControlSender::is_closed` reporting false. It needed
  no pane output to happen — a caller that only sends fills the queue with the
  notifications its own commands raise. Measured identically on tmux 3.2a
  through 3.7c. The connection now holds what the caller has not taken and
  keeps reading while a reply is outstanding, pausing only when none is, so
  events are still never dropped and never reordered.

- The `control` module documentation no longer shows a loop that sends a
  command from inside its own event loop, and no longer says that doing so
  works. It does not: a reply arrives on the connection the events arrive on,
  so a loop that stops reading to await one is waiting on the connection it
  stopped reading. The example now watches from its own task, which is the
  shape the crate's own concurrency test uses.

- A `watch` example shows control mode doing what it is for: one connection
  carrying commands down and the server's own reports back, with the watcher
  in its own task so neither direction waits for the other. The crate's async
  story had no runnable example of its own.

- `Window::respawn`, `Client::lock`, and `Server::lock_all` reach the levels
  tmux offers that the crate did not. `Pane::respawn` restarted one pane and
  nothing restarted a window, and `Session::lock` locked one session while
  neither a single client nor the whole server could be locked at all.

- `Pane::join_into` moves a pane into another window, beside a pane already
  there. `Pane::break_out` took a pane out into a window of its own and nothing
  put one back, so a pane could leave and not return without dropping to
  `Server::cmd`. Placement is a `JoinOptions` carrying a direction, an optional
  size, and whether to span the window; it carries no command or directory,
  because tmux spawns nothing here.

- `Window::select_layout` takes a `Layout` naming one of the seven
  arrangements tmux knows, so a layout that does not exist is a compile error
  rather than a refusal at the far end of a round trip. It still takes a saved
  layout string, now as `LayoutSpec::Saved`, and a `&TmuxText` converts into
  one so a layout read from `Window::layout` goes straight back.
  `Layout::MainHorizontalMirrored` and `MainVerticalMirrored` need tmux 3.5 and
  report `since::MIRRORED_LAYOUTS` below it.

- `Window::next_layout` and `Window::previous_layout` step through tmux's
  layout list. tmux takes those as flags rather than layout names, so no
  argument to `select_layout` could reach them and a caller had to drop to
  `Server::cmd`.

- `Error::Overloaded` says what happened before it says which command it
  happened to. It led with the tmux format string the dispatch carried, which
  is long enough to be truncated, so the meaning arrived after the part a
  reader has to scroll past, and it now also says that nothing was sent and a
  retry is safe.

- `blocking::Runtime` no longer ends the process when it is dropped inside an
  async context. Dropping a tokio runtime blocks until its tasks stop, and
  blocking is forbidden inside another runtime, so a runtime built correctly at
  startup and dropped inside async work aborted: `try_run` reported the nesting
  as a recoverable error and the value that reported it then killed the caller
  who handled it. Such a drop now shuts the runtime down in the background,
  which gives up waiting for the executor to reap its tmux children in that
  case; a drop outside an async context still waits.

- The `scratch` example runs to completion and removes its socket. It asserted
  its own cleanup with the loud `sessions`, which reports the server tmux
  correctly shut down when its last session died as the failure it is, so the
  example exited nonzero on every run and never reached the line that cleaned
  up. `just examples` now runs every example and fails on a nonzero exit or a
  socket left behind.

- `Server::sessions` no longer fails on a session whose name or working
  directory is empty. Most supported tmux releases accept both, and the snapshot decoder refused the empty field rather than
  reading it: one such session made `sessions`, `windows`, `hierarchy`,
  `has_session` and `Session::refreshed` fail for every caller, and
  `sessions_or_empty` report no sessions while several existed. The poison row
  could not be reached to be killed either, because looking it up failed the
  same way. `session_name`, `session_path` and `window_linked_sessions_list`
  now decode an empty value as a value.

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
