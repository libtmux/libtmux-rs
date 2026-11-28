use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};

use libtmux_core::control::{
    Completion, ControlFault, ControlIdentity, ControlNotice, ControlProbe, RequestKind, Tracker,
    fence_command,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};
use tokio::time::Instant;

use super::BlockResult;
use crate::internal::operation::nonce;
use crate::internal::process::PersistentChild;
use crate::limits::ControlLimits;
use crate::{Error, SessionId};

pub(super) struct Creation {
    request: libtmux_core::NewSession,
    server: crate::ServerIdentity,
    generation: crate::ServerGeneration,
    evidence: CreationEvidence,
}

#[derive(Clone, Default)]
pub(super) struct CreationEvidence(Arc<Mutex<CreationCheckpoint>>);

#[derive(Default)]
struct CreationCheckpoint {
    checked: bool,
    receipt: Option<libtmux_core::CreatedSession>,
}

impl CreationEvidence {
    pub(super) fn interrupted(&self, source: Error) -> Error {
        let receipt = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .receipt
            .clone();
        match receipt {
            Some(receipt) => source.after_creation(receipt),
            None => Error::OperationFailure {
                effect: libtmux_core::Effect::Unknown,
                receipt: None,
                source: Box::new(source),
            },
        }
    }
}

impl Creation {
    #[cfg(feature = "plan")]
    pub(super) fn new(
        request: libtmux_core::NewSession,
        server: crate::ServerIdentity,
        generation: crate::ServerGeneration,
    ) -> Self {
        Self {
            request,
            server,
            generation,
            evidence: CreationEvidence::default(),
        }
    }

    pub(super) fn evidence(&self) -> CreationEvidence {
        self.evidence.clone()
    }

    fn checkpoint(&self, first: Option<&libtmux_core::control::ControlBlock>) -> bool {
        let mut evidence = self
            .evidence
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !evidence.checked {
            if let Some(first) = first {
                evidence.checked = true;
                evidence.receipt = self
                    .request
                    .decode(self.server.socket_path(), first.output())
                    .ok()
                    .filter(|receipt| receipt.generation() == self.generation);
            }
        }
        evidence.receipt.is_some()
    }

    fn interrupted(
        &self,
        source: Error,
        first: Option<&libtmux_core::control::ControlBlock>,
    ) -> Error {
        self.checkpoint(first);
        self.evidence.interrupted(source)
    }
}

struct Shared {
    tracker: Mutex<Tracker>,
    changed: Arc<Notify>,
    closed: AtomicBool,
    #[cfg(test)]
    creation_progress: std::sync::atomic::AtomicU8,
}

struct CloseEvents(Arc<Shared>);

impl Drop for CloseEvents {
    fn drop(&mut self) {
        self.0.closed.store(true, Ordering::Release);
        self.0.changed.notify_waiters();
    }
}

