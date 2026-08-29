//! Isolated real-tmux support for downstream tests.
//!
//! This non-default module requires the `test-support` Cargo feature:
//!
//! ```toml
//! [dev-dependencies]
//! libtmux = { version = "0.1", features = ["test-support"] }
//! ```
//!
//! The dev-dependency enables `test-support` on libtmux, so run the downstream
//! package's tests without a feature flag:
//!
//! ```console
//! $ cargo test
//! ```

mod containment;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

use rustix::fs::{AtFlags, Mode, OFlags, fchmod, fstat, openat, unlinkat};
use rustix::io::{Errno, write as write_all};
use rustix::process::{Pid, Signal, getpgid, kill_process, kill_process_group, test_kill_process};

use self::containment::OwnerContainment;
#[cfg(feature = "control-mode")]
use crate::ControlClientLimits;
use crate::limits::{DispatchLimits, OutputLimits};
use crate::{Command, Server};

const SOCKET_NAME: &str = "s";
const CONFIG_NAME: &str = "c";
const LOCK_NAME: &str = "s.lock";
/// Names the process that owns a fixture, so a sweep can tell an abandoned
/// one from a running one without guessing from timestamps.
const OWNER_NAME: &str = "owner";
/// Where this crate's fixtures live, and nothing else does.
///
/// Under a directory of its own rather than loose in the temporary root,
/// because more than one libtmux lives on a developer's machine: the Python
/// suites put their sockets in `/tmp` too, and a shared root makes "whose
/// leftover is this" unanswerable. Owning a directory makes it obvious, and
/// makes [`reap_abandoned_servers`] provably unable to reach anything else.
///
/// Socket paths are bounded by `sun_path`, so the root is kept short.
const FIXTURE_ROOT: &str = "/tmp/libtmux-rs-test";
const TERM: &str = "xterm-256color";

/// The configuration every fixture server starts from.
///
/// A pane must not run whoever's shell happens to own `$SHELL`. With no
/// `default-shell` tmux falls back to it, so a fixture pane sources the
/// developer's interactive startup files and the suite's timing becomes a
/// property of their dotfiles: measured here, an interactive `zsh` sourcing a
/// 395-line `.zshrc` reached a drawn prompt in 9.7 s while other tmux servers
/// on the machine were starting shells, against 12 ms for the shell below.
/// Startup files that enable shared shell history make that worse than it
/// looks, because every shell then takes one machine-wide lock on one history
/// file, coupling a pane here to every unrelated shell starting anywhere.
///
/// `default-command` is set as well as `default-shell`: tmux runs the shell as
/// a *login* shell when the command is empty, which still reads `/etc/profile`
/// and `~/.profile`. Setting both leaves a pane that reads nothing.
const FIXTURE_CONFIG: &str = "\
set -g default-shell /bin/sh\n\
set -g default-command /bin/sh\n";
/// How long containment waits for a process group to go away.
const CONTAINMENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Widen every fixture deadline by `LIBTMUX_TEST_TIMEOUT_SCALE`.
///
/// Five seconds bounds a tmux that starts on a machine with a core to spare.
/// It stops bounding one on a machine running several times its cores in
/// work, and a deadline that fires on a healthy fixture reports the load
/// rather than the thing under test -- which is the failure [design.md]
/// already describes and the rule it draws from it.
///
/// Read once, because a deadline that changed inside a run would make two
/// tests in the same suite measure different things. Unset, unparseable, or
/// below `1` all mean `1`: this exists to widen a deadline, and narrowing one
/// here would fail a fixture for a reason no caller asked for. A test wanting
/// a short deadline sets its own through
/// [`TestServerBuilder::lifecycle_timeout`], which this does not touch.
///
/// [design.md]: ../docs/design.md
fn timeout_scale() -> f64 {
    static SCALE: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *SCALE.get_or_init(|| {
        parse_timeout_scale(std::env::var("LIBTMUX_TEST_TIMEOUT_SCALE").ok().as_deref())
    })
}

/// Read a scale, taking anything that is not a number above `1` as `1`.
///
/// Separate from [`timeout_scale`] because that reads the environment once
/// into a `OnceLock`, which a test cannot set and cannot reset. What is worth
/// testing is this decision, and it is reachable without a process.
fn parse_timeout_scale(value: Option<&str>) -> f64 {
    value
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|scale| scale.is_finite())
        .map_or(1.0, |scale| scale.max(1.0))
}

/// A fixture deadline, widened by [`timeout_scale`].
fn scaled(base: Duration) -> Duration {
    base.mul_f64(timeout_scale())
}

/// The grace ceiling this platform uses, widened like every other deadline.
fn platform_fallback_grace_ceiling() -> Option<Duration> {
    PLATFORM_FALLBACK_GRACE_CEILING.map(scaled)
}
/// How long a blocking wait sleeps before looking again.
///
/// Sleeping rather than yielding, for the reason [design.md] gives about the
/// async poll loops and which applies with more force here: this runs inside
/// `spawn_blocking` while waiting for a *process* to exit, so a spin holds a
/// core the daemon needs to handle the signal it was just sent. On a machine
/// with fewer cores than the suite has concurrent fixtures, that is the
/// difference between a clean shutdown and a grace window that expires.
///
/// [design.md]: ../docs/design.md
pub(crate) const CLEANUP_POLL_INTERVAL: Duration = Duration::from_millis(1);

#[cfg(any(
    target_os = "cygwin",
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
))]
const FALLBACK_GRACE_CEILING: Duration = Duration::from_secs(5);

#[cfg(any(
    target_os = "cygwin",
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
))]
const PLATFORM_FALLBACK_GRACE_CEILING: Option<Duration> = Some(FALLBACK_GRACE_CEILING);

#[cfg(not(any(
    target_os = "cygwin",
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
const PLATFORM_FALLBACK_GRACE_CEILING: Option<Duration> = None;

// The observer travels with the child owner so startup and graceful cleanup
// preserve the same non-reaping result on every supported Unix target.
type LeaderObserver = fn(Pid) -> LeaderObservation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LeaderObservation {
    Running,
    ExitedUnreaped,
    ExternallyReaped,
    #[allow(
        dead_code,
        reason = "constructed on targets without a non-reaping waitid observer"
    )]
    Unavailable,
    Failed,
}

/// The category of a [`TestServer`] lifecycle failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TestServerErrorKind {
    /// The owned directory or empty configuration file could not be prepared.
    FilesystemSetupFailed,
    /// The generated Unix socket path cannot carry tmux's C-string terminator.
    SocketPathTooLong,
    /// The configured tmux executable was not found.
    ExecutableNotFound,
    /// Starting the foreground tmux daemon failed.
    DaemonSpawnFailed,
    /// The foreground daemon exited before readiness completed.
    DaemonExited,
    /// The readiness probe could not be issued or returned malformed output.
    ReadinessProbeFailed,
    /// The daemon reported a different PID than the retained child.
    DaemonPidMismatch,
    /// Startup did not become ready before the configured deadline.
    StartupTimedOut,
    /// Startup rollback or shutdown could not terminate or wait for the daemon,
    /// close the client executor, or complete its blocking lifecycle waiter.
    ShutdownFailed,
    /// Fixed-entry removal or owned-directory removal could not be proved.
    CleanupFailed,
}

/// A source-less, redacted failure from [`TestServer`].
pub struct TestServerError {
    kind: TestServerErrorKind,
    /// Which step produced it, for kinds several steps can produce.
    ///
    /// A fixture that fails on someone else's machine is debugged from this
    /// line alone, and `ShutdownFailed` alone does not say whether the
    /// executor, the wait, or the signal was the part that went wrong.
    stage: Option<String>,
}

impl fmt::Debug for TestServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TestServerError")
            .field("kind", &self.kind)
            .field("stage", &self.stage)
            .finish()
    }
}

impl TestServerError {
    const fn new(kind: TestServerErrorKind) -> Self {
        Self { kind, stage: None }
    }

    /// Record which step failed, where the kind alone is ambiguous.
    fn at(kind: TestServerErrorKind, stage: impl Into<String>) -> Self {
        Self {
            kind,
            stage: Some(stage.into()),
        }
    }

    /// Which step produced this, when the kind alone does not say.
    #[must_use]
    pub fn stage(&self) -> Option<&str> {
        self.stage.as_deref()
    }

    /// Return the stable category for this failure.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::test::{TestServer, TestServerErrorKind};
    ///
    /// let missing = tempfile::tempdir()?.path().join("missing-tmux");
    /// let error = TestServer::builder()
    ///     .tmux_executable(missing)
    ///     .start()
    ///     .await
    ///     .expect_err("missing executable is rejected");
    /// assert_eq!(error.kind(), TestServerErrorKind::ExecutableNotFound);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })
    /// # }
    /// ```
    #[must_use]
    pub const fn kind(&self) -> TestServerErrorKind {
        self.kind
    }
}

