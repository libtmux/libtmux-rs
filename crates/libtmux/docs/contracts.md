# libtmux contracts

What `libtmux` promises beyond the [README](../README.md): how it checks the
tmux version, how a command ends, what an error tells a caller, and what the
test guard isolates. [`design.md`](design.md) says why the crate is shaped this
way, and [`findings.md`](findings.md) lists the tmux defects it works around.

## The tmux version check

Every ordinary command checks the connected daemon before dispatch, including
raw commands, chains, and scoped handles. A missing daemon is checked against
the executable that would start it. Unknown versions and failed probes refuse
the operation.

The version probe shares the command's timeout, and a queue guard refuses a
daemon replacement with a different version. The daemon's version is cached.
Every dispatch checks it in tmux's queue, and a mismatch invalidates the cache
for a fresh probe on the next operation.

A refusal prevents the requested operation from being dispatched. Version
probes and queue guards may still reach the daemon:

- A daemon older than 3.3a, including 3.1 and older, which cannot answer the
  probe at all, is refused as [`Error::UnsupportedTmuxVersion`].
- A report that names no release, such as a bare `master`, `openbsd-*` or
  `3.3a-dev`, is refused as [`Error::UnrecognizedTmuxVersion`].
- A failed probe is [`Error::VersionProbeFailed`], and output that is not a
  version report is [`Error::InvalidVersionOutput`].
- A deadline that passes during the check is [`Error::Timeout`] with
  `before_dispatch` set.
- A daemon replaced between the check and the request fails with
  [`Error::ServerReplaced`], and nothing ran.

To fix any of them, upgrade tmux and restart the server, or pin the previous
release of libtmux.

The configured executable and the metadata and guard commands the library owns
must retain tmux's native built-in semantics. Owner-defined `command-alias`
redefinitions of those commands are outside this qualification; their contents
run with tmux's own semantics.

[`Server::capabilities`] reports the configured executable and caches that
result, including versions below the floor. Standalone [`Server::cmd`] calls
with `-V` or `-h` also retain native executable diagnostics. Client flags have
no representation over an open control connection.

Async operations need an entered Tokio runtime. The default executable is
`tmux`, resolved through the `PATH` captured when the [`Server`] is built. Use
[`ServerBuilder::tmux_executable`] to select another.

## Deadlines, cancellation, and shutdown

Each subprocess command runs in an isolated process group with a supervised
deadline, 30 seconds by default and configurable through
[`ServerBuilder::default_timeout`]. The deadline starts before the command
waits for dispatch capacity.

Dropping the command future, reaching its timeout, or shutting the server down
signals the group and waits for the direct child while the runtime is alive.
The signal reaches the tmux client, and leaves the tmux server alone. A command
the server has already received still runs, so a dropped mutation may or may
not have happened, and a retry can repeat it.

A control-mode connection has its own isolated process group. Its attach
handshake and each command response use the same default timeout. A response's
deadline starts before its line is written and ends with the complete block.
Dropping both handles terminates and reaps the connection.

[`Server::shutdown()`] is shared by all clones. It cancels active subprocess
work and persistent control connections, rejects later commands and attaches,
and is safe to call concurrently or repeatedly. Await it, or await your
commands, before tearing the runtime down. Runtime destruction signals
best-effort and cannot promise that child reaping finished.

## Errors

[`Error`] has a variant per failure mode, and [`Error::kind`] reduces those to
the decision a caller makes. [`Error::is_object_gone`] is the branch most
programs write, because an object disappearing is an ordinary race and the
request was fine.

[`Error::is_transient`] answers the narrower replay question: `true` means the
same call can be repeated without duplicating an effect and may work after the
condition clears. A timeout is `false` even when the server handle remains
usable, because tmux may have completed the command before its caller stopped
waiting.

A composed call can fail after tmux accepted one of its effects. That is
[`Error::AfterEffect`], classified as [`ErrorKind::PartialEffect`]. Its source
still explains the later failure, but `is_transient` is `false` for the whole
call because replay may repeat the accepted effect.

A listing keeps the reason it failed. [`Server::sessions`] returns
`Result<Vec<Session>>`, so an unreachable tmux is an error and a running tmux
with no sessions is an empty list.

A caller that would rather show nothing writes
`sessions().await.unwrap_or_default()`, which suits a status line. A
reconciler that reads "no sessions" from an outage will delete everything, so
it should match on the error.

## The test guard

`test-support` exports [`TestServer`], the same guard this crate's own suite
uses. Each guard owns a tmux child on a private socket with an empty config,
so tests cannot reach your real server or each other.

The guard gives that child only the environment tmux and a pane's shell need:
`PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, the locale variables and
`TMUX_TMPDIR`. A test that reads the environment cannot read yours.

`shutdown().await` closes escaped clients, waits the daemon, and reports
cleanup failures. `Drop` forces best-effort cleanup even after the runtime has
ended. On Linux, cleanup also sweeps processes by an exact environment marker
through pidfds, so PID reuse cannot redirect a signal.

`LIBTMUX_TEST_TMUX` points the guard at a specific build when one release
matters.

[`Error`]: https://libtmux.org/en/rs/latest/reference/error-error/
[`Error::kind`]: https://libtmux.org/en/rs/latest/reference/error-error-kind/
[`Error::is_object_gone`]: https://libtmux.org/en/rs/latest/reference/error-error-is_object_gone/
[`Error::is_transient`]: https://libtmux.org/en/rs/latest/reference/error-error-is_transient/
[`Error::AfterEffect`]: https://libtmux.org/en/rs/latest/reference/error-error-aftereffect/
[`ErrorKind::PartialEffect`]: https://libtmux.org/en/rs/latest/reference/error-errorkind-partialeffect/
[`Error::UnsupportedTmuxVersion`]: https://libtmux.org/en/rs/latest/reference/error-error-unsupportedtmuxversion/
[`Error::UnrecognizedTmuxVersion`]: https://libtmux.org/en/rs/latest/reference/error-error-unrecognizedtmuxversion/
[`Error::VersionProbeFailed`]: https://libtmux.org/en/rs/latest/reference/error-error-versionprobefailed/
[`Error::InvalidVersionOutput`]: https://libtmux.org/en/rs/latest/reference/error-error-invalidversionoutput/
[`Error::Timeout`]: https://libtmux.org/en/rs/latest/reference/error-error-timeout/
[`Error::ServerReplaced`]: https://libtmux.org/en/rs/latest/reference/error-error-serverreplaced/
[`Server`]: https://libtmux.org/en/rs/latest/reference/server-server/
[`Server::capabilities`]: https://libtmux.org/en/rs/latest/reference/server-server-capabilities/
[`Server::cmd`]: https://libtmux.org/en/rs/latest/reference/server-server-cmd/
[`Server::sessions`]: https://libtmux.org/en/rs/latest/reference/server-server-sessions/
[`Server::shutdown()`]: https://libtmux.org/en/rs/latest/reference/server-server-shutdown/
[`ServerBuilder::tmux_executable`]: https://libtmux.org/en/rs/latest/reference/server-builder-serverbuilder-tmux_executable/
[`ServerBuilder::default_timeout`]: https://libtmux.org/en/rs/latest/reference/server-builder-serverbuilder-default_timeout/
[`TestServer`]: https://libtmux.org/en/rs/latest/reference/test-testserver/