impl Shared {
    fn tracker(&self) -> MutexGuard<'_, Tracker> {
        self.tracker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(super) struct EventReceiver {
    shared: Arc<Shared>,
    limits: ControlLimits,
    waiting: Option<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>,
    closed: bool,
}

impl fmt::Debug for EventReceiver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventReceiver")
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl EventReceiver {
    #[cfg(all(test, feature = "plan"))]
    pub(super) fn creation_input_seen(&self) -> bool {
        self.shared.creation_progress.load(Ordering::Acquire) >= 1
    }
    #[cfg(all(test, feature = "plan"))]
    pub(super) fn creation_receipt_ready(&self) -> bool {
        self.shared.creation_progress.load(Ordering::Acquire) >= 2
    }
    pub(super) const fn snapshot_budget(&self) -> (usize, usize) {
        (self.limits.max_events, self.limits.max_event_bytes)
    }
    pub(super) fn is_qualified(&self) -> bool {
        self.shared.tracker().is_qualified()
    }
    pub(super) fn poll_recv(&mut self, context: &mut Context<'_>) -> Poll<Option<ControlNotice>> {
        if self.closed {
            return Poll::Ready(None);
        }
        loop {
            if self.waiting.is_none() {
                self.waiting = Some(Box::pin(Arc::clone(&self.shared.changed).notified_owned()));
            }
            let notified = self
                .waiting
                .as_mut()
                .map_or(Poll::Pending, |waiting| waiting.as_mut().poll(context));
            let notice = self.shared.tracker().next_notice();
            if let Some(notice) = notice {
                self.waiting = None;
                return Poll::Ready(Some(notice));
            }
            if self.shared.closed.load(Ordering::Acquire) {
                self.waiting = None;
                return Poll::Ready(None);
            }
            if notified.is_pending() {
                return Poll::Pending;
            }
            self.waiting = None;
        }
    }

    pub(super) fn close(&mut self) {
        self.closed = true;
        self.waiting = None;
        self.shared.tracker().discard_notices();
    }
}

#[cfg(test)]
pub(super) struct TestEvents(Arc<Shared>);

#[cfg(test)]
impl TestEvents {
    pub(super) fn feed(&self, bytes: &[u8]) -> Result<(), ControlFault> {
        self.0.tracker().feed(bytes)?;
        self.0.changed.notify_waiters();
        Ok(())
    }

    pub(super) fn discard(&self) {
        self.0.tracker().discard_notices();
    }

