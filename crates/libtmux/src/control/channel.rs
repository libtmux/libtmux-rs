//! Bounded control queues and the first-write commitment boundary.

use std::collections::VecDeque;
use std::fmt;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::Instant;

use super::protocol::RawLine;
use super::{BlockResult, Boundary, Delivery};
use crate::command::CommandSummary;
use crate::{CancellationRegistration, CancellationToken, Error};

pub(super) const COMMAND_QUEUE: usize = 16;
pub(super) const EVENT_QUEUE: usize = 256;
const QUEUED: u8 = 0;
const COMMITTED: u8 = 1;
const ABANDONED: u8 = 2;

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "Reader, receiver, admission and executor lifetimes are independent."
)]
pub(super) struct State {
    pub(super) commands: VecDeque<Request>,
    pub(super) events: VecDeque<Delivery>,
    pub(super) incoming: Option<Result<Option<RawLine>, Error>>,
    pub(super) senders: usize,
    pub(super) receiver_open: bool,
    pub(super) accepting: bool,
    pub(super) stop: bool,
    pub(super) core_stopped: bool,
    pub(super) reader_stop: bool,
    pub(super) ready: bool,
    pub(super) finished: bool,
    pub(super) revision: u64,
}

pub(super) struct Shared {
    pub(super) state: Mutex<State>,
    pub(super) changed: Condvar,
    stdin: UnixStream,
    stdout: UnixStream,
    pub(super) stop: CancellationToken,
    callbacks: Mutex<Vec<CancellationRegistration>>,
}

impl Shared {
    pub(super) fn new(stdin: UnixStream, stdout: UnixStream) -> Arc<Self> {
        let shared = Arc::new(Self {
            state: Mutex::new(State {
                commands: VecDeque::new(),
                events: VecDeque::new(),
                incoming: None,
                senders: 1,
                receiver_open: true,
                accepting: true,
                stop: false,
                core_stopped: false,
                reader_stop: false,
                ready: false,
                finished: false,
                revision: 0,
            }),
            changed: Condvar::new(),
            stdin,
            stdout,
            stop: CancellationToken::new(),
            callbacks: Mutex::new(Vec::new()),
        });
        let weak = Arc::downgrade(&shared);
        lock(&shared.callbacks).push(shared.stop.on_cancel(move || {
            if let Some(shared) = weak.upgrade() {
                shared.interrupt(false);
            }
        }));
        shared
    }

    pub(super) fn watch_shutdown(self: &Arc<Self>, token: &CancellationToken) {
        let weak = Arc::downgrade(self);
        let registration = token.on_cancel(move || {
            if let Some(shared) = weak.upgrade() {
                shared.interrupt(true);
            }
        });
        lock(&self.callbacks).push(registration);
    }

    pub(super) fn notify(&self, state: &mut State) {
        state.revision = state.revision.wrapping_add(1);
        self.changed.notify_all();
    }

    pub(super) fn wake(&self) {
        self.notify(&mut lock(&self.state));
    }

    pub(super) fn interrupt(&self, core: bool) {
        {
            let mut state = lock(&self.state);
            state.stop = true;
            state.core_stopped |= core;
            state.reader_stop = true;
            self.notify(&mut state);
        }
        // Shutdown wakes blocked reads and writes; child cleanup owns errors.
        let _ = self.stdin.shutdown(Shutdown::Both);
        let _ = self.stdout.shutdown(Shutdown::Both);
    }

    pub(super) fn stop_reader(&self) {
        {
            let mut state = lock(&self.state);
            state.reader_stop = true;
            self.notify(&mut state);
        }
        let _ = self.stdout.shutdown(Shutdown::Both);
    }

