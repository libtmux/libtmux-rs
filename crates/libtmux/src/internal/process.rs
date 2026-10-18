use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::Error;
use crate::command::{CommandRequest, CommandSummary, RequestId};

#[derive(Clone)]
pub(crate) struct LaunchContext {
    executable: OsString,
    current_dir: Option<PathBuf>,
    environment: Vec<(OsString, Option<OsString>)>,
    group_signal_helper: PathBuf,
}

impl LaunchContext {
    pub(crate) fn with_group_signal_helper(mut self, path: PathBuf) -> Self {
        self.group_signal_helper = path;
        self
    }
    pub(crate) fn group_signal_helper(&self) -> &Path {
        &self.group_signal_helper
    }

    pub(crate) fn new(executable: impl Into<OsString>) -> Self {
        Self {
            executable: executable.into(),
            current_dir: None,
            environment: Vec::new(),
            group_signal_helper: PathBuf::from("/usr/bin/kill"),
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

    pub(crate) fn command(&self, arguments: &[OsString]) -> Command {
        let mut command = Command::new(&self.executable);
        command.args(arguments).process_group(0);
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
    shared: Arc<Admission>,
    limit: usize,
    acquire_timeout: Option<Duration>,
}
struct Admission {
    state: Mutex<AdmissionState>,
    changed: Condvar,
}
struct AdmissionState {
    closed: bool,
    active: usize,
    next: u64,
    waiting: VecDeque<u64>,
}
pub(crate) struct OwnedSemaphorePermit(Arc<Admission>);
impl Drop for OwnedSemaphorePermit {
    fn drop(&mut self) {
        let mut state = lock(&self.0.state);
        state.active -= 1;
        self.0.changed.notify_all();
    }
}
impl ProcessAdmission {
    pub(crate) fn new(limit: usize, acquire_timeout: Option<Duration>) -> Self {
        Self {
            shared: Arc::new(Admission {
                state: Mutex::new(AdmissionState {
                    closed: false,
                    active: 0,
                    next: 0,
                    waiting: VecDeque::new(),
                }),
                changed: Condvar::new(),
            }),
            limit: limit.max(1),
            acquire_timeout,
        }
    }
    pub(crate) fn acquire(
        &self,
        request_id: RequestId,
        command: &CommandSummary,
        deadline: Option<Instant>,
        token: &crate::CancellationToken,
    ) -> Result<OwnedSemaphorePermit, Error> {
        let shared = Arc::clone(&self.shared);
        let _registration = token.on_cancel(move || {
            let _guard = lock(&shared.state);
            shared.changed.notify_all();
        });
        let queue_deadline = self
            .acquire_timeout
            .and_then(|limit| Instant::now().checked_add(limit));
        let deadline = match (deadline, queue_deadline) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (left, right) => left.or(right),
        };
        let mut state = lock(&self.shared.state);
        let ticket = state.next;
        state.next = state.next.wrapping_add(1);
        state.waiting.push_back(ticket);
        loop {
            let failure = if token.is_cancelled() {
                Some(Error::cancelled(request_id.get(), command.clone()))
            } else if state.closed {
                Some(Error::executor_shutdown(request_id.get(), command.clone()))
            } else if state.active < self.limit && state.waiting.front() == Some(&ticket) {
                state.waiting.pop_front();
                state.active += 1;
                self.shared.changed.notify_all();
                return Ok(OwnedSemaphorePermit(Arc::clone(&self.shared)));
            } else if deadline.is_some_and(|end| Instant::now() >= end) {
                Some(Error::Overloaded {
                    request_id: request_id.get(),
                    command: command.clone(),
                    in_flight: self.limit,
                })
            } else {
                None
            };
            if let Some(error) = failure {
                state.waiting.retain(|item| *item != ticket);
                self.shared.changed.notify_all();
                return Err(error);
            }
            state = if let Some(end) = deadline {
                self.shared
                    .changed
                    .wait_timeout(state, end.saturating_duration_since(Instant::now()))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            } else {
                self.shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner)
            };
        }
    }
    pub(crate) fn close(&self) {
        lock(&self.shared.state).closed = true;
        self.shared.changed.notify_all();
    }
}

/// Owns the process group until its original leader has been reaped.
pub(crate) struct ProcessChild {
    child: Child,
    helper: PathBuf,
    reaped: bool,
    retired_error: Option<io::ErrorKind>,
    #[cfg(test)]
    denied_signals: Option<io::ErrorKind>,
    #[cfg(test)]
    denied_helper_kill: Option<io::ErrorKind>,
}
impl ProcessChild {
    pub(crate) fn spawn(launch: &LaunchContext, command: &mut Command) -> io::Result<Self> {
        let helper = launch.group_signal_helper();
        if !helper.is_absolute() || !runnable_file(helper) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "an absolute executable group-signal helper is required",
            ));
        }
        Ok(Self {
            child: command.spawn()?,
            helper: helper.to_path_buf(),
            reaped: false,
            retired_error: None,
            #[cfg(test)]
            denied_signals: None,
            #[cfg(test)]
            denied_helper_kill: None,
        })
    }
    #[cfg(feature = "control-mode")]
    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }
    /// Linux procfs observes exit without releasing the PID used as the group ID.
    pub(crate) fn exited(&mut self) -> io::Result<bool> {
        let observation = (|| {
            let stat = std::fs::read(format!("/proc/{}/stat", self.child.id()))?;
            let end = stat
                .iter()
                .rposition(|byte| *byte == b')')
                .ok_or_else(|| io::Error::other("invalid process status"))?;
            match stat.get(end + 2) {
                Some(b'Z' | b'X') => Ok(true),
                Some(_) => Ok(false),
                None => Err(io::Error::other("missing process state")),
            }
        })();
        self.observe_exit(observation)
    }

    fn observe_exit(&mut self, observation: io::Result<bool>) -> io::Result<bool> {
        if observation.is_err() {
            // An absent proc entry can mean another reaper released this PID.
            // Probe ownership once before any numeric group or child signal.
            loop {
                match self.child.try_wait() {
                    Ok(None) => break,
                    Ok(Some(_)) => {
                        self.reaped = true;
                        self.retired_error = Some(io::ErrorKind::Other);
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        self.retired_error = Some(error.kind());
                        break;
                    }
                }
            }
        }
        observation
    }
    pub(crate) fn finish(&mut self) -> io::Result<ExitStatus> {
        if let Some(kind) = self.retired_error {
            return Err(io::Error::new(
                kind,
                "process ownership was lost before group cleanup; descendants may remain",
            ));
        }
        if self.reaped {
            return self.child.wait();
        }
        let signalled = self.signal_group();
        let killed = self.kill_child();
        let waited = reap_after_signal(&mut self.child, killed.is_err());
        // ECHILD means an external reaper has released the numeric process ID.
        if matches!(waited, Ok(Some(_)))
            || waited
                .as_ref()
                .is_err_and(|error| error.raw_os_error() == Some(10))
        {
            self.reaped = true;
        }
        if let Some(error) = cleanup_error(&[
            (
                "process-group signal",
                signalled.err().map(|error| error.kind()),
            ),
            (
                "direct-child signal",
                killed.err().map(|error| error.kind()),
            ),
            ("child wait", waited.as_ref().err().map(io::Error::kind)),
        ]) {
            return Err(error);
        }
        waited?.ok_or_else(|| io::Error::other("child remains alive after failed termination"))
    }

    fn signal_group(&self) -> io::Result<()> {
        #[cfg(test)]
        if let Some(kind) = self.denied_signals {
            return Err(kind.into());
        }
        signal_group(
            &self.helper,
            self.child.id(),
            #[cfg(test)]
            self.denied_helper_kill,
        )
    }

    fn kill_child(&mut self) -> io::Result<()> {
        #[cfg(test)]
        if let Some(kind) = self.denied_signals {
            return Err(kind.into());
        }
        retry_signal(|| self.child.kill())
    }
}
impl Drop for ProcessChild {
    fn drop(&mut self) {
        if !self.reaped && self.retired_error.is_none() {
            let _ = self.finish();
        }
    }
}

