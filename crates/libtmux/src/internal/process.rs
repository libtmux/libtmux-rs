use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "control-mode")]
use std::io;
#[cfg(feature = "control-mode")]
use std::process::Stdio;
#[cfg(feature = "control-mode")]
use std::sync::{Mutex, MutexGuard};

use rustix::process::{Pid, Signal, kill_process_group};
use tokio::process::Command as TokioCommand;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

#[cfg(feature = "control-mode")]
use tokio::process::{Child, ChildStdin, ChildStdout};
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
        command.args(arguments).kill_on_drop(true).process_group(0);
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
}

impl ProcessGroupGuard {
    pub(crate) fn new(child_id: Option<u32>) -> Self {
        let process_group = child_id
            .and_then(|value| i32::try_from(value).ok())
            .and_then(Pid::from_raw);
        Self {
            process_group,
            armed: true,
        }
    }

    #[cfg(feature = "test-support")]
    pub(crate) const fn is_armed(&self) -> bool {
        self.armed
    }

    pub(crate) fn signal(&self) {
        if self.armed {
            if let Some(process_group) = self.process_group {
                let _ = kill_process_group(process_group, Signal::KILL);
            }
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.signal();
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
    ) -> Result<PersistentReservation, Error> {
        let permit = self
            .admission
            .acquire(request_id, &command, deadline)
            .await?;
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

    pub(crate) async fn shutdown(&self) {
        {
            let mut lifecycle = lock_persistent(&self.shared);
            lifecycle.accepting = false;
            self.shared.stopped.send_replace(true);
        }
        self.admission.close();

        loop {
            let notified = self.shared.empty.notified();
            if lock_persistent(&self.shared).active == 0 {
                return;
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

#[cfg(feature = "control-mode")]
pub(crate) struct PersistentChild {
    // The group guard must drop while the unreaped leader still anchors its PGID.
    process_group: ProcessGroupGuard,
    child: Child,
    stopped: watch::Receiver<bool>,
    request_id: RequestId,
    command: CommandSummary,
    // Admission remains active until process cleanup has finished.
    _registration: PersistentRegistration,
}

#[cfg(feature = "control-mode")]
impl PersistentChild {
    pub(crate) fn spawn(
        launch: &LaunchContext,
        request: &CommandRequest,
        reservation: PersistentReservation,
    ) -> Result<Self, Error> {
        validate_request(launch, request)?;
        let mut command = launch.command(request.argv());
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = command.spawn().map_err(Error::control_mode)?;
        let process_group = ProcessGroupGuard::new(child.id());

        Ok(Self {
            process_group,
            child,
            stopped: reservation.stopped,
            request_id: request.request_id(),
            command: request.summary().clone(),
            _registration: reservation.registration,
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

    pub(crate) fn shutdown_error(&self) -> Error {
        Error::executor_shutdown(self.request_id.get(), self.command.clone())
    }

    pub(crate) async fn terminate(&mut self) -> Result<(), Error> {
        self.process_group.signal();
        let _ = self.child.start_kill();
        loop {
            match self.child.wait().await {
                Ok(_) => {
                    self.process_group.disarm();
                    return Ok(());
                }
                Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
                Err(source) => return Err(Error::control_mode(source)),
            }
        }
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