impl fmt::Display for TestServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.kind {
            TestServerErrorKind::FilesystemSetupFailed => "test-server filesystem setup failed",
            TestServerErrorKind::SocketPathTooLong => "test-server socket path is too long",
            TestServerErrorKind::ExecutableNotFound => "test-server executable was not found",
            TestServerErrorKind::DaemonSpawnFailed => "test-server daemon spawn failed",
            TestServerErrorKind::DaemonExited => "test-server daemon exited during startup",
            TestServerErrorKind::ReadinessProbeFailed => "test-server readiness probe failed",
            TestServerErrorKind::DaemonPidMismatch => "test-server daemon PID did not match",
            TestServerErrorKind::StartupTimedOut => "test-server startup timed out",
            TestServerErrorKind::ShutdownFailed => "test-server shutdown failed",
            TestServerErrorKind::CleanupFailed => "test-server cleanup failed",
        })
    }
}

impl std::error::Error for TestServerError {}

/// A consuming builder for an isolated foreground tmux test server.
#[must_use = "use start to create the isolated test server"]
pub struct TestServerBuilder {
    executable: OsString,
    lifecycle_timeout: Duration,
    output_limits: OutputLimits,
    dispatch_limits: DispatchLimits,
    #[cfg(feature = "control-mode")]
    control_client_limits: ControlClientLimits,
}

/// The tmux to run, unless a caller names one.
///
/// `LIBTMUX_TEST_TMUX` first, then `tmux` resolved through `PATH`. The
/// variable is what the compatibility lane sets to pin a release, and reading
/// it here is what makes the name true: before this, it steered only the tests
/// that read it directly, so pointing it at a pinned build and running the
/// suite produced a green run against whatever `PATH` happened to hold. A pass
/// about the wrong binary is worse than a failure, because nothing says so.
fn default_executable() -> OsString {
    std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| OsString::from("tmux"))
}

impl fmt::Debug for TestServerBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TestServerBuilder")
            .field("lifecycle_timeout", &self.lifecycle_timeout)
            .finish_non_exhaustive()
    }
}

impl TestServerBuilder {
    fn new() -> Self {
        Self {
            executable: default_executable(),
            lifecycle_timeout: scaled(Duration::from_secs(5)),
            output_limits: OutputLimits::default(),
            dispatch_limits: DispatchLimits::default(),
            #[cfg(feature = "control-mode")]
            control_client_limits: ControlClientLimits::default(),
        }
    }

    /// Bound how many bytes one command may read, as [`crate::ServerBuilder`]
    /// does, so a test can prove the ceiling without producing 32 MiB.
    #[must_use = "use the returned builder to retain the limits"]
    pub const fn output_limits(mut self, limits: OutputLimits) -> Self {
        self.output_limits = limits;
        self
    }

    /// Bound how many commands may run at once, as [`crate::ServerBuilder`]
    /// does.
    #[must_use = "use the returned builder to retain the limits"]
    pub const fn dispatch_limits(mut self, limits: DispatchLimits) -> Self {
        self.dispatch_limits = limits;
        self
    }

    /// Bound persistent clients as [`crate::ServerBuilder`] does.
    #[cfg(feature = "control-mode")]
    #[must_use = "use the returned builder to retain the limits"]
    pub const fn control_client_limits(mut self, limits: ControlClientLimits) -> Self {
        self.control_client_limits = limits;
        self
    }

    /// Configure the tmux executable used by the daemon and every client.
    ///
    /// Defaults to `LIBTMUX_TEST_TMUX`, and to `tmux` resolved through `PATH`
    /// when that is unset -- so pinning a release across a whole run is the
    /// variable's job, and this is for a test that needs a particular build
    /// whatever the run was pointed at.
    ///
    /// ```
    /// use libtmux::test::TestServer;
    ///
    /// let _builder = TestServer::builder().tmux_executable("tmux");
    /// ```
    #[must_use = "use the returned builder to retain the executable"]
    pub fn tmux_executable(mut self, executable: impl Into<OsString>) -> Self {
        self.executable = executable.into();
        self
    }

    /// Configure the bounded startup and graceful-shutdown observation time.
    ///
    /// On targets without a safe non-reaping child-exit observer, the graceful
    /// shutdown phase is capped at five seconds before forced cleanup.
    ///
    /// ```
    /// use libtmux::test::TestServer;
    ///
    /// let _builder = TestServer::builder()
    ///     .lifecycle_timeout(std::time::Duration::from_secs(1));
    /// ```
    #[must_use = "use the returned builder to retain the lifecycle timeout"]
    pub fn lifecycle_timeout(mut self, timeout: Duration) -> Self {
        self.lifecycle_timeout = timeout;
        self
    }

    /// Start the isolated foreground tmux daemon.
    ///
    /// # Errors
    ///
    /// Returns a source-less test-support error when setup, startup, readiness,
    /// or rollback cleanup cannot be completed safely.
    ///
    /// Startup creates a private explicit socket and owned empty config without
    /// creating an initial session. Cancelling this future before ownership
    /// transfers to the returned guard triggers synchronous forced best-effort
    /// rollback, whose cleanup failures cannot be reported to the cancelled
    /// caller.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::builder().start().await?;
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })
    /// # }
    /// ```
    pub async fn start(self) -> Result<TestServer, TestServerError> {
        self.start_with_leader_observer(leader_exited_unreaped, platform_fallback_grace_ceiling())
            .await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "server construction and rollback ownership form one startup sequence"
    )]
    async fn start_with_leader_observer(
        self,
        leader_observer: LeaderObserver,
        fallback_grace_ceiling: Option<Duration>,
    ) -> Result<TestServer, TestServerError> {
        let files = OwnedFiles::create()?;
        if !socket_path_fits_tmux(&files.socket_path) {
            return Err(TestServerError::new(TestServerErrorKind::SocketPathTooLong));
        }

        let builder = Server::builder()
            .socket_path(&files.socket_path)
            .config_file(&files.config_path)
            .tmux_executable(self.executable.clone())
            .output_limits(self.output_limits)
            .dispatch_limits(self.dispatch_limits);
        #[cfg(feature = "control-mode")]
        let builder = builder.control_client_limits(self.control_client_limits);
        let server = builder
            .prevent_server_start()
            .build()
            .map_err(|_| TestServerError::new(TestServerErrorKind::FilesystemSetupFailed))?;

        let mut command = ProcessCommand::new(&self.executable);
        command
            .arg("-D")
            .arg("-S")
            .arg(&files.socket_path)
            .arg("-f")
            .arg(&files.config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env("TERM", TERM)
            .process_group(0);
        files.containment.configure(&mut command);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let kind = if error.kind() == std::io::ErrorKind::NotFound {
                    TestServerErrorKind::ExecutableNotFound
                } else {
                    TestServerErrorKind::DaemonSpawnFailed
                };
                return server_startup_failure(&server, kind).await;
            }
        };
        let mut startup = StartupGuard::new(Lifecycle::new_with_leader_observer(
            child,
            files,
            leader_observer,
            fallback_grace_ceiling,
        ));
        let Some(lifecycle) = startup.lifecycle() else {
            return server_startup_failure(&server, TestServerErrorKind::CleanupFailed).await;
        };
        if getpgid(Some(lifecycle.pid)).ok() != Some(lifecycle.pid) {
            return startup_failure(&server, startup, TestServerErrorKind::DaemonSpawnFailed).await;
        }

        let Ok(daemon_pid) = u32::try_from(lifecycle.pid.as_raw_pid()) else {
            return startup_failure(&server, startup, TestServerErrorKind::DaemonSpawnFailed).await;
        };
        let started = Instant::now();
        loop {
            let Some(lifecycle) = startup.lifecycle_mut() else {
                return server_startup_failure(&server, TestServerErrorKind::CleanupFailed).await;
            };
            match lifecycle.observe_leader() {
                LeaderObservation::ExitedUnreaped => {
                    return startup_failure(&server, startup, TestServerErrorKind::DaemonExited)
                        .await;
                }
                LeaderObservation::ExternallyReaped | LeaderObservation::Failed => {
                    return startup_failure(&server, startup, TestServerErrorKind::ShutdownFailed)
                        .await;
                }
                LeaderObservation::Running | LeaderObservation::Unavailable => {}
            }
            let Some(remaining) = remaining_timeout(started, self.lifecycle_timeout) else {
                return startup_timeout(&server, startup).await;
            };
            match readiness_with_timeout(&server, remaining).await {
                Err(()) => return startup_timeout(&server, startup).await,
                Ok(Ok(Some(found))) if found == daemon_pid => {
                    let Some(lifecycle) = startup.disarm() else {
                        return server_startup_failure(&server, TestServerErrorKind::CleanupFailed)
                            .await;
                    };
                    return Ok(TestServer {
                        server,
                        socket_path: lifecycle.files.socket_path.clone(),
                        daemon_pid,
                        lifecycle_timeout: self.lifecycle_timeout,
                        lifecycle: Some(lifecycle),
                    });
                }
                Ok(Ok(Some(_))) => {
                    return startup_failure(
                        &server,
                        startup,
                        TestServerErrorKind::DaemonPidMismatch,
                    )
                    .await;
                }
                Ok(Ok(None)) => {}
                Ok(Err(())) => {
                    return startup_failure(
                        &server,
                        startup,
                        TestServerErrorKind::ReadinessProbeFailed,
                    )
                    .await;
                }
            }
            if remaining_timeout(started, self.lifecycle_timeout).is_none() {
                return startup_timeout(&server, startup).await;
            }
            tokio::task::yield_now().await;
        }
    }
}