fn retry_signal(mut signal: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    loop {
        match signal() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.raw_os_error() == Some(3) => return Ok(()),
            result => return result,
        }
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

fn reap_after_signal(child: &mut Child, signal_failed: bool) -> io::Result<Option<ExitStatus>> {
    loop {
        let result = if signal_failed {
            child.try_wait()
        } else {
            child.wait().map(Some)
        };
        match result {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}

fn signal_group(
    helper: &Path,
    pid: u32,
    #[cfg(test)] denied_helper_kill: Option<io::ErrorKind>,
) -> io::Result<()> {
    let mut command = Command::new(helper);
    command
        .args([
            OsString::from("-KILL"),
            OsString::from("--"),
            OsString::from(format!("-{pid}")),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    let timeout = Duration::from_secs(2);
    #[cfg(test)]
    let timeout = if denied_helper_kill.is_some() {
        Duration::from_millis(20)
    } else {
        timeout
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err(io::Error::other("process-group signal helper failed")),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
            Ok(None) => {
                #[cfg(test)]
                let killed = match denied_helper_kill {
                    Some(kind) => Err(kind.into()),
                    None => retry_signal(|| child.kill()),
                };
                #[cfg(not(test))]
                let killed = retry_signal(|| child.kill());
                let waited = reap_after_signal(&mut child, killed.is_err());
                return Err(cleanup_error(&[
                    ("helper signal", killed.err().map(|error| error.kind())),
                    ("helper wait", waited.err().map(|error| error.kind())),
                    ("helper deadline", Some(io::ErrorKind::TimedOut)),
                ])
                .unwrap_or_else(|| io::Error::from(io::ErrorKind::TimedOut)));
            }
        }
    }
}

pub(crate) fn output_pair() -> io::Result<(UnixStream, Stdio)> {
    let (parent, child) = UnixStream::pair()?;
    parent.set_nonblocking(true)?;
    Ok((parent, Stdio::from(OwnedFd::from(child))))
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(feature = "control-mode")]
mod persistent;
#[cfg(feature = "control-mode")]
pub(crate) use persistent::{PersistentChild, PersistentClients};

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::path::{Path, PathBuf};

    use super::LaunchContext;

    #[cfg(feature = "test-support")]
    #[test]
    fn signals_refused_return_without_waiting_for_a_live_child() {
        use rustix::process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal};
        use std::time::Duration;

        let launch = LaunchContext::new("/bin/sleep");
        let mut command = launch.command(&["30".into()]);
        let mut child = super::ProcessChild::spawn(&launch, &mut command).expect("child starts");
        let pid = Pid::from_raw(i32::try_from(child.child.id()).expect("PID fits")).expect("PID");
        let owned = pidfd_open(pid, PidfdFlags::empty()).expect("owned PIDFD");
        child.denied_signals = Some(std::io::ErrorKind::PermissionDenied);
        let (send, recv) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = child.finish();
            send.send((child, result))
                .expect("test receives cleanup result");
        });
        let observed = recv.recv_timeout(Duration::from_millis(150));
        let returned = observed.is_ok();
        let (mut child, result) = if let Ok(pair) = observed {
            pair
        } else {
            pidfd_send_signal(&owned, Signal::KILL).expect("harness kills owned child");
            recv.recv_timeout(Duration::from_secs(2))
                .expect("killed child releases wait")
        };
        let still_alive = child.child.try_wait().expect("child state").is_none();
        child.denied_signals = None;
        child.finish().expect("harness reaps owned child");
        worker.join().expect("worker joins");
        assert!(
            returned,
            "known signal errors must not wait for a live child"
        );
        assert!(
            still_alive,
            "injection must refuse effects, not report a fake signal success"
        );
        let error = result.expect_err("both denied signals are reported");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        let detail = error.to_string();
        assert!(detail.contains("process-group signal: PermissionDenied"));
        assert!(detail.contains("direct-child signal: PermissionDenied"));
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn drop_returns_when_signals_are_refused() {
        use rustix::process::{
            Pid, PidfdFlags, Signal, WaitOptions, pidfd_open, pidfd_send_signal, waitpid,
        };
        use std::time::Duration;

        let launch = LaunchContext::new("/bin/sleep");
        let mut command = launch.command(&["30".into()]);
        let mut child = super::ProcessChild::spawn(&launch, &mut command).expect("child starts");
        let pid = Pid::from_raw(i32::try_from(child.child.id()).expect("PID fits")).expect("PID");
        let owned = pidfd_open(pid, PidfdFlags::empty()).expect("owned PIDFD");
        child.denied_signals = Some(std::io::ErrorKind::PermissionDenied);
        let (send, recv) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            drop(child);
            send.send(()).expect("test observes completed Drop");
        });
        let returned = recv.recv_timeout(Duration::from_millis(150)).is_ok();
        pidfd_send_signal(&owned, Signal::KILL).expect("harness kills owned child");
        worker.join().expect("worker joins after harness cleanup");
        match waitpid(Some(pid), WaitOptions::empty()) {
            Ok(Some(_)) | Err(rustix::io::Errno::CHILD) => {}
            other => panic!("harness reaps owned child: {other:?}"),
        }
        assert!(returned, "Drop must not wait after known signal failures");
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn helper_signal_failure_does_not_wait_for_a_live_helper() {
        use rustix::process::{
            Pid, PidfdFlags, Signal, WaitOptions, pidfd_open, pidfd_send_signal, waitpid,
        };
        use std::time::{Duration, Instant};

        let directory = tempfile::tempdir().expect("fixture directory");
        let helper = directory.path().join("helper");
        let pid_file = directory.path().join("helper.pid");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec /bin/sleep 30\n",
                pid_file.display()
            ),
        )
        .expect("helper script written");
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))
            .expect("helper executable");
        let launch = LaunchContext::new("/bin/sleep").with_group_signal_helper(helper);
        let mut command = launch.command(&["30".into()]);
        let mut child = super::ProcessChild::spawn(&launch, &mut command).expect("child starts");
        child.denied_helper_kill = Some(std::io::ErrorKind::PermissionDenied);
        let (send, recv) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = child.finish();
            send.send(result).expect("test receives helper failure");
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        let helper_pid = loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file)
                && let Ok(value) = text.parse::<i32>()
            {
                break Pid::from_raw(value).expect("helper PID");
            }
            assert!(Instant::now() < deadline, "helper publishes PID");
            std::thread::sleep(Duration::from_millis(1));
        };
        let owned = pidfd_open(helper_pid, PidfdFlags::empty()).expect("helper PIDFD");
        let observed = recv.recv_timeout(Duration::from_millis(150));
        let returned = observed.is_ok();
        pidfd_send_signal(&owned, Signal::KILL).expect("harness kills owned helper");
        let result = match observed {
            Ok(result) => {
                waitpid(Some(helper_pid), WaitOptions::empty())
                    .expect("harness reaps abandoned helper");
                result
            }
            Err(_) => recv
                .recv_timeout(Duration::from_secs(2))
                .expect("killed helper releases wait"),
        };
        worker.join().expect("worker joins");
        assert!(
            returned,
            "known helper kill failure must not enter a blocking wait"
        );
        let error = result.expect_err("helper signal failure remains visible");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("process-group signal"));
    }

    #[test]
    fn lost_exit_observation_retires_cleanup_after_reaping() {
        let root = tempfile::tempdir().expect("temporary directory");
        let helper = root.path().join("kill");
        std::fs::write(
            &helper,
            b"#!/bin/sh\nprintf called > \"$(dirname \"$0\")/called\"\n",
        )
        .expect("helper written");
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))
            .expect("helper executable");
        let launch = LaunchContext::new("/bin/sh").with_group_signal_helper(helper);
        let mut command = launch.command(&["-c".into(), "exit 0".into()]);
        let mut child = super::ProcessChild::spawn(&launch, &mut command).expect("child starts");
        child
            .child
            .wait()
            .expect("simulate another owner reaping the leader");
        let observation =
            std::io::Error::new(std::io::ErrorKind::NotFound, "proc entry disappeared");
        assert!(child.observe_exit(Err(observation)).is_err());
        for _ in 0..2 {
            assert!(
                child
                    .finish()
                    .expect_err("cleanup cannot be proven")
                    .to_string()
                    .contains("ownership was lost")
            );
        }
        drop(child);
        assert!(
            !root.path().join("called").exists(),
            "retired ownership never invokes numeric cleanup"
        );
    }

    #[test]
    fn failed_exit_observation_still_cleans_up_an_owned_running_child() {
        let launch = LaunchContext::new("/bin/sh");
        let mut command = launch.command(&["-c".into(), "exec sleep 30".into()]);
        let mut child = super::ProcessChild::spawn(&launch, &mut command).expect("child starts");
        let observation =
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "procfs inaccessible");
        assert!(child.observe_exit(Err(observation)).is_err());
        assert!(
            child.retired_error.is_none(),
            "nonblocking ownership probe sees a running child"
        );
        child.finish().expect("owned group is signalled and reaped");
        assert!(child.reaped);
    }

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