    pub(super) fn wait(&self, revision: u64, deadline: Option<Instant>) {
        let mut state = lock(&self.state);
        while state.revision == revision && !state.finished {
            if let Some(deadline) = deadline {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    return;
                };
                let (next, timeout) = self
                    .changed
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(PoisonError::into_inner);
                state = next;
                if timeout.timed_out() {
                    return;
                }
            } else {
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
    }

    pub(super) fn revision(&self) -> u64 {
        lock(&self.state).revision
    }
}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = lock(&self.state);
        f.debug_struct("ControlQueues")
            .field("queued_commands", &state.commands.len())
            .field("queued_events", &state.events.len())
            .field("accepting", &state.accepting)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Completion {
    phase: AtomicU8,
    interested: AtomicBool,
    closed: AtomicBool,
    outcome: Mutex<Option<Result<BlockResult, Error>>>,
}

impl Completion {
    fn abandon(&self) {
        let _ = self
            .phase
            .compare_exchange(QUEUED, ABANDONED, Ordering::AcqRel, Ordering::Acquire);
        self.interested.store(false, Ordering::Release);
    }
}

#[derive(Debug)]
pub(super) struct ReplyPort {
    completion: Arc<Completion>,
    shared: Weak<Shared>,
}
impl Drop for ReplyPort {
    fn drop(&mut self) {
        self.completion.closed.store(true, Ordering::Release);
        if let Some(shared) = self.shared.upgrade() {
            shared.wake();
        }
    }
}
impl ReplyPort {
    pub(super) fn is_closed(&self) -> bool {
        !self.completion.interested.load(Ordering::Acquire)
    }
    pub(super) fn send(self, value: Result<BlockResult, Error>) {
        {
            let mut outcome = lock(&self.completion.outcome);
            if self.completion.interested.load(Ordering::Acquire) {
                *outcome = Some(value);
            }
        }
        if let Some(shared) = self.shared.upgrade() {
            shared.wake();
        }
    }
}

#[derive(Debug)]
pub(super) struct Request {
    pub(super) line: String,
    pub(super) deadline: Option<Instant>,
    pub(super) result: ReplyPort,
    pub(super) boundary: Option<Boundary>,
}
impl Request {
    pub(super) fn commit(self) -> Option<Self> {
        self.result
            .completion
            .phase
            .compare_exchange(QUEUED, COMMITTED, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        if let Some(shared) = self.result.shared.upgrade() {
            shared.wake();
        }
        Some(self)
    }
}

#[derive(Debug)]
pub(super) struct Sender {
    pub(super) shared: Arc<Shared>,
}
impl Clone for Sender {
    fn clone(&self) -> Self {
        lock(&self.shared.state).senders += 1;
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}
impl Drop for Sender {
    fn drop(&mut self) {
        let stop = {
            let mut state = lock(&self.shared.state);
            state.senders -= 1;
            self.shared.notify(&mut state);
            state.senders == 0 && !state.receiver_open
        };
        if stop {
            self.shared.interrupt(false);
        }
    }
}
impl Sender {
    pub(super) fn is_closed(&self) -> bool {
        !lock(&self.shared.state).accepting
    }

    pub(super) fn begin(
        &self,
        line: String,
        deadline: Option<Instant>,
        boundary: Option<Boundary>,
        cancellation: CancellationToken,
        request_id: u64,
        summary: CommandSummary,
    ) -> Submission {
        let completion = Arc::new(Completion {
            phase: AtomicU8::new(QUEUED),
            interested: AtomicBool::new(true),
            closed: AtomicBool::new(false),
            outcome: Mutex::new(None),
        });
        let weak = Arc::downgrade(&self.shared);
        let pending = Arc::clone(&completion);
        let registration = cancellation.on_cancel(move || {
            pending.abandon();
            if let Some(shared) = weak.upgrade() {
                shared.wake();
            }
        });
        let request = Request {
            line,
            deadline,
            boundary,
            result: ReplyPort {
                completion: Arc::clone(&completion),
                shared: Arc::downgrade(&self.shared),
            },
        };
        Submission {
            sender: self.clone(),
            request: Some(request),
            completion,
            deadline,
            cancellation,
            request_id,
            summary,
            _registration: registration,
        }
    }
}

/// An admitted or waiting call. Dropping it closes only its own result slot.
pub(super) struct Submission {
    sender: Sender,
    request: Option<Request>,
    completion: Arc<Completion>,
    deadline: Option<Instant>,
    cancellation: CancellationToken,
    request_id: u64,
    summary: CommandSummary,
    _registration: CancellationRegistration,
}
impl Submission {
    pub(super) fn poll(&mut self) -> Option<Result<BlockResult, Error>> {
        if let Some(outcome) = lock(&self.completion.outcome).take() {
            return Some(outcome);
        }
        if self.cancellation.is_cancelled() {
            self.completion.abandon();
            return Some(Err(Error::cancelled(self.request_id, self.summary.clone())));
        }
        if self.completion.closed.load(Ordering::Acquire) {
            return Some(Err(Error::control_mode_closed()));
        }
        if self.completion.phase.load(Ordering::Acquire) != COMMITTED
            && self
                .deadline
                .is_some_and(|deadline| deadline <= Instant::now())
        {
            self.completion.abandon();
            return Some(Err(Error::control_mode_dispatch_timeout()));
        }
        if self.request.is_some() {
            let mut state = lock(&self.sender.shared.state);
            if !state.accepting {
                return Some(Err(Error::control_mode_closed()));
            }
            if state.commands.len() < COMMAND_QUEUE {
                if let Some(request) = self.request.take() {
                    state.commands.push_back(request);
                }
                self.sender.shared.notify(&mut state);
            }
        }
        None
    }