/// One isolated tmux daemon and its descriptor-relative cleanup guard.
///
/// This value owns its foreground daemon and socket-directory lifecycle and
/// holds a no-start [`Server`] handle. The daemon uses a private explicit
/// socket and an owned empty config, creating no initial session. Escaped
/// [`Server`] clones share that handle's executor. [`TestServer::shutdown`]
/// closes the shared executor and reports graceful lifecycle and cleanup
/// failures; [`Drop`] performs synchronous forced best-effort daemon and file
/// cleanup whose failures cannot be observed.
#[must_use = "keep the guard alive or call shutdown to clean up its daemon"]
pub struct TestServer {
    server: Server,
    socket_path: PathBuf,
    daemon_pid: u32,
    lifecycle_timeout: Duration,
    lifecycle: Option<Lifecycle>,
}

impl fmt::Debug for TestServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TestServer")
            .field("daemon_pid", &self.daemon_pid)
            .finish_non_exhaustive()
    }
}

/// Whether a fixture's tmux daemon is still running, and how it ended if not.
///
/// A test drives tmux through tmux's own client, and that client reports a
/// daemon that died as an ordinary refusal: it prints `server exited
/// unexpectedly` and exits 1, the same shape as a command tmux rejected. An
/// assertion written against the reply alone therefore blames the command.
/// [`TestServer::daemon_state`] answers the question the reply cannot.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::test::{DaemonState, TestServer};
///
/// let mut guard = TestServer::new().await?;
/// assert_eq!(guard.daemon_state(), DaemonState::Running);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DaemonState {
    /// The daemon has not exited.
    Running,
    /// The daemon has exited, with the status the kernel reported.
    ///
    /// `signal: 11 (SIGSEGV)` is tmux crashing; `signal: 9 (SIGKILL)` is
    /// something outside the fixture killing it; an exit status is tmux
    /// deciding to stop.
    Gone(std::process::ExitStatus),
    /// The wait could not be made, so the daemon's fate is unknown.
    Unreadable,
}

impl DaemonState {
    /// Whether the daemon has not exited.
    ///
    /// [`DaemonState::Unreadable`] counts as not running: a fixture that
    /// cannot prove its daemon is there has nothing to assert against.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let mut guard = libtmux::test::TestServer::new().await?;
    /// assert!(guard.daemon_state().is_running());
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })
    /// # }
    /// ```
    #[must_use]
    pub const fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }
}

impl fmt::Display for DaemonState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => formatter.write_str("running"),
            Self::Gone(status) => write!(formatter, "gone ({status})"),
            Self::Unreadable => formatter.write_str("in an unreadable state"),
        }
    }
}

impl TestServer {
    /// Start an isolated tmux server with the default builder settings.
    ///
    /// # Errors
    ///
    /// Returns a source-less test-support error when setup or startup fails.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })
    /// # }
    /// ```
    pub async fn new() -> Result<Self, TestServerError> {
        Self::builder().start().await
    }

    /// Start a consuming isolated test-server builder.
    ///
    /// ```
    /// let _builder = libtmux::test::TestServer::builder();
    /// ```
    pub fn builder() -> TestServerBuilder {
        TestServerBuilder::new()
    }

    /// Return the no-start client handle for this isolated daemon.
    ///
    /// Clones share this handle's executor. Consuming [`TestServer::shutdown`]
    /// closes it, so escaped clones reject later commands instead of starting a
    /// replacement daemon.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let client = guard.server().clone();
    /// guard.shutdown().await?;
    /// assert!(client.cmd(libtmux::Command::new("list-sessions")).await.is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })
    /// # }
    /// ```
    #[must_use]
    pub fn server(&self) -> &Server {
        &self.server
    }

    /// Return this guard's explicit Unix socket path.
    ///
    /// The path is removed during successful shutdown or forced Drop cleanup.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// assert!(guard.socket_path().is_absolute());
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })
    /// # }
    /// ```
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Create a session on this fixture's server.
    ///
    /// A test almost always wants a session before it wants anything else,
    /// and reaching through [`TestServer::server`] to say so is noise. Takes
    /// the same options [`crate::Server::new_session`] does, so a test that
    /// needs a start directory or a window name says it here rather than
    /// dropping back to the server.
    ///
    /// # Errors
    ///
    /// Returns the error tmux gave, including
    /// [`crate::Error::SessionExists`] when the name is taken.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.session("work").await?;
    ///
    /// assert_eq!(session.name().as_bytes(), b"work");
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn session(
        &self,
        options: impl Into<crate::NewSessionOptions>,
    ) -> Result<crate::Session, crate::Error> {
        self.server().new_session(options).await
    }

    /// Return the retained foreground daemon PID.
    ///
    /// This is the PID captured at spawn time, not a liveness probe.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// assert!(guard.daemon_pid() > 1);
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })
    /// # }
    /// ```
    #[must_use]
    pub const fn daemon_pid(&self) -> u32 {
        self.daemon_pid
    }

    /// Report whether the daemon is still running, and how it ended if not.
    ///
    /// Takes `&mut self` because reading the fate of a daemon that has exited
    /// reaps it; the guard then skips the signalling it no longer needs and
    /// still sweeps the panes the daemon left behind. Calling this on a
    /// running daemon changes nothing and costs one `waitpid`.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use std::time::Duration;
    ///
    /// use libtmux::{Command, test::{DaemonState, TestServer, retry_until}};
    ///
    /// let mut guard = TestServer::new().await?;
    /// guard.session("work").await?;
    /// guard.server().cmd(Command::new("kill-server")).await.ok();
    ///
    /// // tmux stops answering on the socket before the kernel has a status
    /// // for the process behind it, so this is a wait rather than a reading.
    /// retry_until(Duration::from_secs(5), async || {
    ///     !guard.daemon_state().is_running()
    /// })
    /// .await?;
    /// assert!(matches!(guard.daemon_state(), DaemonState::Gone(_)));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn daemon_state(&mut self) -> DaemonState {
        self.lifecycle
            .as_mut()
            .map_or(DaemonState::Unreadable, Lifecycle::daemon_state)
    }

    /// Stop the client executor and consume the daemon guard.
    ///
    /// # Errors
    ///
    /// Returns `ShutdownFailed` when executor closure, lifecycle
    /// signaling/waiting, or the blocking waiter fails. Returns `CleanupFailed`
    /// when descriptor-relative cleanup cannot be proved after lifecycle
    /// processing.
    ///
    /// This consumes the guard, closes its shared no-start executor, and then
    /// terminates the retained foreground daemon. Escaped [`Server`] clones
    /// cannot issue later commands.
    ///
    /// Cancelling this future before lifecycle ownership transfers leaves the
    /// guard responsible for synchronous forced best-effort cleanup. Once this
    /// future transfers ownership to its blocking waiter, that waiter completes
    /// cleanup even if this future is cancelled; later cleanup failures are
    /// unobservable to the cancelled caller.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })
    /// # }
    /// ```
    pub async fn shutdown(mut self) -> Result<(), TestServerError> {
        let executor_failed = self.server.shutdown().await.is_err();
        let Some(mut lifecycle) = self.lifecycle.take() else {
            return Err(TestServerError::at(
                TestServerErrorKind::CleanupFailed,
                "lifecycle already taken",
            ));
        };
        let timeout = self.lifecycle_timeout;
        let waiter = tokio::task::spawn_blocking(move || {
            let outcome = lifecycle.cleanup(timeout);
            (outcome, lifecycle.failure())
        });
        let Ok((cleanup, detail)) = waiter.await else {
            return Err(TestServerError::at(
                TestServerErrorKind::ShutdownFailed,
                "cleanup task",
            ));
        };
        if executor_failed {
            return Err(TestServerError::at(
                TestServerErrorKind::ShutdownFailed,
                "executor",
            ));
        }
        match cleanup {
            CleanupOutcome::Complete => Ok(()),
            CleanupOutcome::LifecycleFailed | CleanupOutcome::LifecycleAndFilesystemFailed => {
                Err(TestServerError::at(
                    TestServerErrorKind::ShutdownFailed,
                    detail.unwrap_or_else(|| "daemon did not exit".to_owned()),
                ))
            }
            CleanupOutcome::FilesystemFailed => Err(TestServerError::at(
                TestServerErrorKind::CleanupFailed,
                "files remain",
            )),
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(mut lifecycle) = self.lifecycle.take() {
            let _ = lifecycle.force_cleanup();
        }
    }
}

struct OwnedFiles {
    socket_path: PathBuf,
    config_path: PathBuf,
    directory_name: OsString,
    socket_name: OsString,
    config_name: OsString,
    lock_name: OsString,
    owner_name: OsString,
    parent: OwnedFd,
    directory: OwnedFd,
    containment: OwnerContainment,
    cleanup_armed: bool,
}

impl OwnedFiles {
    fn create() -> Result<Self, TestServerError> {
        Self::create_with_setup_hook(|_| {})
    }

