use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "control-mode")]
use std::process::Stdio;
#[cfg(feature = "control-mode")]
use std::sync::{Mutex, MutexGuard};

use rustix::process::{Pid, Signal, kill_process_group};
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

#[cfg(feature = "control-mode")]
use tokio::process::{ChildStdin, ChildStdout};
#[cfg(feature = "control-mode")]
use tokio::sync::{Notify, watch};

use crate::Error;
use crate::command::{CommandRequest, CommandSummary, RequestId};
#[cfg(feature = "control-mode")]
use crate::limits::ControlClientLimits;

#[derive(Clone)]
pub(crate) struct LaunchContext {
    executable: OsString,
    current_dir: Option<PathBuf>,
    environment: Vec<(OsString, Option<OsString>)>,
}

impl LaunchContext {
    pub(crate) fn new(executable: impl Into<OsString>) -> Self {
        Self {
            executable: executable.into(),
            current_dir: None,
            environment: Vec::new(),
        }
    }

    pub(crate) fn with_current_dir(mut self, current_dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(current_dir.into());
        self
    }

    pub(crate) fn with_environment(
        mut self,
        key: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        self.environment.push((key.into(), Some(value.into())));
        self
    }

    pub(crate) fn with_environment_removed(mut self, key: impl Into<OsString>) -> Self {
        self.environment.push((key.into(), None));
        self
    }

    pub(crate) fn command(&self, arguments: &[OsString]) -> TokioCommand {
        let mut command = TokioCommand::new(&self.executable);
        // The group guard owns termination. Tokio's independent drop kill could
        // signal a reused PID after an ECHILD wait error disarms that guard.
        command.args(arguments).kill_on_drop(false).process_group(0);
        if let Some(current_dir) = &self.current_dir {
            command.current_dir(current_dir);
        }
        for (key, value) in &self.environment {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        command
    }

    pub(crate) fn executable(&self) -> &OsStr {
        &self.executable
    }

    pub(crate) fn current_dir(&self) -> Option<&Path> {
        self.current_dir.as_deref()
    }

    pub(crate) fn resolved_executable(&self) -> Option<PathBuf> {
        let working_directory = self.current_dir.as_deref()?;
        if !working_directory.is_absolute() || !working_directory.is_dir() {
            return None;
        }
        let executable = self.executable.as_os_str();
        if executable.as_bytes().is_empty() || executable.as_bytes().contains(&0) {
            return None;
        }

        if executable.as_bytes().contains(&b'/') {
            let configured = Path::new(executable);
            let candidate = if configured.is_absolute() {
                configured.to_path_buf()
            } else {
                working_directory.join(configured)
            };
            return runnable_file(&candidate).then_some(candidate);
        }

        let path = self
            .environment
            .iter()
            .rev()
            .find(|(key, _)| key == "PATH")
            .and_then(|(_, value)| value.as_deref())?;
        if path.as_bytes().contains(&0) {
            return None;
        }
        std::env::split_paths(path).find_map(|directory| {
            let directory = if directory.is_absolute() {
                directory
            } else {
                working_directory.join(directory)
            };
            let candidate = directory.join(executable);
            runnable_file(&candidate).then_some(candidate)
        })
    }

    #[cfg(test)]
    #[allow(
        clippy::option_option,
        reason = "tests distinguish an absent action from removal and assignment"
    )]
    pub(crate) fn environment_value(&self, key: &OsStr) -> Option<Option<&OsStr>> {
        self.environment
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_deref())
    }

    /// Report whether a bare executable resolves through captured `PATH`.
    ///
    /// WSL may return `EIO`, rather than `NotFound`, when it does not.
    pub(crate) fn executable_missing_from_path(&self) -> bool {
        let executable = Path::new(&self.executable);
        if executable.components().count() != 1 {
            return false;
        }

        let path = self
            .environment
            .iter()
            .rev()
            .find(|(key, _)| key == "PATH")
            .map_or_else(|| std::env::var_os("PATH"), |(_, value)| value.clone());
        let Some(path) = path else {
            return true;
        };

        !std::env::split_paths(&path).any(|directory| directory.join(executable).is_file())
    }
}

