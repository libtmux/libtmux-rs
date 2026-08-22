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
