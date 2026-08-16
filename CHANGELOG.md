# Changelog

Notable changes to the workspace. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crates follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Crates are versioned together except `tmux-mcp`, which moves at its own pace
because its dependencies need a newer compiler than the libraries do.
`tmux-workspace` is not published.

While the version is `0.1.0-alpha.*`, any release may break the API, and
breaking changes are not called out as such: nothing here is stable yet. The
prerelease suffix is load-bearing rather than decorative, because Cargo will
not resolve a prerelease unless the requirement names one: a plain `0.1`
requirement selects nothing, so a caller opts in by writing the version out in
full.

## Unreleased

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
