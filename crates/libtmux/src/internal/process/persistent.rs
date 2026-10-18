use std::collections::HashSet;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Instant;

use super::{
    LaunchContext, OwnedSemaphorePermit, ProcessAdmission, ProcessChild, lock, validate_request,
};
use crate::Error;
use crate::command::{CommandRequest, CommandSummary, RequestId};
use crate::limits::ControlClientLimits;

#[derive(Clone)]
pub(crate) struct PersistentClients {
    shared: Arc<Shared>,
    admission: ProcessAdmission,
}
struct Shared {
    lifecycle: Mutex<Lifecycle>,
    stopped: Arc<AtomicBool>,
    shutdown: crate::CancellationToken,
    empty: Condvar,
}
struct Lifecycle {
    accepting: bool,
    active: usize,
    cleanup_failure: Option<CleanupFailure>,
}

#[derive(Clone)]
struct CleanupFailure {
    request_id: u64,
    command: CommandSummary,
    kind: io::ErrorKind,
    message: String,
}
impl CleanupFailure {
    fn error(&self) -> Error {
        Error::wait_child(
            self.request_id,
            self.command.clone(),
            io::Error::new(self.kind, self.message.clone()),
        )
    }
}
impl PersistentClients {
    pub(crate) fn new(limits: ControlClientLimits) -> Self {
        Self {
            shared: Arc::new(Shared {
                lifecycle: Mutex::new(Lifecycle {
                    accepting: true,
                    active: 0,
                    cleanup_failure: None,
                }),
                stopped: Arc::new(AtomicBool::new(false)),
                empty: Condvar::new(),
                shutdown: crate::CancellationToken::new(),
            }),
            admission: ProcessAdmission::new(limits.max_clients, limits.acquire_timeout),
        }
    }
    pub(crate) fn reserve(
        &self,
        request_id: RequestId,
        command: CommandSummary,
        deadline: Option<Instant>,
        cancellation: &crate::CancellationToken,
    ) -> Result<PersistentReservation, Error> {
        let permit = self
            .admission
            .acquire(request_id, &command, deadline, cancellation)?;
        let mut lifecycle = lock(&self.shared.lifecycle);
        if !lifecycle.accepting {
            return Err(Error::executor_shutdown(request_id.get(), command));
        }
        lifecycle.active += 1;
        Ok(PersistentReservation {
            stopped: Arc::clone(&self.shared.stopped),
            shutdown: self.shared.shutdown.clone(),
            registration: Registration {
                shared: Arc::clone(&self.shared),
                _permit: permit,
            },
        })
    }
    pub(crate) fn shutdown(&self) -> Result<(), Error> {
        self.admission.close();
        {
            let mut lifecycle = lock(&self.shared.lifecycle);
            lifecycle.accepting = false;
            self.shared.stopped.store(true, Ordering::Release);
        }
        // Callbacks can wake a connection that is releasing its registration.
        self.shared.shutdown.cancel();
        let mut lifecycle = lock(&self.shared.lifecycle);
        while lifecycle.active != 0 {
            lifecycle = self
                .shared
                .empty
                .wait(lifecycle)
                .unwrap_or_else(PoisonError::into_inner);
        }
        match &lifecycle.cleanup_failure {
            Some(failure) => Err(failure.error()),
            None => Ok(()),
        }
    }
}
pub(crate) struct PersistentReservation {
    stopped: Arc<AtomicBool>,
    shutdown: crate::CancellationToken,
    registration: Registration,
}
struct Registration {
    shared: Arc<Shared>,
    _permit: OwnedSemaphorePermit,
}
impl Drop for Registration {
    fn drop(&mut self) {
        lock(&self.shared.lifecycle).active -= 1;
        self.shared.empty.notify_all();
    }
}
struct PidGuard {
    pids: Arc<Mutex<HashSet<u32>>>,
    pid: u32,
}
impl Drop for PidGuard {
    fn drop(&mut self) {
        lock(&self.pids).remove(&self.pid);
    }
}

pub(crate) struct PersistentChild {
    // Cleanup and PID removal precede releasing the persistent-client permit.
    child: ProcessChild,
    _pid: PidGuard,
    registration: Registration,
    stdin: Option<UnixStream>,
    stdout: Option<UnixStream>,
    shutdown: crate::CancellationToken,
    request_id: RequestId,
    command: CommandSummary,
    terminated: bool,
    cleanup_failure: Option<CleanupFailure>,
}
impl PersistentChild {
    pub(crate) fn spawn(
        launch: &LaunchContext,
        request: &CommandRequest,
        reservation: PersistentReservation,
        pids: Arc<Mutex<HashSet<u32>>>,
    ) -> Result<Self, Error> {
        validate_request(launch, request)?;
        if request.cancellation_token().is_cancelled() {
            return Err(Error::cancelled(
                request.request_id().get(),
                request.summary().clone(),
            ));
        }
        if reservation.stopped.load(Ordering::Acquire) {
            return Err(Error::executor_shutdown(
                request.request_id().get(),
                request.summary().clone(),
            ));
        }
        let (stdin, child_stdin) = UnixStream::pair().map_err(Error::control_mode)?;
        let (stdout, child_stdout) = UnixStream::pair().map_err(Error::control_mode)?;
        let mut command = launch.command(request.argv());
        command
            .stdin(Stdio::from(OwnedFd::from(child_stdin)))
            .stdout(Stdio::from(OwnedFd::from(child_stdout)))
            .stderr(Stdio::null());
        let child = ProcessChild::spawn(launch, &mut command).map_err(|source| {
            let missing = (source.kind() == io::ErrorKind::NotFound
                && launch.current_dir().is_none_or(Path::is_dir))
                || launch.executable_missing_from_path();
            if missing {
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
        let pid = child.id();
        lock(&pids).insert(pid);
        Ok(Self {
            child,
            _pid: PidGuard { pids, pid },
            registration: reservation.registration,
            stdin: Some(stdin),
            stdout: Some(stdout),
            shutdown: reservation.shutdown,
            request_id: request.request_id(),
            command: request.summary().clone(),
            terminated: false,
            cleanup_failure: None,
        })
    }
    pub(crate) fn take_stdin(&mut self) -> Option<UnixStream> {
        self.stdin.take()
    }
    pub(crate) fn take_stdout(&mut self) -> Option<UnixStream> {
        self.stdout.take()
    }
    pub(crate) fn shutdown_token(&self) -> crate::CancellationToken {
        self.shutdown.clone()
    }
    pub(crate) fn cancelled_error(&self) -> Error {
        Error::cancelled(self.request_id.get(), self.command.clone())
    }
    pub(crate) fn shutdown_error(&self) -> Error {
        Error::executor_shutdown(self.request_id.get(), self.command.clone())
    }
    pub(crate) fn terminate(&mut self) -> Result<(), Error> {
        if !self.terminated {
            self.terminated = true;
            if let Err(source) = self.child.finish() {
                let failure = CleanupFailure {
                    request_id: self.request_id.get(),
                    command: self.command.clone(),
                    kind: source.kind(),
                    message: source.to_string(),
                };
                lock(&self.registration.shared.lifecycle)
                    .cleanup_failure
                    .get_or_insert_with(|| failure.clone());
                self.cleanup_failure = Some(failure);
            }
        }
        match &self.cleanup_failure {
            Some(failure) => Err(failure.error()),
            None => Ok(()),
        }
    }
}
impl Drop for PersistentChild {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}