    fn create_with_setup_hook(hook: impl FnOnce(&Path)) -> Result<Self, TestServerError> {
        // Created rather than assumed: the first fixture on a machine makes
        // the root, and every later one shares it.
        fs::create_dir_all(FIXTURE_ROOT)
            .map_err(|_| TestServerError::new(TestServerErrorKind::FilesystemSetupFailed))?;
        let temporary = tempfile::Builder::new()
            .prefix("s-")
            .tempdir_in(FIXTURE_ROOT)
            .map_err(|_| TestServerError::new(TestServerErrorKind::FilesystemSetupFailed))?;
        let directory_path = temporary.path().to_path_buf();
        let initial = match fs::symlink_metadata(&directory_path) {
            Ok(metadata) if metadata.file_type().is_dir() => metadata,
            Ok(_) | Err(_) => return Err(retain_setup_failure(temporary)),
        };
        // A restrictive umask can create mode 000, which cannot be opened
        // portably. Repair it, then authenticate the inode before creating any
        // entry or retaining the directory beyond TempDir's ownership.
        if fs::set_permissions(&directory_path, fs::Permissions::from_mode(0o700)).is_err() {
            return Err(retain_setup_failure(temporary));
        }
        let repaired = match fs::symlink_metadata(&directory_path) {
            Ok(metadata) if same_metadata_identity(&initial, &metadata) => metadata,
            Ok(_) | Err(_) => return Err(retain_setup_failure(temporary)),
        };
        let parent_path = directory_path
            .parent()
            .ok_or_else(|| TestServerError::new(TestServerErrorKind::FilesystemSetupFailed))?;
        let directory_name = directory_path
            .file_name()
            .ok_or_else(|| TestServerError::new(TestServerErrorKind::FilesystemSetupFailed))?
            .to_os_string();
        hook(&directory_path);
        let Ok(parent) = open_directory(parent_path) else {
            return Err(retain_setup_failure(temporary));
        };
        let Ok(directory) = open_owned_directory(&parent, &directory_name) else {
            return Err(retain_setup_failure(temporary));
        };
        if !metadata_matches_fd(&repaired, &directory) {
            return Err(retain_setup_failure(temporary));
        }
        let Ok(verified) = open_owned_directory(&parent, &directory_name) else {
            return Err(retain_setup_failure(temporary));
        };
        if !same_fd_identity(&directory, &verified) {
            return Err(retain_setup_failure(temporary));
        }
        let socket_name = OsString::from(SOCKET_NAME);
        let config_name = OsString::from(CONFIG_NAME);
        let lock_name = OsString::from(LOCK_NAME);
        let owner_name = OsString::from(OWNER_NAME);
        let containment = OwnerContainment::new(&directory_name);
        let directory_path = temporary.keep();
        let files = Self {
            socket_path: directory_path.join(&socket_name),
            config_path: directory_path.join(&config_name),
            directory_name,
            socket_name,
            config_name,
            lock_name,
            owner_name,
            parent,
            directory,
            containment,
            cleanup_armed: true,
        };
        fchmod(&files.directory, Mode::from_raw_mode(0o700))
            .map_err(|_| TestServerError::new(TestServerErrorKind::FilesystemSetupFailed))?;
        if fstat(&files.directory)
            .map_err(|_| TestServerError::new(TestServerErrorKind::FilesystemSetupFailed))?
            .st_mode
            & 0o777
            != 0o700
        {
            return Err(TestServerError::new(
                TestServerErrorKind::FilesystemSetupFailed,
            ));
        }
        let config = openat(
            &files.directory,
            &files.config_name,
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| TestServerError::new(TestServerErrorKind::FilesystemSetupFailed))?;
        fchmod(&config, Mode::from_raw_mode(0o600))
            .map_err(|_| TestServerError::new(TestServerErrorKind::FilesystemSetupFailed))?;
        if fstat(&config)
            .map_err(|_| TestServerError::new(TestServerErrorKind::FilesystemSetupFailed))?
            .st_mode
            & 0o777
            != 0o600
        {
            return Err(TestServerError::new(
                TestServerErrorKind::FilesystemSetupFailed,
            ));
        }
        write_all(&config, FIXTURE_CONFIG.as_bytes())
            .map_err(|_| TestServerError::new(TestServerErrorKind::FilesystemSetupFailed))?;
        files.record_owner()?;
        Ok(files)
    }

    /// Name the process that owns this fixture, so a sweep can tell an
    /// abandoned one from a running one.
    fn record_owner(&self) -> Result<(), TestServerError> {
        let owner = openat(
            &self.directory,
            &self.owner_name,
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| TestServerError::new(TestServerErrorKind::FilesystemSetupFailed))?;
        write_all(&owner, std::process::id().to_string().as_bytes())
            .map_err(|_| TestServerError::new(TestServerErrorKind::FilesystemSetupFailed))?;
        Ok(())
    }

    fn retain_until_contained(&mut self) {
        self.cleanup_armed = false;
    }

    fn cleanup_after_containment(&mut self) -> bool {
        self.cleanup_armed = true;
        self.cleanup()
    }

    fn cleanup(&mut self) -> bool {
        let socket = unlink_fixed(&self.directory, &self.socket_name);
        let config = unlink_fixed(&self.directory, &self.config_name);
        let lock = unlink_fixed(&self.directory, &self.lock_name);
        let _ = unlink_fixed(&self.directory, &self.owner_name);
        let cleaned = socket && config && lock && self.remove_directory();
        if cleaned {
            self.cleanup_armed = false;
        }
        cleaned
    }

    fn remove_directory(&self) -> bool {
        let opened = openat(
            &self.parent,
            &self.directory_name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        );
        let Ok(opened) = opened else {
            return false;
        };
        let Ok(expected) = fstat(&self.directory) else {
            return false;
        };
        let Ok(found) = fstat(&opened) else {
            return false;
        };
        if expected.st_dev != found.st_dev || expected.st_ino != found.st_ino {
            return false;
        }
        unlinkat(&self.parent, &self.directory_name, AtFlags::REMOVEDIR).is_ok()
    }
}

impl Drop for OwnedFiles {
    fn drop(&mut self) {
        if self.cleanup_armed {
            let _ = self.cleanup();
        }
    }
}

struct Lifecycle {
    child: Child,
    pid: Pid,
    files: OwnedFiles,
    leader_observer: LeaderObserver,
    fallback_grace_ceiling: Option<Duration>,
    leader_state: LeaderState,
    cleanup_pending: bool,
    /// Which sub-condition failed, for a cleanup that did.
    ///
    /// A fixture failing on a machine the author does not have is debugged
    /// from this alone, and "shutdown failed" names four different problems.
    failure: Option<String>,
}

#[derive(Clone, Copy, Debug)]
enum LeaderState {
    Waitable,
    Reaped(std::process::ExitStatus),
    Lost,
}

enum CleanupOutcome {
    Complete,
    LifecycleFailed,
    FilesystemFailed,
    LifecycleAndFilesystemFailed,
}

impl Lifecycle {
    #[cfg(test)]
    fn new(child: Child, files: OwnedFiles) -> Self {
        Self::new_with_leader_observer(
            child,
            files,
            leader_exited_unreaped,
            platform_fallback_grace_ceiling(),
        )
    }

    fn new_with_leader_observer(
        child: Child,
        mut files: OwnedFiles,
        leader_observer: LeaderObserver,
        fallback_grace_ceiling: Option<Duration>,
    ) -> Self {
        let pid = Pid::from_child(&child);
        files.retain_until_contained();
        Self {
            child,
            pid,
            files,
            leader_observer,
            fallback_grace_ceiling,
            leader_state: LeaderState::Waitable,
            cleanup_pending: true,
            failure: None,
        }
    }

    fn observe_leader(&mut self) -> LeaderObservation {
        match self.leader_state {
            LeaderState::Reaped(_) => return LeaderObservation::ExitedUnreaped,
            LeaderState::Lost => return LeaderObservation::ExternallyReaped,
            LeaderState::Waitable => {}
        }
        let observation = (self.leader_observer)(self.pid);
        if matches!(
            observation,
            LeaderObservation::ExternallyReaped | LeaderObservation::Failed
        ) {
            self.leader_state = LeaderState::Lost;
        }
        observation
    }

    fn numeric_phase_is_safe(&mut self) -> bool {
        matches!(
            self.observe_leader(),
            LeaderObservation::Running
                | LeaderObservation::ExitedUnreaped
                | LeaderObservation::Unavailable
        ) && matches!(self.leader_state, LeaderState::Waitable)
    }

    #[cfg(test)]
    fn numeric_signaling_retired(&self) -> bool {
        !matches!(self.leader_state, LeaderState::Waitable)
    }

    fn reaped_status(&self) -> Option<std::process::ExitStatus> {
        match self.leader_state {
            LeaderState::Reaped(status) => Some(status),
            LeaderState::Waitable | LeaderState::Lost => None,
        }
    }

    /// Read the daemon's fate without disturbing a daemon that is still there.
    ///
    /// Reaping here is what makes the status readable at all: only the parent
    /// can be told how a child ended, and the fixture is the parent. The
    /// retained status is the one cleanup would have collected, so cleanup
    /// stays correct after this runs.
    fn daemon_state(&mut self) -> DaemonState {
        if let Some(status) = self.reaped_status() {
            return DaemonState::Gone(status);
        }
        if !matches!(self.leader_state, LeaderState::Waitable) {
            return DaemonState::Unreadable;
        }
        match self.child.try_wait() {
            Ok(None) => DaemonState::Running,
            Ok(Some(status)) => {
                self.leader_state = LeaderState::Reaped(status);
                DaemonState::Gone(status)
            }
            Err(_) => DaemonState::Unreadable,
        }
    }

    fn cleanup(&mut self, timeout: Duration) -> CleanupOutcome {
        let (_, lifecycle_ok) = self.terminate_gracefully(timeout);
        let filesystem_ok = lifecycle_ok && self.files.cleanup_after_containment();
        CleanupOutcome::from_results(lifecycle_ok, filesystem_ok)
    }

