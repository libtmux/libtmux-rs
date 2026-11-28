# Changelog

Notable changes to `libtmux-blocking`.

## Unreleased

### Added

- Create sessions and capture panes synchronously. A creation receipt identifies the session, window and pane even when a subsequent hook fails. The client supervises subprocesses without Tokio. ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

### Fixed

- A control client waiting for its owner thread to start now honors an
  expired deadline, cancellation or shutdown before spawning tmux.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

- A control subscriber can read the final `%exit` notice and reach the end of
  the stream. An unexpected disconnect delivers its error once after queued
  notices and cleanup.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

- `Server::snapshots` preserves non-ASCII names when the caller uses the C
  locale.
- `ControlMode::attach` preserves session environment variables selected by
  `update-environment`.
- `ControlMode::attach` retries its identity and version probes when a
  concurrent fork still has the executable open for writing. The attachment
  deadline and cancellation apply while it waits.
- **Breaking.** `ControlMode::send` and `ControlSender::send` refuse scripts,
  unqualified waits and arbitrary output before submission. Inspect
  `ControlError::policy_refusal` for the reason. Use `Server::capture` for
  pane bytes and an ordinary tmux client for scripts.
