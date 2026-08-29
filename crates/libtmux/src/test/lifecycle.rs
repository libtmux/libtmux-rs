use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use rustix::fs::{AtFlags, Mode, OFlags, fchmod, fstat, openat, unlinkat};
use rustix::io::{Errno, write as write_all};
use rustix::process::{Pid, Signal, kill_process, kill_process_group};

use super::containment::OwnerContainment;
use super::{
    CLEANUP_POLL_INTERVAL, CONFIG_NAME, CONTAINMENT_TIMEOUT, DaemonState, FIXTURE_CONFIG,
    FIXTURE_ROOT, LOCK_NAME, LeaderObservation, LeaderObserver, OWNER_NAME, SOCKET_NAME,
    TestServer, TestServerError, TestServerErrorKind, scaled,
};
use crate::{Command, Server};

pub(super) struct OwnedFiles {
    pub(super) socket_path: PathBuf,
    pub(super) config_path: PathBuf,
    directory_name: OsString,
    socket_name: OsString,
    config_name: OsString,
    lock_name: OsString,
    owner_name: OsString,
    parent: OwnedFd,
    directory: OwnedFd,
    pub(super) containment: OwnerContainment,
    cleanup_armed: bool,
}

impl OwnedFiles {
    pub(super) fn create() -> Result<Self, TestServerError> {
        Self::create_with_setup_hook(|_| {})
    }

    pub(super) fn create_with_setup_hook(
        hook: impl FnOnce(&Path),
    ) -> Result<Self, TestServerError> {
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

    pub(super) fn retain_until_contained(&mut self) {
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

pub(super) struct Lifecycle {
    child: Child,
    pub(super) pid: Pid,
    pub(super) files: OwnedFiles,
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

pub(super) enum CleanupOutcome {
    Complete,
    LifecycleFailed,
    FilesystemFailed,
    LifecycleAndFilesystemFailed,
}

impl Lifecycle {
    #[cfg(test)]
    pub(super) fn new(child: Child, files: OwnedFiles) -> Self {
        Self::new_with_leader_observer(
            child,
            files,
            leader_exited_unreaped,
            super::platform_fallback_grace_ceiling(),
        )
    }

    pub(super) fn new_with_leader_observer(
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

    pub(super) fn observe_leader(&mut self) -> LeaderObservation {
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
    pub(super) fn numeric_signaling_retired(&self) -> bool {
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
    pub(super) fn daemon_state(&mut self) -> DaemonState {
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

    pub(super) fn cleanup(&mut self, timeout: Duration) -> CleanupOutcome {
        let (_, lifecycle_ok) = self.terminate_gracefully(timeout);
        let filesystem_ok = lifecycle_ok && self.files.cleanup_after_containment();
        CleanupOutcome::from_results(lifecycle_ok, filesystem_ok)
    }

    pub(super) fn force_cleanup(&mut self) -> CleanupOutcome {
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
    pub(super) fn failure(&self) -> Option<String> {
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

pub(super) struct StartupGuard {
    lifecycle: Option<Lifecycle>,
}

impl StartupGuard {
    pub(super) fn new(lifecycle: Lifecycle) -> Self {
        Self {
            lifecycle: Some(lifecycle),
        }
    }

    pub(super) fn lifecycle(&self) -> Option<&Lifecycle> {
        self.lifecycle.as_ref()
    }

    pub(super) fn lifecycle_mut(&mut self) -> Option<&mut Lifecycle> {
        self.lifecycle.as_mut()
    }

    pub(super) fn disarm(mut self) -> Option<Lifecycle> {
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

pub(super) async fn server_startup_failure(
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

pub(super) async fn startup_timeout(
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

pub(super) async fn startup_failure(
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

pub(super) fn remaining_timeout(started: Instant, timeout: Duration) -> Option<Duration> {
    timeout.checked_sub(started.elapsed())
}

pub(super) async fn readiness_with_timeout(
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
pub(super) fn leader_exited_unreaped(pid: Pid) -> LeaderObservation {
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
pub(super) fn leader_exited_unreaped(_pid: Pid) -> LeaderObservation {
    LeaderObservation::Unavailable
}

pub(super) fn socket_path_fits_tmux(path: &Path) -> bool {
    let bytes = path.as_os_str().as_bytes();
    if bytes.contains(&0) {
        return false;
    }
    let mut sentinel = bytes.to_vec();
    sentinel.push(b'x');
    rustix::net::SocketAddrUnix::new(PathBuf::from(OsString::from_vec(sentinel))).is_ok()
}