    fn force_cleanup(&mut self) -> CleanupOutcome {
        let (_, lifecycle_ok) = self.terminate_forced();
        let filesystem_ok = lifecycle_ok && self.files.cleanup_after_containment();
        CleanupOutcome::from_results(lifecycle_ok, filesystem_ok)
    }

    fn force_cleanup_status(&mut self) -> Result<std::process::ExitStatus, CleanupOutcome> {
        let (status, lifecycle_ok) = self.terminate_forced();
        let filesystem_ok = lifecycle_ok && self.files.cleanup_after_containment();
        let outcome = CleanupOutcome::from_results(lifecycle_ok, filesystem_ok);
        if !matches!(outcome, CleanupOutcome::Complete) {
            return Err(outcome);
        }
        status.ok_or(CleanupOutcome::LifecycleFailed)
    }

    fn terminate_gracefully(
        &mut self,
        timeout: Duration,
    ) -> (Option<std::process::ExitStatus>, bool) {
        if !self.cleanup_pending {
            return (
                self.reaped_status(),
                matches!(self.leader_state, LeaderState::Reaped(_)),
            );
        }
        let mut lifecycle_ok = !matches!(self.leader_state, LeaderState::Lost);
        if matches!(self.leader_state, LeaderState::Waitable) {
            if self.numeric_phase_is_safe() {
                let outcome = kill_process(self.pid, Signal::TERM);
                if !signal_result(outcome) {
                    lifecycle_ok = false;
                    self.failure = Some(format!("TERM leader: {outcome:?}"));
                }
            } else {
                lifecycle_ok = false;
                self.failure = Some("leader unsafe before TERM".to_owned());
            }
        }
        let started = Instant::now();
        let graceful_timeout = self
            .fallback_grace_ceiling
            .map_or(timeout, |ceiling| timeout.min(ceiling));
        while matches!(self.leader_state, LeaderState::Waitable)
            && remaining_timeout(started, graceful_timeout).is_some()
        {
            match self.observe_leader() {
                LeaderObservation::ExitedUnreaped => break,
                LeaderObservation::Running | LeaderObservation::Unavailable => {
                    std::thread::sleep(CLEANUP_POLL_INTERVAL);
                }
                LeaderObservation::ExternallyReaped | LeaderObservation::Failed => {
                    lifecycle_ok = false;
                    break;
                }
            }
        }
        self.force_leader_and_group(&mut lifecycle_ok);
        self.finish_cleanup(lifecycle_ok)
    }

    fn terminate_forced(&mut self) -> (Option<std::process::ExitStatus>, bool) {
        if !self.cleanup_pending {
            return (
                self.reaped_status(),
                matches!(self.leader_state, LeaderState::Reaped(_)),
            );
        }
        let mut lifecycle_ok = !matches!(self.leader_state, LeaderState::Lost);
        self.force_leader_and_group(&mut lifecycle_ok);
        self.finish_cleanup(lifecycle_ok)
    }

    fn force_leader_and_group(&mut self, lifecycle_ok: &mut bool) {
        if matches!(self.leader_state, LeaderState::Waitable) {
            if self.numeric_phase_is_safe() {
                let outcome = self.child.kill();
                let described = format!("{outcome:?}");
                if !child_kill_result(outcome) {
                    *lifecycle_ok = false;
                    self.failure = Some(format!("kill child: {described}"));
                }
            } else {
                *lifecycle_ok = false;
                self.failure = Some("leader unsafe before kill".to_owned());
            }
        }
        if matches!(self.leader_state, LeaderState::Waitable) {
            if self.numeric_phase_is_safe() {
                let outcome = kill_process_group(self.pid, Signal::KILL);
                if !group_signal_result(outcome) {
                    *lifecycle_ok = false;
                    self.failure = Some(format!("KILL group: {outcome:?}"));
                }
            } else {
                *lifecycle_ok = false;
                self.failure = Some("leader unsafe before group kill".to_owned());
            }
        }
        if matches!(self.leader_state, LeaderState::Waitable) {
            if self.numeric_phase_is_safe() {
                if let Ok(status) = self.child.wait() {
                    self.leader_state = LeaderState::Reaped(status);
                } else {
                    self.leader_state = LeaderState::Lost;
                    *lifecycle_ok = false;
                }
            } else {
                *lifecycle_ok = false;
            }
        }
    }

    fn finish_cleanup(
        &mut self,
        mut lifecycle_ok: bool,
    ) -> (Option<std::process::ExitStatus>, bool) {
        if !lifecycle_ok && self.failure.is_none() {
            self.failure = Some("signal or wait".to_owned());
        }
        if !matches!(self.leader_state, LeaderState::Reaped(_)) {
            lifecycle_ok = false;
            self.failure = Some(
                match self.leader_state {
                    LeaderState::Waitable => "leader still waitable",
                    LeaderState::Lost => "leader lost",
                    LeaderState::Reaped(_) => unreachable!(),
                }
                .to_owned(),
            );
        }
        if !self
            .files
            .containment
            .terminate_all(scaled(CONTAINMENT_TIMEOUT))
            .is_success()
        {
            lifecycle_ok = false;
            self.failure = Some("containment sweep".to_owned());
        }
        self.cleanup_pending = false;
        (self.reaped_status(), lifecycle_ok)
    }

    /// Why the last cleanup failed, when it did.
    fn failure(&self) -> Option<String> {
        self.failure.clone()
    }
}

impl CleanupOutcome {
    const fn from_results(lifecycle_ok: bool, filesystem_ok: bool) -> Self {
        match (lifecycle_ok, filesystem_ok) {
            (true, true) => Self::Complete,
            (false, true) => Self::LifecycleFailed,
            (true, false) => Self::FilesystemFailed,
            (false, false) => Self::LifecycleAndFilesystemFailed,
        }
    }
}

impl Drop for Lifecycle {
    fn drop(&mut self) {
        if self.cleanup_pending {
            let _ = self.force_cleanup();
        }
    }
}

struct StartupGuard {
    lifecycle: Option<Lifecycle>,
}

impl StartupGuard {
    fn new(lifecycle: Lifecycle) -> Self {
        Self {
            lifecycle: Some(lifecycle),
        }
    }

    fn lifecycle(&self) -> Option<&Lifecycle> {
        self.lifecycle.as_ref()
    }

    fn lifecycle_mut(&mut self) -> Option<&mut Lifecycle> {
        self.lifecycle.as_mut()
    }

    fn disarm(mut self) -> Option<Lifecycle> {
        self.lifecycle.take()
    }
}

impl Drop for StartupGuard {
    fn drop(&mut self) {
        if let Some(mut lifecycle) = self.lifecycle.take() {
            let _ = lifecycle.force_cleanup();
        }
    }
}

async fn server_startup_failure(
    server: &Server,
    primary: TestServerErrorKind,
) -> Result<TestServer, TestServerError> {
    let kind = if server.shutdown().await.is_err() {
        TestServerErrorKind::ShutdownFailed
    } else {
        primary
    };
    Err(TestServerError::new(kind))
}

async fn startup_timeout(
    server: &Server,
    startup: StartupGuard,
) -> Result<TestServer, TestServerError> {
    let executor_failed = server.shutdown().await.is_err();
    let mut lifecycle = startup
        .disarm()
        .ok_or_else(|| TestServerError::new(TestServerErrorKind::CleanupFailed))?;
    let status = match lifecycle.force_cleanup_status() {
        Ok(status) => status,
        Err(CleanupOutcome::LifecycleFailed | CleanupOutcome::LifecycleAndFilesystemFailed) => {
            return Err(TestServerError::new(TestServerErrorKind::ShutdownFailed));
        }
        Err(CleanupOutcome::FilesystemFailed) if executor_failed => {
            return Err(TestServerError::new(TestServerErrorKind::ShutdownFailed));
        }
        Err(CleanupOutcome::FilesystemFailed | CleanupOutcome::Complete) => {
            return Err(TestServerError::new(TestServerErrorKind::CleanupFailed));
        }
    };
    if executor_failed {
        return Err(TestServerError::new(TestServerErrorKind::ShutdownFailed));
    }
    if std::os::unix::process::ExitStatusExt::signal(&status) != Some(9) {
        return Err(TestServerError::new(TestServerErrorKind::DaemonExited));
    }
    Err(TestServerError::new(TestServerErrorKind::StartupTimedOut))
}

async fn startup_failure(
    server: &Server,
    startup: StartupGuard,
    primary: TestServerErrorKind,
) -> Result<TestServer, TestServerError> {
    let executor_failed = server.shutdown().await.is_err();
    let lifecycle = startup
        .disarm()
        .ok_or_else(|| TestServerError::new(TestServerErrorKind::CleanupFailed))?;
    cleanup_startup(lifecycle, primary, executor_failed)
}

fn cleanup_startup(
    mut lifecycle: Lifecycle,
    primary: TestServerErrorKind,
    executor_failed: bool,
) -> Result<TestServer, TestServerError> {
    let kind = match lifecycle.force_cleanup() {
        CleanupOutcome::Complete | CleanupOutcome::FilesystemFailed if executor_failed => {
            TestServerErrorKind::ShutdownFailed
        }
        CleanupOutcome::Complete => primary,
        CleanupOutcome::LifecycleFailed | CleanupOutcome::LifecycleAndFilesystemFailed => {
            TestServerErrorKind::ShutdownFailed
        }
        CleanupOutcome::FilesystemFailed => TestServerErrorKind::CleanupFailed,
    };
    Err(TestServerError::new(kind))
}