fn runnable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

pub(crate) fn validate_request(
    launch: &LaunchContext,
    request: &CommandRequest,
) -> Result<(), Error> {
    use std::os::unix::ffi::OsStrExt as _;

    let request_id = request.request_id();
    if launch.executable().as_bytes().contains(&0) {
        return Err(Error::invalid_command_input(
            request_id.get(),
            "tmux executable",
        ));
    }
    for (index, argument) in request.argv().iter().enumerate() {
        if argument.as_os_str().as_bytes().contains(&0) {
            let input = match index.cmp(&request.logical_subcommand_index()) {
                std::cmp::Ordering::Less => "tmux global argument",
                std::cmp::Ordering::Equal => "tmux subcommand",
                std::cmp::Ordering::Greater => "tmux argument",
            };
            return Err(Error::invalid_command_input(request_id.get(), input));
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct ProcessAdmission {
    permits: Arc<Semaphore>,
    limit: usize,
    acquire_timeout: Option<Duration>,
}

impl ProcessAdmission {
    pub(crate) fn new(limit: usize, acquire_timeout: Option<Duration>) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(limit)),
            limit,
            acquire_timeout,
        }
    }

    pub(crate) async fn acquire(
        &self,
        request_id: RequestId,
        command: &CommandSummary,
        deadline: Option<Instant>,
    ) -> Result<OwnedSemaphorePermit, Error> {
        let deadline = match (
            deadline,
            self.acquire_timeout
                .and_then(|timeout| Instant::now().checked_add(timeout)),
        ) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        };
        let acquired = match deadline {
            Some(deadline) => {
                match tokio::time::timeout_at(deadline, Arc::clone(&self.permits).acquire_owned())
                    .await
                {
                    Ok(acquired) => acquired,
                    Err(_) => {
                        return Err(Error::Overloaded {
                            request_id: request_id.get(),
                            command: command.clone(),
                            in_flight: self.limit,
                        });
                    }
                }
            }
            None => Arc::clone(&self.permits).acquire_owned().await,
        };
        acquired.map_err(|_| Error::executor_shutdown(request_id.get(), command.clone()))
    }

    pub(crate) fn close(&self) {
        self.permits.close();
    }
}

pub(crate) struct ProcessGroupGuard {
    process_group: Option<Pid>,
    armed: bool,
    #[cfg(test)]
    cleanup_failure: Option<(CleanupFailureStage, io::ErrorKind)>,
    #[cfg(test)]
    denied_signals: Option<io::ErrorKind>,
}

impl ProcessGroupGuard {
    pub(crate) fn new(child_id: Option<u32>) -> Self {
        let process_group = child_id
            .and_then(|value| i32::try_from(value).ok())
            .and_then(Pid::from_raw);
        Self {
            process_group,
            armed: true,
            #[cfg(test)]
            cleanup_failure: None,
            #[cfg(test)]
            denied_signals: None,
        }
    }

    pub(crate) const fn is_armed(&self) -> bool {
        self.armed
    }

    pub(crate) fn signal(&self) -> io::Result<()> {
        if self.armed
            && let Some(process_group) = self.process_group
        {
            #[cfg(test)]
            if let Some(kind) = self.denied_signals {
                return Err(kind.into());
            }
            retry_signal(|| kill_process_group(process_group, Signal::KILL).map_err(Into::into))
        } else {
            Ok(())
        }
    }