    pub(super) fn finish(&self) {
        self.0.closed.store(true, Ordering::Release);
        self.0.changed.notify_waiters();
    }
}

#[cfg(test)]
impl Drop for TestEvents {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
pub(super) fn event_fixture(
    session: SessionId,
    limits: ControlLimits,
) -> (EventReceiver, TestEvents) {
    let shared = Arc::new(Shared {
        tracker: Mutex::new(Tracker::new(session, limits.shared())),
        changed: Arc::new(Notify::new()),
        closed: AtomicBool::new(false),
        #[cfg(test)]
        creation_progress: std::sync::atomic::AtomicU8::new(0),
    });
    (
        EventReceiver {
            shared: Arc::clone(&shared),
            limits,
            waiting: None,
            closed: false,
        },
        TestEvents(shared),
    )
}

pub(super) struct OpenedConnection {
    pub(super) commands: mpsc::Sender<Request>,
    pub(super) admission: Arc<Semaphore>,
    pub(super) events: EventReceiver,
    pub(super) stop: watch::Sender<()>,
    pub(super) connection: tokio::task::JoinHandle<Result<(), Error>>,
}

pub(super) async fn open(
    mut child: PersistentChild,
    session: SessionId,
    limits: ControlLimits,
    opening_deadline: Option<Instant>,
) -> Result<OpenedConnection, Error> {
    let Some(stdin) = child.take_stdin() else {
        return Err(cleanup_opening_child(child, Error::control_mode_pipes()).await);
    };
    let Some(stdout) = child.take_stdout() else {
        return Err(cleanup_opening_child(child, Error::control_mode_pipes()).await);
    };
    let core_stopped = child.stopped();
    let shared = Arc::new(Shared {
        tracker: Mutex::new(Tracker::new(session, limits.shared())),
        changed: Arc::new(Notify::new()),
        closed: AtomicBool::new(false),
        #[cfg(test)]
        creation_progress: std::sync::atomic::AtomicU8::new(0),
    });
    let admission = Arc::new(Semaphore::new(limits.max_pending_requests));
    let (commands, requests) = mpsc::channel(limits.max_pending_requests.max(1));
    let (stop, stopped) = watch::channel(());
    let (ready, mut opened) = oneshot::channel();
    let actor = Connection {
        child,
        stdin,
        stdout,
        shared: Arc::clone(&shared),
        limits,
        admission: Arc::clone(&admission),
        requests,
        stopped,
        core_stopped,
        active: None,
        token: 0,
        watching: true,
        sending: true,
    };
    let closure = CloseEvents(Arc::clone(&shared));
    let mut connection = tokio::spawn(async move {
        let _closure = closure;
        actor.run(ready, opening_deadline).await
    });
    tokio::select! {
        biased;
        result = &mut opened => {
            if result.is_err() {
                let _ = stop.send(());
                return Err(opening_cleanup(connection, Error::control_mode_closed()).await);
            }
        }
        () = deadline_elapsed(opening_deadline) => {
            let _ = stop.send(());
            return Err(opening_cleanup(connection, Error::control_mode_timeout()).await);
        }
        outcome = &mut connection => {
            return match outcome {
                Ok(Err(error)) => Err(error),
                _ => Err(Error::control_mode_closed()),
            };
        }
    }
    Ok(OpenedConnection {
        commands,
        admission,
        events: EventReceiver {
            shared,
            limits,
            waiting: None,
            closed: false,
        },
        stop,
        connection,
    })
}

async fn cleanup_opening_child(mut child: PersistentChild, primary: Error) -> Error {
    let cleanup = tokio::spawn(async move { child.terminate().await });
    let now = Instant::now();
    let latest = now
        .checked_add(libtmux_core::supervision::CLEANUP_ALLOWANCE)
        .unwrap_or(now);
    match tokio::time::timeout_at(latest, cleanup).await {
        Ok(Ok(Err(cleanup))) => Error::OperationAndCleanup {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        },
        Ok(Ok(Ok(()))) => primary,
        Ok(Err(_)) | Err(_) => Error::OperationCleanupPending {
            source: Box::new(primary),
        },
    }
}

async fn opening_cleanup(
    connection: tokio::task::JoinHandle<Result<(), Error>>,
    primary: Error,
) -> Error {
    let now = Instant::now();
    let latest = now
        .checked_add(libtmux_core::supervision::CLEANUP_ALLOWANCE)
        .unwrap_or(now);
    match tokio::time::timeout_at(latest, connection).await {
        Ok(Ok(Err(terminal))) => super::retain_cleanup(primary, terminal),
        Ok(Ok(Ok(())) | Err(_)) => primary,
        Err(_) => Error::OperationCleanupPending {
            source: Box::new(primary),
        },
    }
}

pub(super) struct Request {
    pub(super) line: String,
    pub(super) deadline: Option<Instant>,
    pub(super) result: oneshot::Sender<Result<BlockResult, Error>>,
    pub(super) commit: oneshot::Sender<()>,
    pub(super) kind: RequestKind,
    pub(super) permit: OwnedSemaphorePermit,
    pub(super) challenge: Option<(ControlProbe, ControlIdentity)>,
    pub(super) reply_limit: Option<usize>,
    pub(super) creation: Option<Creation>,
}

struct Active {
    token: u64,
    wire: Vec<u8>,
    written: usize,
    deadline: Option<Instant>,
    drain_deadline: Option<Instant>,
    result: Option<oneshot::Sender<Result<BlockResult, Error>>>,
    _permit: OwnedSemaphorePermit,
    creation: Option<Creation>,
}

struct Connection {
    child: PersistentChild,
    stdin: ChildStdin,
    stdout: ChildStdout,
    shared: Arc<Shared>,
    limits: ControlLimits,
    admission: Arc<Semaphore>,
    requests: mpsc::Receiver<Request>,
    stopped: watch::Receiver<()>,
    core_stopped: watch::Receiver<bool>,
    active: Option<Active>,
    token: u64,
    watching: bool,
    sending: bool,
}

enum Step {
    Read(io::Result<usize>),
    Write(io::Result<usize>),
    Request(Option<Box<Request>>),
    Watch(bool),
    CoreStopped,
    Cancelled,
    Deadline,
}

impl Connection {
    async fn run(
        mut self,
        mut ready: oneshot::Sender<()>,
        deadline: Option<Instant>,
    ) -> Result<(), Error> {
        let outcome = match self.opening(&mut ready, deadline).await {
            Ok(true) if ready.send(()).is_ok() => self.serve().await,
            Ok(_) => Ok(()),
            Err(error) => Err(error),
        };
        self.requests.close();
        self.admission.close();
        self.shared.tracker().close(ControlFault::Disconnected);
        self.complete(outcome.as_ref().err());
        while let Ok(request) = self.requests.try_recv() {
            let _ = request.result.send(Err(before_submission(
                request.creation.is_some(),
                Error::control_mode_closed(),
            )));
        }
        drop(self.stdin);
        let cleanup = self.child.terminate().await;
        self.shared.closed.store(true, Ordering::Release);
        self.shared.changed.notify_waiters();
        Error::after_cleanup(outcome, cleanup)
    }

