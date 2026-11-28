# Changelog

Notable changes to `libtmux-blocking`.

## Unreleased

### Added

- Create sessions and capture panes synchronously. A creation receipt identifies the session, window and pane even when a subsequent hook fails. The client supervises subprocesses without Tokio. ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

### Fixed

- `Server::snapshots` preserves non-ASCII names when the caller uses the C
  locale.
- `ControlMode::attach` preserves session environment variables selected by
  `update-environment`.
- `ControlMode::attach` retries a pinned client's `-V` probe when a concurrent
  fork temporarily retains its writable executable handle. The attachment's
  deadline and cancellation still bound qualification.
- **Breaking.** `ControlMode::send` and `ControlSender::send` refuse scripts,
  unqualified waits and arbitrary output before submission. Inspect
  `ControlError::policy_refusal` for the reason. Use `Server::capture` for
  pane bytes and an ordinary tmux client for scripts.