    pub(crate) fn kill_child(&self, child: &mut Child) -> io::Result<()> {
        if self.armed {
            #[cfg(test)]
            if let Some(kind) = self.denied_signals {
                return Err(kind.into());
            }
            retry_signal(|| child.start_kill())
        } else {
            Ok(())
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }

    #[cfg(test)]
    pub(crate) fn refuse_signals_for_test(&mut self, kind: io::ErrorKind) {
        self.denied_signals = Some(kind);
    }

    #[cfg(test)]
    pub(crate) fn fail_cleanup_for_test(
        &mut self,
        stage: CleanupFailureStage,
        kind: io::ErrorKind,
    ) {
        self.cleanup_failure = Some((stage, kind));
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        let _ = self.signal();
    }
}

fn retry_signal(mut signal: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    loop {
        match signal() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) => {
                return Ok(());
            }
            result => return result,
        }
    }
}

pub(crate) async fn terminate_child(
    child: &mut Child,
    process_group: &mut ProcessGroupGuard,
) -> io::Result<()> {
    // A failed group signal must not skip the direct kill or the wait. Once
    // ECHILD disarms the guard, neither path may signal the old PID again.
    let (signal, kill) = if process_group.is_armed() {
        (process_group.signal(), process_group.kill_child(child))
    } else {
        (Ok(()), Ok(()))
    };
    let waited = if kill.is_err() {
        // A known termination failure must not wait for a still-live child.
        // Keep ownership armed so Drop can retry without claiming containment.
        loop {
            match child.try_wait() {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                result => break result.map(|status| status.map(|_| ())),
            }
        }
    } else {
        loop {
            match child.wait().await {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                result => break result.map(|_| Some(())),
            }
        }
    };
    if matches!(waited, Ok(Some(())))
        || waited.as_ref().is_err_and(|error| {
            error.raw_os_error() == Some(rustix::io::Errno::CHILD.raw_os_error())
        })
    {
        process_group.disarm();
    }
    // Inject only after real cleanup so the error-path tests cannot leak a child.
    #[cfg(test)]
    let (signal, kill, waited) = match process_group.cleanup_failure.take() {
        Some((CleanupFailureStage::GroupSignal, kind)) => {
            (Err(io::Error::from(kind)), kill, waited)
        }
        Some((CleanupFailureStage::ChildKill, kind)) => {
            (signal, Err(io::Error::from(kind)), waited)
        }
        Some((CleanupFailureStage::Wait, kind)) => (signal, kill, Err(io::Error::from(kind))),
        None => (signal, kill, waited),
    };
    match cleanup_error(&[
        (
            "process-group signal",
            signal.err().map(|error| error.kind()),
        ),
        ("direct-child signal", kill.err().map(|error| error.kind())),
        ("child wait", waited.err().map(|error| error.kind())),
    ]) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn cleanup_error(stages: &[(&str, Option<io::ErrorKind>)]) -> Option<io::Error> {
    let failures: Vec<_> = stages
        .iter()
        .filter_map(|(stage, kind)| kind.map(|kind| (*stage, kind)))
        .collect();
    let (_, kind) = failures.first()?;
    let message = failures
        .iter()
        .map(|(stage, kind)| format!("{stage}: {kind:?}"))
        .collect::<Vec<_>>()
        .join("; ");
    Some(io::Error::new(*kind, message))
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum CleanupFailureStage {
    GroupSignal,
    ChildKill,
    Wait,
}

#[cfg(test)]
impl CleanupFailureStage {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::GroupSignal => "process-group signal",
            Self::ChildKill => "direct-child signal",
            Self::Wait => "child wait",
        }
    }
}

#[derive(Clone)]
pub(crate) struct CleanupFailure {
    request_id: u64,
    command: CommandSummary,
    kind: io::ErrorKind,
    message: String,
}

impl CleanupFailure {
    pub(crate) fn new(request_id: u64, command: CommandSummary, source: &io::Error) -> Self {
        Self {
            request_id,
            command,
            kind: source.kind(),
            // terminate_child emits only stage names and error kinds.
            message: source.to_string(),
        }
    }