    async fn opening(
        &mut self,
        ready: &mut oneshot::Sender<()>,
        deadline: Option<Instant>,
    ) -> Result<bool, Error> {
        let mut bytes = [0; 4096];
        loop {
            let read = tokio::select! {
                biased;
                () = ready.closed() => return Ok(false),
                _ = self.stopped.changed() => return Ok(false),
                () = cancellation_requested(&mut self.core_stopped) => return Err(self.child.shutdown_error()),
                () = deadline_elapsed(deadline) => return Err(Error::control_mode_timeout()),
                read = self.stdout.read(&mut bytes) => read.map_err(Error::control_mode)?,
            };
            if read == 0 {
                return Err(Error::control_mode_closed());
            }
            self.shared
                .tracker()
                .feed(&bytes[..read])
                .map_err(protocol_error)?;
            self.shared.changed.notify_waiters();
            if self.shared.tracker().is_established() {
                self.complete(None);
                return Ok(true);
            }
            if self.shared.tracker().is_closed() {
                return Err(Error::control_mode_closed());
            }
        }
    }

    async fn serve(&mut self) -> Result<(), Error> {
        let mut bytes = [0; 4096];
        while self.sending || self.watching || self.active.is_some() {
            self.checkpoint_creation();
            self.complete(None);
            let has_active = self.active.is_some();
            let receive_request = self.sending && !has_active;
            let mut no_result = None;
            let (wire, written, result, deadline, draining) = match &mut self.active {
                Some(active) => (
                    active.wire.as_slice(),
                    active.written,
                    &mut active.result,
                    active.drain_deadline.or(active.deadline),
                    active.drain_deadline.is_some(),
                ),
                None => (&[][..], 0, &mut no_result, None, false),
            };
            let writing = written < wire.len();
            let cancellable = result.is_some() && !draining;
            let step = tokio::select! {
                biased;
                changed = self.stopped.changed(), if self.watching => Step::Watch(changed.is_ok()),
                () = cancellation_requested(&mut self.core_stopped) => Step::CoreStopped,
                () = deadline_elapsed(deadline), if has_active => Step::Deadline,
                () = caller_cancelled(result), if cancellable => Step::Cancelled,
                write = self.stdin.write(&wire[written..]), if writing => Step::Write(write),
                request = self.requests.recv(), if receive_request => Step::Request(request.map(Box::new)),
                read = self.stdout.read(&mut bytes) => Step::Read(read),
            };
            match step {
                Step::Read(Ok(0)) => {
                    self.shared.tracker().eof().map_err(protocol_error)?;
                    // A normal %exit is handled as soon as its notice is read.
                    return Err(Error::control_mode_closed());
                }
                Step::Read(Ok(read)) => {
                    self.shared
                        .tracker()
                        .feed(&bytes[..read])
                        .map_err(protocol_error)?;
                    #[cfg(test)]
                    if self
                        .active
                        .as_ref()
                        .is_some_and(|active| active.creation.is_some())
                    {
                        self.shared
                            .creation_progress
                            .fetch_max(1, Ordering::Release);
                    }
                    if !self.watching {
                        self.shared.tracker().discard_notices();
                    }
                    self.shared.changed.notify_waiters();
                    self.complete(None);
                    if self.shared.tracker().is_closed() {
                        return Ok(());
                    }
                }
                Step::Read(Err(source)) | Step::Write(Err(source))
                    if source.kind() == io::ErrorKind::Interrupted => {}
                Step::Read(Err(source)) | Step::Write(Err(source)) => {
                    return Err(Error::control_mode(source));
                }
                Step::Write(Ok(0)) => {
                    return Err(Error::control_mode(io::ErrorKind::WriteZero.into()));
                }
                Step::Write(Ok(written)) => {
                    if let Some(active) = &mut self.active {
                        active.written += written;
                    }
                }
                Step::Request(Some(request)) => self.start(*request),
                Step::Request(None) => self.sending = false,
                Step::Watch(true) => return Ok(()),
                Step::Watch(false) => {
                    self.watching = false;
                    self.shared.tracker().discard_notices();
                }
                Step::CoreStopped => return Err(self.child.shutdown_error()),
                Step::Cancelled => self.cancel(false)?,
                Step::Deadline => self.cancel(true)?,
            }
        }
        Ok(())
    }