    pub(super) fn wait_deadline(&self) -> Option<Instant> {
        (self.completion.phase.load(Ordering::Acquire) != COMMITTED)
            .then_some(self.deadline)
            .flatten()
    }

    pub(super) fn wait(mut self) -> Result<BlockResult, Error> {
        loop {
            let revision = self.sender.shared.revision();
            if let Some(outcome) = self.poll() {
                return outcome;
            }
            self.sender.shared.wait(revision, self.wait_deadline());
        }
    }
}
impl Drop for Submission {
    fn drop(&mut self) {
        self.completion.abandon();
        self.sender.shared.wake();
    }
}

#[derive(Debug)]
pub(super) struct Receiver {
    pub(super) shared: Arc<Shared>,
    pub(super) connection: Option<JoinHandle<Result<(), Error>>>,
}
impl Receiver {
    pub(super) fn revision(&self) -> u64 {
        self.shared.revision()
    }
    pub(super) fn wait(&self, revision: u64, deadline: Option<Instant>) {
        self.shared.wait(revision, deadline);
    }
    pub(super) fn try_delivery(&self) -> Option<Delivery> {
        let mut state = lock(&self.shared.state);
        let delivery = state.events.pop_front();
        if delivery.is_some() {
            self.shared.notify(&mut state);
        }
        delivery
    }
    pub(super) fn finished(&self) -> bool {
        lock(&self.shared.state).finished
    }
    pub(super) fn close(&self) {
        let mut state = lock(&self.shared.state);
        state.receiver_open = false;
        state.events.clear();
        self.shared.notify(&mut state);
    }
    pub(super) fn finish(&mut self) -> Result<(), Error> {
        match self.connection.take() {
            Some(handle) => match handle.join() {
                Ok(result) => result,
                Err(panic) => std::panic::resume_unwind(panic),
            },
            None => Ok(()),
        }
    }
    pub(super) fn shutdown(&mut self) -> Result<(), Error> {
        self.close();
        self.shared.stop.cancel();
        self.finish()
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.close();
        if lock(&self.shared.state).senders == 0 {
            self.shared.interrupt(false);
        }
    }
}

#[cfg(any(test, feature = "unstable-fuzzing"))]
#[derive(Debug)]
pub(super) struct ReplyReceiver(Arc<Completion>);

#[cfg(any(test, feature = "unstable-fuzzing"))]
#[derive(Debug, PartialEq)]
pub(super) enum TryRecvError {
    Empty,
    Closed,
}

#[cfg(any(test, feature = "unstable-fuzzing"))]
impl ReplyReceiver {
    pub(super) fn try_recv(&mut self) -> Result<Result<BlockResult, Error>, TryRecvError> {
        if let Some(outcome) = lock(&self.0.outcome).take() {
            return Ok(outcome);
        }
        Err(if self.0.closed.load(Ordering::Acquire) {
            TryRecvError::Closed
        } else {
            TryRecvError::Empty
        })
    }
}

#[cfg(any(test, feature = "unstable-fuzzing"))]
impl Drop for ReplyReceiver {
    fn drop(&mut self) {
        self.0.abandon();
    }
}

#[cfg(any(test, feature = "unstable-fuzzing"))]
pub(super) fn reply_channel() -> (ReplyPort, ReplyReceiver) {
    let completion = Arc::new(Completion {
        phase: AtomicU8::new(QUEUED),
        interested: AtomicBool::new(true),
        closed: AtomicBool::new(false),
        outcome: Mutex::new(None),
    });
    (
        ReplyPort {
            completion: Arc::clone(&completion),
            shared: Weak::new(),
        },
        ReplyReceiver(completion),
    )
}
