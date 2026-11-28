use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt as _;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
#[cfg(feature = "control-mode")]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

use libtmux_core::supervision::{
    Budget, CLEANUP_ALLOWANCE, CleanupErrors, Event, MAX_IN_FLIGHT, after_cleanup,
};
use libtmux_core::{CreationNonce, Error};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};

use crate::RequestOptions;

const POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_CLEANUP_SUMMARY: usize = 1024;

#[derive(Clone, Copy)]
enum OutputMode {
    Collect,
    DiscardStdout,
}

#[derive(Clone, Debug)]
pub(super) struct Children {
    ordinary: Arc<Ordinary>,
    limit: usize,
    #[cfg(feature = "control-mode")]
    persistent_available: Arc<AtomicUsize>,
    #[cfg(test)]
    pub(super) reject_pipe_setup: Option<PathBuf>,
    #[cfg(test)]
    cleanup_gate: Option<Arc<tests::CleanupGate>>,
}

impl Default for Children {
    fn default() -> Self {
        Self::with_limit(MAX_IN_FLIGHT)
    }
}

#[cfg(feature = "control-mode")]
pub(super) struct Permit(Arc<AtomicUsize>);

#[derive(Debug, Default)]
struct Ordinary {
    state: Mutex<OrdinaryState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct OrdinaryState {
    closed: bool,
    active: usize,
    failure: Option<String>,
}

struct OrdinaryPermit(Arc<Ordinary>);

impl Ordinary {
    fn lock(&self) -> MutexGuard<'_, OrdinaryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn failure(&self, error: &impl std::fmt::Display) {
        let mut state = self.lock();
        if state.failure.is_none() {
            let mut summary = error.to_string();
            let mut end = summary.len().min(MAX_CLEANUP_SUMMARY);
            while !summary.is_char_boundary(end) {
                end -= 1;
            }
            summary.truncate(end);
            state.failure = Some(summary);
        }
    }
}

impl Drop for OrdinaryPermit {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.failure(&"subprocess supervisor panicked");
        }
        let mut state = self.0.lock();
        state.active -= 1;
        self.0.changed.notify_all();
    }
}

enum CreationReceipt {
    #[cfg(test)]
    Control(PathBuf),
    Ordinary {
        socket: PathBuf,
        nonce: CreationNonce,
    },
}

fn interrupted(receipt: Option<&CreationReceipt>, stdout: &[u8], cause: Error) -> Error {
    match receipt {
        #[cfg(test)]
        Some(CreationReceipt::Control(socket)) => {
            libtmux_core::Reply::interrupted(Some(socket), stdout, cause)
        }
        Some(CreationReceipt::Ordinary { socket, nonce }) => {
            libtmux_core::Reply::interrupted_ordinary_creation(socket, *nonce, stdout, cause)
        }
        None => cause,
    }
}

#[cfg(feature = "control-mode")]
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}

impl Children {
    fn with_limit(limit: usize) -> Self {
        Self {
            ordinary: Arc::default(),
            limit,
            #[cfg(feature = "control-mode")]
            persistent_available: Arc::new(AtomicUsize::new(4)),
            #[cfg(test)]
            reject_pipe_setup: None,
            #[cfg(test)]
            cleanup_gate: None,
        }
    }