    pub(crate) fn error(&self) -> Error {
        Error::wait_child(
            self.request_id,
            self.command.clone(),
            io::Error::new(self.kind, self.message.clone()),
        )
    }
}

#[cfg(feature = "control-mode")]
#[derive(Clone)]
pub(crate) struct PersistentClients {
    shared: Arc<PersistentShared>,
    admission: ProcessAdmission,
}

#[cfg(feature = "control-mode")]
struct PersistentShared {
    lifecycle: Mutex<PersistentLifecycle>,
    stopped: watch::Sender<bool>,
    empty: Notify,
}

#[cfg(feature = "control-mode")]
struct PersistentLifecycle {
    accepting: bool,
    active: usize,
    cleanup_failure: Option<CleanupFailure>,
}

#[cfg(feature = "control-mode")]
impl PersistentClients {
    pub(crate) fn new(limits: ControlClientLimits) -> Self {
        let (stopped, _) = watch::channel(false);
        Self {
            shared: Arc::new(PersistentShared {
                lifecycle: Mutex::new(PersistentLifecycle {
                    accepting: true,
                    active: 0,
                    cleanup_failure: None,
                }),
                stopped,
                empty: Notify::new(),
            }),
            admission: ProcessAdmission::new(limits.max_clients, limits.acquire_timeout),
        }
    }

    pub(crate) async fn reserve(
        &self,
        request_id: RequestId,
        command: CommandSummary,
        deadline: Option<Instant>,
        cancellation: &crate::CancellationToken,
    ) -> Result<PersistentReservation, Error> {
        let (wake, mut cancelled) = watch::channel(false);
        let _registration = cancellation.on_cancel(move || {
            wake.send_replace(true);
        });
        if cancellation.is_cancelled() {
            return Err(Error::cancelled(request_id.get(), command));
        }
        let permit = tokio::select! {
            biased;
            _ = cancelled.changed() => return Err(Error::cancelled(request_id.get(), command)),
            permit = self.admission.acquire(request_id, &command, deadline) => permit?,
        };
        let stopped = {
            let mut lifecycle = lock_persistent(&self.shared);
            if !lifecycle.accepting {
                return Err(Error::executor_shutdown(request_id.get(), command));
            }
            lifecycle.active += 1;
            self.shared.stopped.subscribe()
        };

        Ok(PersistentReservation {
            stopped,
            registration: PersistentRegistration {
                shared: Arc::clone(&self.shared),
                _permit: permit,
            },
        })
    }

    pub(crate) async fn shutdown(&self) -> Result<(), Error> {
        {
            let mut lifecycle = lock_persistent(&self.shared);
            lifecycle.accepting = false;
            self.shared.stopped.send_replace(true);
        }
        self.admission.close();

        loop {
            let notified = self.shared.empty.notified();
            {
                let lifecycle = lock_persistent(&self.shared);
                if lifecycle.active == 0 {
                    return match &lifecycle.cleanup_failure {
                        Some(failure) => Err(failure.error()),
                        None => Ok(()),
                    };
                }
            }
            notified.await;
        }
    }
}

#[cfg(feature = "control-mode")]
pub(crate) struct PersistentReservation {
    stopped: watch::Receiver<bool>,
    registration: PersistentRegistration,
}

#[cfg(feature = "control-mode")]
struct PersistentRegistration {
    shared: Arc<PersistentShared>,
    _permit: OwnedSemaphorePermit,
}

#[cfg(feature = "control-mode")]
impl Drop for PersistentRegistration {
    fn drop(&mut self) {
        let mut lifecycle = lock_persistent(&self.shared);
        lifecycle.active -= 1;
        let empty = lifecycle.active == 0;
        drop(lifecycle);
        if empty {
            self.shared.empty.notify_waiters();
        }
    }
}

