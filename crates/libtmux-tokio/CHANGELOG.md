# Changelog

Notable changes to `libtmux-tokio`.

## Unreleased

### Added

- `Error::after_cleanup` combines an operation and its completed cleanup,
  preserving both typed errors when both fail.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))
- `Error::PlanInterrupted` retains the results of earlier plan operations when a later operation fails. A caller can recover a created session, window or pane from those results. ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

- Run tmux operations with Tokio. `Server::new_session` returns the shared creation receipt. Use `Server::inspect_session` to read the session and `Server::capture` to capture the pane identified by either driver. ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

### Changed

- **Behaviour change.** Creation replies without verified identities remain
  uncertain rather than reporting an acknowledged partial effect. Raw
  `Error::SessionExists` describes a diagnostic, not proof that nothing ran.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))
- **Behaviour change.** Tokio channel waits and queued locks have a separate
  finite budget, `DispatchLimits::max_parked`, shared across clones and
  control-derived handles. It defaults to 16 and includes retained channel
  signals. Saturation returns `Error::ParkedLimitExceeded` before spawning
  a client; signal and unlock keep their ordinary capacity. Fresh discovery
  of a replacement retires the old generation's clients and signals.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

- Explicit shutdown returns `Error::ShutdownPending` if child cleanup takes
  longer than one second. The server keeps reaping and a later
  `Server::shutdown` can confirm completion. A successful subprocess call
  also signals any surviving members of its client process group.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

- **Breaking.** The Tokio client no longer exposes `Error::ServerReplaced`.
  An ordinary queue-guard failure returns the native result without claiming
  that nothing ran. `Error::is_transient` no longer treats diagnostics about
  an absent server as proof that a mutation can be repeated safely.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

- `Server::kill` reports a refused shutdown when the daemon generation has changed. An endpoint already stopped at the initial check still succeeds. ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

- **Breaking.** Select, move and swap an observed placement through `WindowLink::{select, move_to, swap_with}`. Obtain it with `window.link()`. Reindexed links are refused, and successful moves and swaps refresh the returned placement. `Window::kill` still kills the intrinsic window, and `Window::link_to` can link it after its discovery session has gone. ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

### Fixed

- A subprocess request now returns its timeout and pending-cleanup evidence
  when all Tokio blocking workers stay occupied past the cleanup allowance.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

- All subprocess calls acquire the child before registering its pipes with
  Tokio. When registration fails after spawn, the client reaps the child
  and reports the operation’s uncertain effect. A runtime shutting down
  leaves reaping with the process supervisor.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

- Pane streams drain queued bytes before reporting connection loss. An idle
  control connection that closes without `%exit` yields a terminal error.
  An interrupted control task reports `Error::OperationCleanupPending` when
  the caller cannot confirm its client was reaped.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

- If a subprocess call and its cleanup both fail, `Error::OperationAndCleanup`
  retains the command error and the failed cleanup stage.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

- Generation discovery and session, window or pane listings report
  `Error::ServerGone` when the daemon has stopped, instead of a decode error.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))