    pub(super) fn close_ordinary(&self) -> Result<(), Error> {
        let deadline = Instant::now() + CLEANUP_ALLOWANCE;
        let mut state = self.ordinary.lock();
        state.closed = true;
        while state.active != 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let cause = state
                    .failure
                    .clone()
                    .unwrap_or_else(|| "ordinary subprocess cleanup has not completed".into());
                return Err(Error::CleanupPending(Box::new(Error::Shutdown(cause))));
            }
            state = self
                .ordinary
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        match &state.failure {
            Some(summary) => Err(Error::Shutdown(summary.clone())),
            None => Ok(()),
        }
    }

    fn reserve_ordinary(&self) -> Result<OrdinaryPermit, Error> {
        let mut state = self.ordinary.lock();
        if state.closed {
            return Err(Error::Closed);
        }
        if state.active == self.limit {
            return Err(Error::Busy { limit: self.limit });
        }
        state.active += 1;
        Ok(OrdinaryPermit(Arc::clone(&self.ordinary)))
    }

    #[cfg(feature = "control-mode")]
    pub(super) fn reserve_persistent(&self) -> Result<Permit, Error> {
        reserve(&self.persistent_available, 4)
    }

    #[cfg(test)]
    fn dispatch(
        &self,
        command: Command,
        options: RequestOptions,
        creation_socket: Option<PathBuf>,
    ) -> Result<(Output, bool), Error> {
        self.dispatch_inner(
            command,
            options,
            creation_socket.map(CreationReceipt::Control),
            OutputMode::Collect,
        )
    }

    pub(super) fn dispatch_ordinary(
        &self,
        command: Command,
        options: RequestOptions,
        creation: Option<(PathBuf, CreationNonce)>,
    ) -> Result<(Output, bool), Error> {
        self.dispatch_inner(
            command,
            options,
            creation.map(|(socket, nonce)| CreationReceipt::Ordinary { socket, nonce }),
            OutputMode::Collect,
        )
    }

    pub(super) fn dispatch_probe(
        &self,
        command: Command,
        options: RequestOptions,
    ) -> Result<(Output, bool), Error> {
        self.dispatch_inner(command, options, None, OutputMode::DiscardStdout)
    }

    fn dispatch_inner(
        &self,
        command: Command,
        options: RequestOptions,
        creation: Option<CreationReceipt>,
        output_mode: OutputMode,
    ) -> Result<(Output, bool), Error> {
        check_before_spawn(&options)?;
        let permit = self.reserve_ordinary()?;
        let budget = options.budget;
        let (tx, rx) = mpsc::channel();
        #[cfg(test)]
        let reject_pipe_setup = self.reject_pipe_setup.clone();
        #[cfg(test)]
        let cleanup_gate = self.cleanup_gate.clone();
        std::thread::Builder::new()
            .name("libtmux blocking child".into())
            .spawn(move || {
                supervise(
                    command,
                    &options,
                    creation.as_ref(),
                    output_mode,
                    permit,
                    &tx,
                    #[cfg(test)]
                    (reject_pipe_setup.as_deref(), cleanup_gate),
                );
            })
            .map_err(Error::Spawn)?;
        let latest = budget.deadline.checked_add(CLEANUP_ALLOWANCE);
        let remaining = || {
            latest.map_or(CLEANUP_ALLOWANCE, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            })
        };
        match rx.recv_timeout(remaining()) {
            Ok(Event::Done(result)) => result,
            Ok(Event::Failed(primary)) => Err(after_cleanup(
                primary,
                rx.recv_timeout(remaining().min(CLEANUP_ALLOWANCE)).ok(),
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(Error::CleanupPending(Box::new(Error::Deadline)))
            }
            Ok(Event::Cleaned(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(Error::CleanupPending(Box::new(Error::LostReply(
                    io::Error::other("subprocess supervisor stopped before reporting completion"),
                ))))
            }
        }
    }
}

#[cfg(feature = "control-mode")]
pub(super) fn reserve(available: &Arc<AtomicUsize>, limit: usize) -> Result<Permit, Error> {
    let mut observed = available.load(Ordering::Acquire);
    loop {
        let next = observed.checked_sub(1).ok_or(Error::Busy { limit })?;
        match available.compare_exchange_weak(observed, next, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => return Ok(Permit(Arc::clone(available))),
            Err(current) => observed = current,
        }
    }
}

pub(super) fn check_before_spawn(options: &RequestOptions) -> Result<(), Error> {
    if options.cancellation.is_cancelled() {
        Err(Error::CancelledBeforeSpawn)
    } else if Instant::now() >= options.budget.deadline {
        Err(Error::BeforeSpawn)
    } else {
        Ok(())
    }
}

fn supervise(
    mut command: Command,
    options: &RequestOptions,
    creation: Option<&CreationReceipt>,
    output_mode: OutputMode,
    permit: OrdinaryPermit,
    tx: &mpsc::Sender<Event<(Output, bool)>>,
    #[cfg(test)] hooks: (Option<&Path>, Option<Arc<tests::CleanupGate>>),
) {
    if let Err(error) = check_before_spawn(options) {
        drop(permit);
        let _ = tx.send(Event::Done(Err(error)));
        return;
    }
    let spawned = command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(match output_mode {
            OutputMode::Collect => Stdio::piped(),
            OutputMode::DiscardStdout => Stdio::null(),
        })
        .stderr(Stdio::piped())
        .spawn();
    let child = match spawned {
        Ok(child) => child,
        Err(error) => {
            drop(permit);
            let _ = tx.send(Event::Done(Err(Error::Spawn(error))));
            return;
        }
    };
    let mut child = OwnedChild::new(child);
    child.ordinary = Some(permit);
    #[cfg(test)]
    {
        child.cleanup_gate = hooks.1;
    }
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let drained = drain(
        &mut child,
        options,
        &mut stdout,
        &mut stderr,
        output_mode,
        #[cfg(test)]
        hooks.0,
    );
    match drained {
        Ok(()) => match child.finish() {
            Ok(status) => {
                let output = Output {
                    status,
                    stdout,
                    stderr,
                };
                drop(child);
                let _ = tx.send(Event::Done(Ok((
                    output,
                    options.budget.is_late(Instant::now()),
                ))));
            }
            Err(errors) => {
                let cause = Error::LostReply(io::Error::other("subprocess group cleanup failed"));
                let primary = interrupted(creation, &stdout, cause);
                let _ = tx.send(Event::Failed(primary));
                publish_cleanup(child, tx, Err(errors));
            }
        },
        Err(cause) => {
            let primary = interrupted(creation, &stdout, cause);
            let _ = tx.send(Event::Failed(primary));
            let cleanup = child.finish().map(|_| ());
            publish_cleanup(child, tx, cleanup);
        }
    }
}