/// Removes this control client's pid from the shared owned set on drop.
///
/// Lives on [`PersistentChild`] rather than on either half a caller splits
/// it into ([`crate::control::ControlSender`], [`crate::control::ControlEvents`]):
/// either can be dropped while the other keeps the connection, and the
/// underlying `Child` -- what this pid actually names -- lives exactly as
/// long as `PersistentChild` does.
#[cfg(feature = "control-mode")]
struct ControlClientPidGuard {
    pids: Arc<Mutex<std::collections::HashSet<u32>>>,
    pid: u32,
}

#[cfg(feature = "control-mode")]
impl Drop for ControlClientPidGuard {
    fn drop(&mut self) {
        self.pids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.pid);
    }
}

#[cfg(feature = "control-mode")]
pub(crate) struct PersistentChild {
    // The group guard must drop while the unreaped leader still anchors its PGID.
    process_group: ProcessGroupGuard,
    child: Child,
    stopped: watch::Receiver<bool>,
    request_id: RequestId,
    command: CommandSummary,
    // Admission remains active until process cleanup has finished.
    registration: PersistentRegistration,
    terminated: bool,
    cleanup_failure: Option<CleanupFailure>,
    // Removes this pid from the owned set once dropped; its ordering
    // relative to the other fields does not matter, since nothing here reads
    // it back.
    _pid_guard: Option<ControlClientPidGuard>,
}

#[cfg(feature = "control-mode")]
impl PersistentChild {
    pub(crate) fn spawn(
        launch: &LaunchContext,
        request: &CommandRequest,
        reservation: PersistentReservation,
        control_client_pids: Arc<Mutex<std::collections::HashSet<u32>>>,
    ) -> Result<Self, Error> {
        validate_request(launch, request)?;
        if request.cancellation_token().is_cancelled() {
            return Err(Error::cancelled(
                request.request_id().get(),
                request.summary().clone(),
            ));
        }
        let mut command = launch.command(request.argv());
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = command.spawn().map_err(|source| {
            // The same test the subprocess path applies, so a missing tmux is
            // `ExecutableNotFound` whichever path meets it first. Anything else
            // stays a control-mode failure.
            let executable_not_found = (source.kind() == io::ErrorKind::NotFound
                && launch.current_dir().is_none_or(Path::is_dir))
                || launch.executable_missing_from_path();
            if executable_not_found {
                Error::spawn(
                    request.request_id().get(),
                    request.summary().clone(),
                    source,
                    true,
                )
            } else {
                Error::control_mode(source)
            }
        })?;
        let process_group = ProcessGroupGuard::new(child.id());
        // `child.id()` is `None` only once the child has already been
        // reaped, which cannot be true immediately after `spawn` returns.
        let pid_guard = child.id().map(|pid| {
            control_client_pids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(pid);
            ControlClientPidGuard {
                pids: control_client_pids,
                pid,
            }
        });

        Ok(Self {
            process_group,
            child,
            stopped: reservation.stopped,
            request_id: request.request_id(),
            command: request.summary().clone(),
            registration: reservation.registration,
            terminated: false,
            cleanup_failure: None,
            _pid_guard: pid_guard,
        })
    }

    pub(crate) fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    pub(crate) fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub(crate) fn stopped(&self) -> watch::Receiver<bool> {
        self.stopped.clone()
    }

    pub(crate) fn cancelled_error(&self) -> Error {
        Error::cancelled(self.request_id.get(), self.command.clone())
    }

    pub(crate) fn shutdown_error(&self) -> Error {
        Error::executor_shutdown(self.request_id.get(), self.command.clone())
    }

    #[cfg(test)]
    pub(crate) fn fail_cleanup_for_test(
        &mut self,
        stage: CleanupFailureStage,
        kind: io::ErrorKind,
    ) {
        self.process_group.fail_cleanup_for_test(stage, kind);
    }

