# Changelog

Notable changes to the workspace. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crates follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Crates are versioned together except `tmux-mcp`, which moves at its own pace
because its dependencies need a newer compiler than the libraries do.
`tmux-workspace` is not published.

While the version is `0.1.0-alpha.*`, any release may break the API.

## Unreleased

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
- `Error::CapabilityDefective`, for a capability the running release has and
  gets wrong. `Server::run_shell` now raises it on tmux 3.3, 3.3a, and 3.4,
  which send `run-shell` output to a pane's copy-mode buffer instead of the
  client and still exit zero. It previously returned an empty listing there,
  which a caller could not tell from a command that printed nothing.
- `ServerConfigurationErrorKind::NotInsideTmux` and `MalformedTmuxVariable`,
  separating "this process is not inside tmux", an ordinary state to branch on,
  from a `TMUX` variable that is present and does not say what tmux says.

### Changed

- Option failures are classified by the same path as every other failure. They
  previously built `Error::CommandFailed` directly and so bypassed it.
- Licensed under `MIT OR Apache-2.0` rather than MIT alone, matching Rust
  convention. Releases up to and including `0.1.0-alpha.3` were MIT only.
- `repository` points at this workspace rather than at the Python libtmux
  repository, which does not contain this code.

## 0.1.0-alpha.4 - 2026-08-14

`tmux-mcp` only.

## 0.1.0-alpha.3 - 2026-08-14

## 0.1.0-alpha.2 - 2026-08-14

## 0.1.0-alpha.1 - 2026-08-13

First published alphas. These predate this repository, which was extracted from
the Python libtmux repository without its history, so their changes are not
itemized here.