fn publish_cleanup(
    mut child: OwnedChild,
    tx: &mpsc::Sender<Event<(Output, bool)>>,
    cleanup: Result<(), CleanupErrors>,
) {
    if child.child.is_none() {
        drop(child);
        let _ = tx.send(Event::Cleaned(cleanup));
    } else {
        let _ = tx.send(Event::Cleaned(cleanup));
        // An unreaped child keeps its admission slot even after its caller returns.
        child.reap();
    }
}

pub(super) struct OwnedChild {
    pub(super) child: Option<Child>,
    group: Option<Pid>,
    // Field drop follows Drop's final wait, including during unwinding.
    ordinary: Option<OrdinaryPermit>,
    #[cfg(test)]
    cleanup_gate: Option<Arc<tests::CleanupGate>>,
}

impl OwnedChild {
    pub(super) fn new(child: Child) -> Self {
        let group = i32::try_from(child.id()).ok().and_then(Pid::from_raw);
        Self {
            child: Some(child),
            group,
            ordinary: None,
            #[cfg(test)]
            cleanup_gate: None,
        }
    }

    pub(super) fn has_exited(&self) -> io::Result<bool> {
        let pid = self
            .group
            .ok_or_else(|| io::Error::other("subprocess identity is unavailable"))?;
        waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .map(|status| status.is_some())
        .map_err(Into::into)
    }

    pub(super) fn finish(&mut self) -> Result<ExitStatus, CleanupErrors> {
        #[cfg(test)]
        let gate = self.cleanup_gate.take();
        #[cfg(test)]
        if let Some(gate) = &gate {
            gate.wait();
        }
        let result = self.finish_inner();
        #[cfg(test)]
        let result = if gate.is_some_and(|gate| gate.fail) && result.is_ok() {
            Err(CleanupErrors {
                kill: Some(io::Error::other(
                    "injected cleanup signal failure".repeat(256),
                )),
                reap: None,
            })
        } else {
            result
        };
        if let (Some(permit), Err(errors)) = (&self.ordinary, &result) {
            permit.0.failure(errors);
        }
        result
    }

    fn finish_inner(&mut self) -> Result<ExitStatus, CleanupErrors> {
        let Some(child) = self.child.as_mut() else {
            return Err(CleanupErrors {
                kill: None,
                reap: Some(io::Error::other("subprocess was already reaped")),
            });
        };
        let Some(group) = self.group.take() else {
            return Err(CleanupErrors {
                kill: Some(io::Error::other("subprocess group identity is unavailable")),
                reap: None,
            });
        };
        let ownership = waitid(
            WaitId::Pid(group),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        );
        if let Err(error) = ownership {
            if error == rustix::io::Errno::CHILD {
                self.child = None;
            }
            return Err(CleanupErrors {
                kill: None,
                reap: Some(error.into()),
            });
        }
        // The unreaped group leader prevents its numeric identity from being reused.
        let signalled = match kill_process_group(group, Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(error) => Err(io::Error::from(error)),
        };
        // A child can leave its original group, so signal its still-owned PID too.
        let direct = child.kill();
        let kill_error = match (signalled, direct) {
            (Ok(()), Ok(())) => None,
            (Err(group), Ok(())) => Some(group),
            (Ok(()), Err(direct)) => Some(direct),
            (Err(group), Err(direct)) => {
                return Err(CleanupErrors {
                    kill: Some(io::Error::other(format!(
                        "process group signal: {group}; direct child signal: {direct}"
                    ))),
                    reap: Some(io::Error::other("child is still owned; reaping continues")),
                });
            }
        };
        let waited = child.wait();
        if waited.is_ok() {
            self.child = None;
        }
        match (kill_error, waited) {
            (None, Ok(status)) => Ok(status),
            (kill, reap) => Err(CleanupErrors {
                kill,
                reap: reap.err(),
            }),
        }
    }