fn open_directory(path: &Path) -> rustix::io::Result<OwnedFd> {
    openat(
        rustix::fs::CWD,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

fn open_owned_directory(parent: &OwnedFd, name: &OsStr) -> rustix::io::Result<OwnedFd> {
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

fn metadata_matches_fd(metadata: &fs::Metadata, fd: &OwnedFd) -> bool {
    let Ok(cloned) = fd.try_clone() else {
        return false;
    };
    let Ok(found) = fs::File::from(cloned).metadata() else {
        return false;
    };
    metadata.dev() == found.dev() && metadata.ino() == found.ino()
}

fn same_metadata_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn same_fd_identity(left: &OwnedFd, right: &OwnedFd) -> bool {
    let Ok(left) = fstat(left) else {
        return false;
    };
    let Ok(right) = fstat(right) else {
        return false;
    };
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn retain_setup_failure(temporary: tempfile::TempDir) -> TestServerError {
    let _ = temporary.keep();
    TestServerError::new(TestServerErrorKind::FilesystemSetupFailed)
}

fn unlink_fixed(directory: &OwnedFd, name: &OsStr) -> bool {
    match unlinkat(directory, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => true,
        Err(_) => false,
    }
}

async fn readiness_probe(server: &Server) -> Result<Option<u32>, ()> {
    let output = server
        .cmd(Command::new("display-message").arg("-p").arg("#{pid}"))
        .await
        .map_err(|_| ())?;
    if !output.success() {
        return Ok(None);
    }
    let value = output.stdout_utf8().map_err(|_| ())?.trim();
    value.parse::<u32>().map(Some).map_err(|_| ())
}

fn remaining_timeout(started: Instant, timeout: Duration) -> Option<Duration> {
    timeout.checked_sub(started.elapsed())
}

async fn readiness_with_timeout(
    server: &Server,
    timeout: Duration,
) -> Result<Result<Option<u32>, ()>, ()> {
    if timeout == Duration::MAX {
        Ok(readiness_probe(server).await)
    } else {
        tokio::time::timeout(timeout, readiness_probe(server))
            .await
            .map_err(|_| ())
    }
}

fn signal_result(result: rustix::io::Result<()>) -> bool {
    matches!(result, Ok(()) | Err(Errno::SRCH))
}

/// Whether a sweep of the leader's process group did all it could.
///
/// `ESRCH` means the group is already gone, which is the outcome this is
/// trying to reach.
///
/// `EPERM` is accepted only away from Linux, and only here. macOS returns it
/// for the leader's own group once the leader has exited -- observed on every
/// fixture shutdown in CI, with the leader killed and reaped successfully in
/// the same cleanup, so the daemon is gone and only this sweep disagrees.
/// There is nothing further a caller can do about a group the kernel will not
/// let it signal, and the leader is handled separately either way. On Linux
/// the same errno would be a real permission bug worth failing on, so it is
/// not accepted there.
fn group_signal_result(result: rustix::io::Result<()>) -> bool {
    #[cfg(target_os = "linux")]
    {
        signal_result(result)
    }
    #[cfg(not(target_os = "linux"))]
    {
        matches!(result, Ok(()) | Err(Errno::SRCH) | Err(Errno::PERM))
    }
}

fn child_kill_result(result: std::io::Result<()>) -> bool {
    match result {
        Ok(()) => true,
        Err(error) => error.raw_os_error() == Some(Errno::SRCH.raw_os_error()),
    }
}

#[cfg(not(any(
    target_os = "cygwin",
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
fn leader_exited_unreaped(pid: Pid) -> LeaderObservation {
    use rustix::process::{WaitId, WaitIdOptions, waitid};

    loop {
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(_)) => return LeaderObservation::ExitedUnreaped,
            Ok(None) => return LeaderObservation::Running,
            Err(Errno::INTR) => {}
            Err(Errno::CHILD) => return LeaderObservation::ExternallyReaped,
            Err(_) => return LeaderObservation::Failed,
        }
    }
}

#[cfg(any(
    target_os = "cygwin",
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
))]
fn leader_exited_unreaped(_pid: Pid) -> LeaderObservation {
    LeaderObservation::Unavailable
}

fn socket_path_fits_tmux(path: &Path) -> bool {
    let bytes = path.as_os_str().as_bytes();
    if bytes.contains(&0) {
        return false;
    }
    let mut sentinel = bytes.to_vec();
    sentinel.push(b'x');
    rustix::net::SocketAddrUnix::new(PathBuf::from(OsString::from_vec(sentinel))).is_ok()
}

#[cfg(test)]
mod tests {

    /// A deadline widens for a loaded machine and never narrows.
    ///
    /// The fixture's five seconds bound a tmux that starts with a core to
    /// spare. Under a machine running several times its cores in work they
    /// stop bounding startup and start deciding the result, which is the
    /// failure `design.md` names and then leaves to a constant. A scale of
    /// less than one would push it the wrong way, so it is refused rather
    /// than honoured: nothing here is trying to make a fixture fail sooner.
    #[test]
    fn a_timeout_scale_only_ever_widens() {
        use super::{parse_timeout_scale, scaled};

        for (given, expected) in [
            (None, 1.0),
            (Some("2"), 2.0),
            (Some("  3.5  "), 3.5),
            // Anything that is not a number is a typo, not an instruction.
            (Some(""), 1.0),
            (Some("later"), 1.0),
            (Some("NaN"), 1.0),
            (Some("inf"), 1.0),
            // Narrowing is refused, not obeyed.
            (Some("0"), 1.0),
            (Some("0.25"), 1.0),
            (Some("-4"), 1.0),
        ] {
            assert!(
                (parse_timeout_scale(given) - expected).abs() < f64::EPSILON,
                "{given:?} should read as {expected}, got {}",
                parse_timeout_scale(given),
            );
        }

        // Unset, the deadline is exactly what it was before this existed.
        assert_eq!(scaled(Duration::from_secs(5)), Duration::from_secs(5));
    }

    use std::ffi::OsString;
    use std::fs;
    use std::io::Write as _;
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::os::unix::process::CommandExt as _;
    use std::path::PathBuf;
    use std::process::{Command as ProcessCommand, Stdio};
    use std::time::{Duration, Instant};

    use rustix::io::Errno;
    use rustix::process::{Pid, test_kill_process};
    #[cfg(target_os = "linux")]
    use rustix::process::{Signal, WaitOptions, getpgid, kill_process, waitpid};

    use super::{
        CleanupOutcome, LeaderObservation, Lifecycle, OwnedFiles, TestServerBuilder,
        TestServerErrorKind, scaled, socket_path_fits_tmux,
    };

    #[cfg(target_os = "linux")]
    const CONTAINMENT_FAILURE_CHILD: &str = "LIBTMUX_CONTAINMENT_FAILURE_CHILD";
    #[cfg(target_os = "linux")]
    struct ChildGuard {
        child: std::process::Child,
    }