    fn start(&mut self, request: Request) {
        if request.result.is_closed()
            || request.commit.is_closed()
            || request
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            let _ = request.result.send(Err(before_submission(
                request.creation.is_some(),
                Error::control_mode_dispatch_timeout(),
            )));
            return;
        }
        if request.line.len().saturating_add(1) > self.limits.max_line_bytes {
            let _ = request.result.send(Err(before_submission(
                request.creation.is_some(),
                Error::control_mode_frame_too_large("command", self.limits.max_line_bytes),
            )));
            return;
        }
        let fence = match nonce() {
            Ok(nonce) => nonce,
            Err(error) => {
                let _ = request
                    .result
                    .send(Err(before_submission(request.creation.is_some(), error)));
                return;
            }
        };
        let Some(fence_line) = fence_command(fence).control_mode_line() else {
            let _ = request.result.send(Err(before_submission(
                request.creation.is_some(),
                Error::control_mode_unrepresentable(),
            )));
            return;
        };
        if request.commit.send(()).is_err() {
            return;
        }
        self.token = self.token.wrapping_add(1);
        let token = self.token;
        let registered = {
            let mut tracker = self.shared.tracker();
            match request.challenge {
                Some((probe, expected)) => {
                    tracker.register_challenge(token, fence, probe, expected)
                }
                None if tracker.is_qualified() => match request.reply_limit {
                    Some(limit) => {
                        tracker.register_with_reply_limit(token, fence, request.kind, limit)
                    }
                    None => tracker.register(token, fence, request.kind),
                },
                None => Err(ControlFault::InvalidChallenge),
            }
        };
        if let Err(fault) = registered {
            let _ = request.result.send(Err(before_submission(
                request.creation.is_some(),
                protocol_error(fault),
            )));
            return;
        }
        let wire = format!("{}\n{fence_line}\n", request.line).into_bytes();
        #[cfg(test)]
        if request.creation.is_some() {
            self.shared.creation_progress.store(0, Ordering::Release);
        }
        self.active = Some(Active {
            token,
            wire,
            written: 0,
            deadline: request.deadline,
            drain_deadline: None,
            result: Some(request.result),
            _permit: request.permit,
            creation: request.creation,
        });
    }

    fn checkpoint_creation(&self) {
        if let Some(active) = &self.active {
            if let Some(creation) = &active.creation {
                let verified =
                    creation.checkpoint(self.shared.tracker().first_input_block(active.token));
                #[cfg(test)]
                if verified {
                    self.shared.creation_progress.store(2, Ordering::Release);
                }
                #[cfg(not(test))]
                let _ = verified;
            }
        }
    }