    pub(super) fn reap(&mut self) {
        while let Some(child) = self.child.as_mut() {
            match child.wait() {
                Ok(_) => break,
                Err(error)
                    if error.raw_os_error() == Some(rustix::io::Errno::CHILD.raw_os_error()) =>
                {
                    break;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => {
                    if let Some(permit) = &self.ordinary {
                        permit.0.failure(&CleanupErrors {
                            kill: None,
                            reap: Some(error),
                        });
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            }
        }
        self.child = None;
    }
}

#[cfg(feature = "control-mode")]
pub(super) fn nonblocking_file(fd: impl Into<OwnedFd>) -> io::Result<File> {
    let file = File::from(fd.into());
    fcntl_getfl(&file).and_then(|flags| fcntl_setfl(&file, flags | OFlags::NONBLOCK))?;
    Ok(file)
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.finish();
            self.reap();
        }
    }
}

struct Pipe {
    reader: Option<File>,
    stream: &'static str,
}

impl Pipe {
    fn new(fd: impl Into<OwnedFd>, stream: &'static str) -> Result<Self, Error> {
        let file = File::from(fd.into());
        fcntl_getfl(&file)
            .and_then(|flags| fcntl_setfl(&file, flags | OFlags::NONBLOCK))
            .map_err(|error| Error::LostReply(error.into()))?;
        Ok(Self {
            reader: Some(file),
            stream,
        })
    }

    fn read_ready(&mut self, budget: Budget, bytes: &mut Vec<u8>) -> Result<(), Error> {
        let Some(reader) = self.reader.as_mut() else {
            return Ok(());
        };
        let mut scratch = [0; 8192];
        match reader.read(&mut scratch) {
            Ok(0) => self.reader = None,
            Ok(count) => {
                let available = budget.bytes_per_stream.saturating_sub(bytes.len());
                budget.append(self.stream, bytes, &scratch[..count.min(available)])?;
                if count > available {
                    return Err(Error::OutputLimit {
                        stream: self.stream,
                        limit: budget.bytes_per_stream,
                    });
                }
            }
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
            Err(error) => return Err(Error::LostReply(error)),
        }
        Ok(())
    }
}

fn drain(
    child: &mut OwnedChild,
    options: &RequestOptions,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    output_mode: OutputMode,
    #[cfg(test)] reject_pipe_setup: Option<&Path>,
) -> Result<(), Error> {
    #[cfg(test)]
    if let Some(pid_file) = reject_pipe_setup {
        while !pid_file.exists() && Instant::now() < options.budget.deadline {
            std::thread::sleep(POLL_INTERVAL);
        }
        return Err(Error::LostReply(io::Error::other(
            "injected pipe setup failure",
        )));
    }
    let process = child
        .child
        .as_mut()
        .ok_or_else(|| Error::LostReply(io::Error::other("subprocess is unavailable")))?;
    let missing_pipe =
        || Error::LostReply(io::Error::other("subprocess output pipe is unavailable"));
    let mut out = match output_mode {
        OutputMode::Collect => {
            Pipe::new(process.stdout.take().ok_or_else(missing_pipe)?, "stdout")?
        }
        OutputMode::DiscardStdout => Pipe {
            reader: None,
            stream: "stdout",
        },
    };
    let mut err = Pipe::new(process.stderr.take().ok_or_else(missing_pipe)?, "stderr")?;
    loop {
        if options.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if Instant::now() >= options.budget.deadline {
            return Err(Error::Deadline);
        }
        out.read_ready(options.budget, stdout)?;
        err.read_ready(options.budget, stderr)?;
        if out.reader.is_none()
            && err.reader.is_none()
            && child.has_exited().map_err(Error::LostReply)?
        {
            return Ok(());
        }
        let mut fds: Vec<_> = [out.reader.as_ref(), err.reader.as_ref()]
            .into_iter()
            .flatten()
            .map(|reader| PollFd::new(reader, PollFlags::IN))
            .collect();
        let remaining = options
            .budget
            .deadline
            .saturating_duration_since(Instant::now())
            .min(POLL_INTERVAL);
        let timeout = Timespec::try_from(remaining)
            .map_err(|error| Error::LostReply(io::Error::other(error)))?;
        match poll(&mut fds, Some(&timeout)) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(error) => return Err(Error::LostReply(error.into())),
        }
    }
}

#[cfg(test)]
mod tests;