    #[cfg(target_os = "linux")]
    impl ChildGuard {
        fn is_alive(&mut self) -> bool {
            matches!(self.child.try_wait(), Ok(None))
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[tokio::test]
    async fn never_observe_fallback_cleans_an_exited_leaders_group() {
        let root = tempfile::tempdir().expect("fake executable directory is created");
        let executable = root.path().join("tmux");
        let pids = root.path().join("pids");
        let quoted_pids = format!("'{}'", pids.to_string_lossy().replace('\'', "'\\''"));
        let script = format!(
            "#!/bin/sh\n\
             if [ \"${{1-}}\" = '__libtmux_fixture_ready__' ]; then exit 0; fi\n\
             for argument in \"$@\"; do\n\
               if [ \"$argument\" = '-D' ]; then\n\
                 (trap '' TERM; sleep 86400 & wait) &\n\
                 helper=$!\n\
                 printf '%s\\n%s\\n' \"$$\" \"$helper\" > {quoted_pids}.new\n\
                 mv {quoted_pids}.new {quoted_pids}\n\
                 exit 23\n\
               fi\n\
             done\n\
             exit 1\n",
        );
        let mut file = fs::File::create(&executable).expect("fake executable is created");
        file.write_all(script.as_bytes())
            .expect("fake executable is written");
        file.sync_all().expect("fake executable is durable");
        drop(file);
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("fake executable is executable");
        let executable_deadline = Instant::now() + scaled(Duration::from_secs(5));
        loop {
            match ProcessCommand::new(&executable)
                .arg("__libtmux_fixture_ready__")
                .status()
            {
                Ok(status) => {
                    assert!(status.success(), "fake executable readiness failed");
                    break;
                }
                Err(error)
                    if error.raw_os_error() == Some(Errno::TXTBSY.raw_os_error())
                        && Instant::now() < executable_deadline =>
                {
                    std::thread::sleep(super::CLEANUP_POLL_INTERVAL);
                }
                Err(error) => panic!("fake executable readiness failed: {error}"),
            }
        }

        // The subject here is the daemon-exited path, not the clock. A
        // lifecycle timeout of the same magnitude as the observer interval
        // makes the two race, and on a loaded machine the clock wins: the
        // failure reads `StartupTimedOut`, which is a different path through
        // the code and says nothing about the one under test. The ceiling is
        // therefore far above the interval. It costs nothing, because a
        // daemon that exits is noticed when it exits.
        let error = TestServerBuilder::new()
            .tmux_executable(&executable)
            .lifecycle_timeout(Duration::from_secs(5))
            .start_with_leader_observer(
                |_| LeaderObservation::Unavailable,
                Some(Duration::from_millis(50)),
            )
            .await
            .expect_err("exited fallback leader is rejected");
        assert_eq!(error.kind(), TestServerErrorKind::DaemonExited);

        let published = fs::read_to_string(&pids).expect("leader publishes both PIDs");
        let pids = published
            .lines()
            .map(|value| value.parse::<i32>().expect("published PID is numeric"))
            .collect::<Vec<_>>();

        assert_eq!(pids.len(), 2);
        for raw_pid in pids {
            let pid = Pid::from_raw(raw_pid).expect("published PID is nonzero");
            let deadline = Instant::now() + scaled(Duration::from_secs(5));
            while !matches!(test_kill_process(pid), Err(Errno::SRCH)) {
                assert!(
                    Instant::now() < deadline,
                    "fallback process survives cleanup"
                );
                std::thread::sleep(super::CLEANUP_POLL_INTERVAL);
            }
        }
    }

    #[cfg(not(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    )))]
    #[test]
    fn unavailable_observer_caps_oversized_grace_before_group_cleanup() {
        for timeout in [Duration::MAX, Duration::from_secs(100 * 365 * 24 * 60 * 60)] {
            let root = tempfile::tempdir().expect("fallback process directory is created");
            let pids_path = root.path().join("pids");
            let mut child = ProcessCommand::new("sh");
            child
                .arg("-c")
                .arg(
                    "(trap '' TERM; sleep 86400 & wait) &\n\
                     helper=$!\n\
                     printf '%s\\n%s\\n' \"$$\" \"$helper\" > \"$PIDS.new\"\n\
                     mv \"$PIDS.new\" \"$PIDS\"\n\
                     exit 23\n",
                )
                .env("PIDS", &pids_path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0);
            let child = child.spawn().expect("fallback leader starts");
            let leader = Pid::from_child(&child);
            let deadline = Instant::now() + scaled(Duration::from_secs(5));
            while super::leader_exited_unreaped(leader) != LeaderObservation::ExitedUnreaped {
                assert!(
                    Instant::now() < deadline,
                    "fallback leader exits before the observation deadline"
                );
                std::thread::sleep(super::CLEANUP_POLL_INTERVAL);
            }
            let published = fs::read_to_string(&pids_path)
                .expect("fallback leader atomically publishes both PIDs");
            let pids = published
                .lines()
                .map(|value| value.parse::<i32>().expect("published PID is numeric"))
                .collect::<Vec<_>>();
            let files = OwnedFiles::create().expect("owned files are prepared");
            let mut lifecycle = Lifecycle::new_with_leader_observer(
                child,
                files,
                |_| LeaderObservation::Unavailable,
                Some(Duration::from_millis(25)),
            );

            let started = Instant::now();
            assert!(matches!(
                lifecycle.cleanup(timeout),
                CleanupOutcome::Complete
            ));
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "unobservable graceful cleanup uses its fallback ceiling"
            );
            assert_eq!(pids.len(), 2);
            for raw_pid in pids {
                let pid = Pid::from_raw(raw_pid).expect("published PID is nonzero");
                while !matches!(test_kill_process(pid), Err(Errno::SRCH)) {
                    assert!(
                        Instant::now() < deadline,
                        "fallback process survives terminal group cleanup"
                    );
                    std::thread::sleep(super::CLEANUP_POLL_INTERVAL);
                }
            }
        }
    }

    #[test]
    fn setup_refuses_a_substituted_directory_entry() {
        let outside = tempfile::tempdir().expect("outside directory is created");
        let sentinel = outside.path().join("sentinel");
        fs::write(&sentinel, b"preserve").expect("outside sentinel is written");
        let mut renamed = None;
        let result = OwnedFiles::create_with_setup_hook(|directory| {
            let moved = directory.with_extension("renamed");
            fs::rename(directory, &moved).expect("owned directory is renamed");
            symlink(outside.path(), directory).expect("old name is substituted");
            renamed = Some((directory.to_path_buf(), moved));
        });
        let Some(error) = result.err() else {
            unreachable!("substituted setup path is rejected");
        };
        let (substitution, moved) = renamed.expect("hook records both paths");

        assert_eq!(error.kind(), TestServerErrorKind::FilesystemSetupFailed);
        assert_eq!(fs::read(&sentinel).expect("sentinel remains"), b"preserve");
        assert!(!outside.path().join(super::CONFIG_NAME).exists());

        fs::remove_file(substitution).expect("test removes its symlink");
        fs::remove_dir(moved).expect("test removes the retained directory");
    }

    #[test]
    fn unproved_lifecycle_containment_retains_owned_files_on_drop() {
        let mut files = OwnedFiles::create().expect("owned files are prepared");
        let directory = files
            .socket_path
            .parent()
            .expect("socket has an owned directory")
            .to_path_buf();
        let config = files.config_path.clone();

        files.retain_until_contained();
        drop(files);

        assert!(directory.exists(), "unproved lifecycle retains its root");
        assert!(config.exists(), "unproved lifecycle retains its config");
        fs::remove_file(config).expect("test removes the retained config");
        fs::remove_file(directory.join(super::OWNER_NAME))
            .expect("test removes the retained owner record");
        fs::remove_dir(directory).expect("test removes the retained root");
    }

    #[test]
    fn socket_limit_reserves_tmux_c_string_terminator() {
        let mut accepted = vec![b'a'; 1];
        while rustix::net::SocketAddrUnix::new(PathBuf::from(OsString::from_vec(accepted.clone())))
            .is_ok()
        {
            accepted.push(b'a');
        }
        accepted.pop();

        let full_sun_path = PathBuf::from(OsString::from_vec(accepted));
        let tmux_max = PathBuf::from(OsString::from_vec(
            full_sun_path.as_os_str().as_encoded_bytes()
                [..full_sun_path.as_os_str().as_encoded_bytes().len() - 1]
                .to_vec(),
        ));

        assert!(!socket_path_fits_tmux(&full_sun_path));
        assert!(socket_path_fits_tmux(&tmux_max));

        let with_nul = PathBuf::from(OsString::from_vec(b"socket\0tail".to_vec()));
        assert!(!socket_path_fits_tmux(&with_nul));
    }