    fn cancel(&mut self, deadline: bool) -> Result<(), Error> {
        let Some(active) = &mut self.active else {
            return Ok(());
        };
        if active.drain_deadline.is_some() || active.written < active.wire.len() {
            return Err(if deadline {
                Error::control_mode_timeout()
            } else {
                protocol_error(ControlFault::Cancelled)
            });
        }
        if let Some(result) = active.result.take() {
            let source = if deadline {
                Error::control_mode_timeout()
            } else {
                Error::control_mode_closed()
            };
            let error = match &active.creation {
                Some(creation) => creation.interrupted(
                    source,
                    self.shared.tracker().first_input_block(active.token),
                ),
                None => source,
            };
            let _ = result.send(Err(error));
        }
        self.shared.tracker().cancel(active.token);
        active.drain_deadline = Some(
            Instant::now()
                .checked_add(self.limits.drain_timeout)
                .unwrap_or_else(Instant::now),
        );
        Ok(())
    }

    fn take_active(&mut self, token: u64) -> Option<Active> {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.token == token)
        {
            self.active.take()
        } else {
            None
        }
    }

    fn complete(&mut self, failure: Option<&Error>) {
        loop {
            let completion = self.shared.tracker().next_completion();
            match completion {
                Some(Completion::Reply { token, reply }) => {
                    if let Some(mut active) = self.take_active(token) {
                        if let Some(result) = active.result.take() {
                            let _ = result.send(Ok(BlockResult::from_shared(reply)));
                        }
                    }
                }
                Some(Completion::Lost {
                    token,
                    fault,
                    first_block,
                }) => {
                    if let Some(mut active) = self.take_active(token) {
                        if let Some(result) = active.result.take() {
                            let source =
                                failure.map_or_else(|| protocol_error(fault), terminal_copy);
                            let error = match &active.creation {
                                Some(creation) => {
                                    creation.interrupted(source, first_block.as_ref())
                                }
                                None => source,
                            };
                            let _ = result.send(Err(error));
                        }
                    }
                }
                Some(Completion::Drained { token }) => {
                    if self
                        .active
                        .as_ref()
                        .is_some_and(|active| active.token == token)
                    {
                        self.active = None;
                    }
                }
                Some(Completion::Opened) => {}
                None => return,
            }
        }
    }
}

fn before_submission(creation: bool, error: Error) -> Error {
    if creation {
        crate::internal::operation::OperationContext::before_dispatch(error)
    } else {
        error
    }
}

fn terminal_copy(error: &Error) -> Error {
    match error {
        Error::ControlProtocol { fault } => protocol_error(*fault),
        Error::ControlModeFrameTooLarge { frame, limit } => {
            Error::control_mode_frame_too_large(frame, *limit)
        }
        Error::ControlMode { kind, source } => Error::ControlMode {
            kind: *kind,
            source: source.as_ref().map(|source| io::Error::from(source.kind())),
        },
        Error::ExecutorShutdown {
            request_id,
            command,
        } => Error::executor_shutdown(*request_id, command.clone()),
        _ => Error::control_mode_closed(),
    }
}

pub(super) fn protocol_error(fault: ControlFault) -> Error {
    match fault {
        ControlFault::LineLimit { limit } => Error::control_mode_frame_too_large("line", limit),
        ControlFault::ReplyLimit { limit } => Error::control_mode_frame_too_large("reply", limit),
        ControlFault::BlockLimit { limit } => Error::control_mode_frame_too_large("blocks", limit),
        ControlFault::Deadline => Error::control_mode_timeout(),
        fault => Error::ControlProtocol { fault },
    }
}

async fn caller_cancelled(result: &mut Option<oneshot::Sender<Result<BlockResult, Error>>>) {
    if let Some(result) = result {
        result.closed().await;
    } else {
        std::future::pending::<()>().await;
    }
}

pub(super) async fn deadline_elapsed(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

async fn cancellation_requested(stopped: &mut watch::Receiver<bool>) {
    loop {
        if *stopped.borrow() {
            return;
        }
        if stopped.changed().await.is_err() {
            return;
        }
    }
}