    fn cleanup_failed(&mut self, source: &io::Error) -> Error {
        let failure = CleanupFailure::new(self.request_id.get(), self.command.clone(), source);
        lock_persistent(&self.registration.shared)
            .cleanup_failure
            .get_or_insert_with(|| failure.clone());
        self.cleanup_failure = Some(failure.clone());
        failure.error()
    }

    pub(crate) async fn terminate(&mut self) -> Result<(), Error> {
        if self.terminated {
            return match &self.cleanup_failure {
                Some(failure) => Err(failure.error()),
                None => Ok(()),
            };
        }
        let result = terminate_child(&mut self.child, &mut self.process_group).await;
        self.terminated = true;
        result.map_err(|source| self.cleanup_failed(&source))
    }
}

#[cfg(feature = "control-mode")]
impl Drop for PersistentChild {
    fn drop(&mut self) {
        // The group guard drops next, while the child still anchors its PGID.
        let _ = self.process_group.kill_child(&mut self.child);
    }
}

#[cfg(feature = "control-mode")]
fn lock_persistent(shared: &PersistentShared) -> MutexGuard<'_, PersistentLifecycle> {
    shared
        .lifecycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::path::{Path, PathBuf};

    use super::LaunchContext;

    fn executable(path: &Path) {
        std::fs::write(path, b"#!/bin/sh\nexit 0\n").expect("fixture executable is written");
        let mut permissions = std::fs::metadata(path)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions).expect("fixture executable is runnable");
    }

    fn launch(executable: impl Into<OsString>, cwd: &Path, path: Option<&OsStr>) -> LaunchContext {
        let launch = LaunchContext::new(executable).with_current_dir(cwd);
        match path {
            Some(path) => launch.with_environment("PATH", path),
            None => launch.with_environment_removed("PATH"),
        }
    }

    #[tokio::test]
    async fn signals_refused_return_without_waiting_for_a_live_child() {
        use std::time::Duration;

        let launch = LaunchContext::new("/bin/sleep");
        let mut child = launch
            .command(&["30".into()])
            .spawn()
            .expect("child starts");
        let mut group = super::ProcessGroupGuard::new(child.id());
        group.refuse_signals_for_test(std::io::ErrorKind::PermissionDenied);
        let observed = tokio::time::timeout(
            Duration::from_millis(150),
            super::terminate_child(&mut child, &mut group),
        )
        .await;
        let still_alive = child.try_wait().expect("child state").is_none();
        let still_owned = group.is_armed();
        group.denied_signals = None;
        super::terminate_child(&mut child, &mut group)
            .await
            .expect("harness reaps owned child");
        let error = observed
            .expect("known signal failures return without waiting")
            .expect_err("both denied signals are reported");
        assert!(
            still_alive,
            "injection must refuse effects, not report a fake signal success"
        );
        assert!(still_owned, "a live child's guard remains armed for Drop");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        let detail = error.to_string();
        assert!(detail.contains("process-group signal: PermissionDenied"));
        assert!(detail.contains("direct-child signal: PermissionDenied"));
    }

    #[test]
    fn signal_retries_interruptions() {
        let mut attempts = 0;
        super::retry_signal(|| {
            attempts += 1;
            if attempts < 3 {
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
            } else {
                Ok(())
            }
        })
        .expect("an interrupted signal is retried");
        assert_eq!(attempts, 3);
    }

    #[test]
    fn signal_accepts_a_missing_process_and_reports_other_errors() {
        super::retry_signal(|| Err(rustix::io::Errno::SRCH.into()))
            .expect("an absent process is already stopped");
        let error = super::retry_signal(|| Err(rustix::io::Errno::PERM.into()))
            .expect_err("a denied signal is not successful cleanup");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn resolver_preserves_absolute_relative_and_symlink_paths() {
        let root = tempfile::tempdir().expect("temporary root");
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).expect("bin exists");
        let target = bin.join("real");
        let link = bin.join("wrapper");
        executable(&target);
        symlink(&target, &link).expect("symlink exists");

        assert_eq!(
            launch(&link, root.path(), None).resolved_executable(),
            Some(link.clone())
        );
        assert_eq!(
            launch("bin/wrapper", root.path(), None).resolved_executable(),
            Some(link)
        );
    }

    #[test]
    fn resolver_searches_captured_path_in_order() {
        let root = tempfile::tempdir().expect("temporary root");
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::create_dir(&first).expect("first exists");
        std::fs::create_dir(&second).expect("second exists");
        std::fs::write(first.join("tmux"), b"not executable").expect("first candidate exists");
        executable(&second.join("tmux"));
        let path = std::env::join_paths([first, second.clone()]).expect("fixture PATH joins");

        assert_eq!(
            launch("tmux", root.path(), Some(&path)).resolved_executable(),
            Some(second.join("tmux"))
        );
    }

    #[test]
    fn resolver_anchors_empty_and_relative_path_entries_to_captured_cwd() {
        let root = tempfile::tempdir().expect("temporary root");
        let relative = root.path().join("relative");
        std::fs::create_dir(&relative).expect("relative bin exists");
        executable(&root.path().join("from-empty"));
        executable(&relative.join("from-relative"));
        let path = OsString::from(":relative");

        assert_eq!(
            launch("from-empty", root.path(), Some(&path)).resolved_executable(),
            Some(root.path().join("from-empty"))
        );
        assert_eq!(
            launch("from-relative", root.path(), Some(&path)).resolved_executable(),
            Some(relative.join("from-relative"))
        );
    }

    #[test]
    fn resolver_preserves_apostrophe_and_newline() {
        let root = tempfile::tempdir().expect("temporary root");
        let name = OsString::from_vec(b"tmux-'line\n-x".to_vec());
        let path = root.path().join(&name);
        executable(&path);

        assert_eq!(
            launch(path.as_os_str(), root.path(), None).resolved_executable(),
            Some(path)
        );
    }

    /// A path is bytes on Linux and valid UTF-8 on macOS.
    ///
    /// APFS and HFS+ reject a name that is not valid UTF-8, so creating the
    /// fixture fails with `EILSEQ` before the resolver is reached. The
    /// invariant still holds there -- a path that cannot exist cannot be
    /// resolved -- so the coverage is skipped rather than weakened, and the
    /// apostrophe and newline above still run everywhere.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn resolver_preserves_non_utf8_bytes() {
        let root = tempfile::tempdir().expect("temporary root");
        let name = OsString::from_vec(b"tmux-\xff".to_vec());
        let path = root.path().join(&name);
        executable(&path);

        assert_eq!(
            launch(path.as_os_str(), root.path(), None).resolved_executable(),
            Some(path)
        );
    }

    #[test]
    fn resolver_fails_closed_for_missing_context_nul_and_nonexecutables() {
        let root = tempfile::tempdir().expect("temporary root");
        let plain = root.path().join("plain");
        std::fs::write(&plain, b"plain").expect("plain file exists");
        let directory = root.path().join("directory");
        std::fs::create_dir(&directory).expect("directory exists");
        let nul = OsString::from_vec(b"tmux\0bad".to_vec());

        assert_eq!(
            launch("tmux", root.path(), None).resolved_executable(),
            None
        );
        assert_eq!(launch(nul, root.path(), None).resolved_executable(), None);
        assert_eq!(
            launch(&plain, root.path(), None).resolved_executable(),
            None
        );
        assert_eq!(
            launch(&directory, root.path(), None).resolved_executable(),
            None
        );

        let vanished = root.path().join("vanished");
        std::fs::create_dir(&vanished).expect("captured cwd exists");
        let path = PathBuf::from("bin");
        std::fs::remove_dir(&vanished).expect("captured cwd vanishes");
        assert_eq!(
            launch("tmux", &vanished, Some(path.as_os_str())).resolved_executable(),
            None
        );
    }
}