    #[test]
    fn final_wait_disarms_lifecycle_before_drop() {
        let files = OwnedFiles::create().expect("owned files are created");
        let mut command = ProcessCommand::new("sh");
        command.arg("-c").arg("exec sleep 86400").process_group(0);
        let child = command.spawn().expect("child starts in its own group");
        let mut lifecycle = Lifecycle::new(child, files);

        assert!(matches!(
            lifecycle.force_cleanup(),
            CleanupOutcome::Complete
        ));
        assert!(
            lifecycle.numeric_signaling_retired(),
            "a reaped lifecycle cannot signal its old group"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn externally_reaped_leader_does_not_signal_retired_process_group() {
        let files = OwnedFiles::create().expect("owned files are created");
        let directory = files
            .socket_path
            .parent()
            .expect("owned socket has a directory")
            .to_path_buf();
        let config = files.config_path.clone();
        let mut leader = ProcessCommand::new("sh");
        leader
            .arg("-c")
            .arg("exec sleep 86400")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let child = leader.spawn().expect("leader starts in its own group");
        let leader = Pid::from_child(&child);
        let mut helper = ProcessCommand::new("sh");
        helper
            .arg("-c")
            .arg("exec sleep 86400")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(leader.as_raw_pid());
        let mut helper = ChildGuard {
            child: helper
                .spawn()
                .expect("helper joins the leader process group"),
        };
        assert_eq!(
            getpgid(Some(Pid::from_child(&helper.child))).expect("helper has a process group"),
            leader,
        );
        let mut lifecycle = Lifecycle::new(child, files);

        kill_process(leader, Signal::KILL).expect("external owner kills the leader");
        let deadline = Instant::now() + scaled(Duration::from_secs(5));
        loop {
            match waitpid(Some(leader), WaitOptions::NOHANG) {
                Ok(Some((_pid, _status))) => break,
                Ok(None) | Err(Errno::INTR) if Instant::now() < deadline => {
                    std::thread::sleep(super::CLEANUP_POLL_INTERVAL);
                }
                outcome => panic!("external owner could not reap the leader: {outcome:?}"),
            }
        }

        let outcome = lifecycle.force_cleanup();

        assert!(matches!(
            outcome,
            CleanupOutcome::LifecycleAndFilesystemFailed
        ));
        let survived_cleanup = helper.is_alive();
        drop(lifecycle);
        let survived_drop = helper.is_alive();
        let root_retained = directory.exists();
        let config_retained = config.exists();

        drop(helper);
        fs::remove_file(config).expect("test removes the retained config");
        fs::remove_dir_all(directory).expect("test removes the retained root");

        assert!(
            survived_cleanup,
            "lost child ownership retires numeric process-group signaling"
        );
        assert!(
            survived_drop,
            "lifecycle drop cannot rearm a lost process group"
        );
        assert!(root_retained, "lost ownership retains its root");
        assert!(config_retained, "lost ownership retains its config");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn containment_failure_cannot_rearm_a_reaped_leader() {
        if std::env::var_os(CONTAINMENT_FAILURE_CHILD).is_some() {
            containment_failure_cannot_rearm_a_reaped_leader_inner();
            return;
        }

        let executable = std::env::current_exe().expect("test executable path is available");
        let status = ProcessCommand::new("sh")
            .arg("-c")
            .arg(
                "ulimit -n 64; exec \"$1\" --exact \
                 test::tests::containment_failure_cannot_rearm_a_reaped_leader --nocapture",
            )
            .arg("sh")
            .arg(executable)
            .env(CONTAINMENT_FAILURE_CHILD, "1")
            .status()
            .expect("isolated containment-failure test starts");
        assert!(status.success(), "isolated containment-failure test passes");
    }

    #[cfg(target_os = "linux")]
    fn containment_failure_cannot_rearm_a_reaped_leader_inner() {
        use rustix::process::{WaitOptions, waitpid};

        let files = OwnedFiles::create().expect("owned files are created");
        let directory = files
            .socket_path
            .parent()
            .expect("owned socket has a directory")
            .to_path_buf();
        let config = files.config_path.clone();
        let mut command = ProcessCommand::new("sh");
        command.arg("-c").arg("exec sleep 86400").process_group(0);
        let child = command.spawn().expect("child starts in its own group");
        let leader = Pid::from_child(&child);
        let mut lifecycle = Lifecycle::new(child, files);

        let mut descriptors = Vec::new();
        loop {
            match fs::File::open("/dev/null") {
                Ok(descriptor) => descriptors.push(descriptor),
                Err(error) if error.raw_os_error() == Some(Errno::MFILE.raw_os_error()) => {
                    break;
                }
                Err(error) => panic!("descriptor exhaustion failed: {error}"),
            }
        }
        let Err(scan_error) = fs::read_dir("/proc") else {
            panic!("descriptor exhaustion must prevent containment scanning");
        };
        assert_eq!(scan_error.raw_os_error(), Some(Errno::MFILE.raw_os_error()));

        let outcome = lifecycle.force_cleanup();
        drop(descriptors);

        assert!(matches!(
            waitpid(Some(leader), WaitOptions::NOHANG),
            Err(Errno::CHILD)
        ));
        assert!(matches!(
            outcome,
            CleanupOutcome::LifecycleAndFilesystemFailed
        ));
        assert!(
            lifecycle.numeric_signaling_retired(),
            "a successful wait permanently retires numeric leader signaling"
        );

        drop(lifecycle);
        assert!(directory.exists(), "failed containment retains its root");
        assert!(config.exists(), "failed containment retains its config");
        fs::remove_file(config).expect("test removes the retained config");
        fs::remove_dir_all(directory).expect("test removes the retained root");
    }
}

/// Poll until a condition holds, or the deadline passes.
///
/// tmux applies most changes asynchronously, so a test that reads straight
/// back can observe the state before the change. This waits for the state the
/// test is about rather than sleeping a fixed amount, which is both faster and
/// not tied to how loaded the machine is.
///
/// # Errors
///
/// Returns [`RetryTimeout`] when the condition did not hold before the
/// deadline. The condition's own errors are its business; return `false` to
/// keep waiting.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use std::time::Duration;
/// use libtmux::test::retry_until;
///
/// let mut polls = 0;
/// retry_until(Duration::from_secs(5), async || {
///     polls += 1;
///     polls >= 3
/// })
/// .await?;
/// assert_eq!(polls, 3);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
pub async fn retry_until(
    within: Duration,
    mut condition: impl AsyncFnMut() -> bool,
) -> Result<(), RetryTimeout> {
    let deadline = Instant::now() + within;
    loop {
        if condition().await {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(RetryTimeout { waited: within });
        }
        // Sleeping rather than yielding, because the condition almost always
        // waits on tmux or on another process. A task that yields keeps its
        // worker thread and competes with whatever it is waiting for, so on a
        // busy machine it makes its own deadline harder to meet. This is a
        // public helper, so every caller would inherit that.
        tokio::time::sleep(RETRY_POLL_INTERVAL).await;
    }
}

/// How long [`retry_until`] waits before testing its condition again.
const RETRY_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// A condition did not hold before its deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryTimeout {
    waited: Duration,
}

impl RetryTimeout {
    /// Return how long the condition was given.
    #[must_use]
    pub const fn waited(self) -> Duration {
        self.waited
    }
}

impl fmt::Display for RetryTimeout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "condition did not hold within {:?}", self.waited)
    }
}

impl std::error::Error for RetryTimeout {}

/// Counter behind [`unique_name`], so two names never collide in a process.
static NAME_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Build a name no other caller in this process will produce.
///
/// The name combines the prefix, the process id, and a counter. Unlike a
/// random name it needs no collision check and no random source, and unlike a
/// bare counter it does not collide between concurrent test processes sharing
/// one server.
///
/// # Examples
///
/// ```
/// use libtmux::test::unique_name;
///
/// let first = unique_name("libtmux");
/// let second = unique_name("libtmux");
/// assert_ne!(first, second);
/// assert!(first.starts_with("libtmux-"));
/// ```
#[must_use]
pub fn unique_name(prefix: &str) -> String {
    let count = NAME_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    format!("{prefix}-{}-{count}", std::process::id())
}

/// Kill tmux servers left behind by fixtures that never cleaned up.
///
/// A [`TestServer`] removes its own daemon and directory. One whose process
/// was killed mid-run cannot, and the daemon it left keeps running: each
/// holds a pseudo-terminal for every pane it has, and a system has a few
/// thousand. Enough abandoned runs and the next `fork` fails with `No space
/// left on device`, which reads as a bug in whatever ran next.
///
/// Only this crate's own fixtures are considered: a directory under
/// `/tmp/libtmux-rs-test`, owned by this user, holding a socket where a
/// fixture puts one. A tmux server started any other way -- including one belonging
/// to another libtmux on the same machine -- is never touched, so this cannot
/// reach a real session.
///
/// Abandonment is read from the process that made the fixture, not guessed
/// from a timestamp: each one records its owner, and a fixture is only reaped
/// once that process is gone. A recycled process id makes this skip a
/// leftover rather than reap a live one, which is the direction to fail in.
/// `older_than` is a second guard on top, for a fixture whose owner id has
/// been reused; pass `Duration::ZERO` to rely on the owner check alone.
///
/// # Errors
///
/// Returns an error when the temporary root cannot be read. A directory that
/// cannot be reaped is skipped rather than failing the sweep, because one
/// unreadable leftover should not stop the rest being cleared.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::time::Duration;
///
/// // Nothing this old is in use, so nothing a running test owns is at risk.
/// let reaped = libtmux::test::reap_abandoned_servers(Duration::from_secs(3600))?;
/// println!("reaped {} abandoned fixtures", reaped.len());
/// # Ok(())
/// # }
/// ```
pub fn reap_abandoned_servers(older_than: Duration) -> Result<Vec<PathBuf>, TestServerError> {
    let entries = match fs::read_dir(FIXTURE_ROOT) {
        Ok(entries) => entries,
        // No root means no fixture has ever run here, which is not a failure.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => {
            return Err(TestServerError::new(
                TestServerErrorKind::FilesystemSetupFailed,
            ));
        }
    };

    let mut reaped = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_abandoned_fixture(&path, older_than) {
            continue;
        }
        // Killing the daemon first means the socket it owns is gone before
        // the directory holding it is, so a half-swept fixture is never left
        // reachable.
        let socket = path.join(SOCKET_NAME);
        let _ = ProcessCommand::new("tmux")
            .arg("-S")
            .arg(&socket)
            .arg("kill-server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if fs::remove_dir_all(&path).is_ok() {
            reaped.push(path);
        }
    }

    Ok(reaped)
}

/// Whether one path is a fixture directory old enough to reap.
fn is_abandoned_fixture(path: &Path, older_than: Duration) -> bool {
    if owner_is_running(path) {
        return false;
    }

    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    // A fixture makes its directory 0700 and its own. Anything else under
    // this name is somebody else's and is left alone.
    if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
        return false;
    }
    if metadata.uid() != rustix::process::getuid().as_raw() {
        return false;
    }
    if !path.join(SOCKET_NAME).exists() {
        return false;
    }

    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= older_than)
}

/// Whether the process that created a fixture is still there.
///
/// A fixture with no recorded owner is not reaped: it is either older than
/// this bookkeeping or not a fixture at all, and neither is worth guessing
/// about when the cost of being wrong is killing a live server.
fn owner_is_running(path: &Path) -> bool {
    let Ok(recorded) = fs::read_to_string(path.join(OWNER_NAME)) else {
        return true;
    };
    let Ok(owner) = recorded.trim().parse::<i32>() else {
        return true;
    };
    let Some(owner) = Pid::from_raw(owner) else {
        return true;
    };

    // Signal zero asks the kernel whether the process could be signalled,
    // without sending anything. `Err(ESRCH)` is the one answer that means
    // gone; a permission error means it is there and owned by somebody else.
    !matches!(test_kill_process(owner), Err(Errno::SRCH))
}
