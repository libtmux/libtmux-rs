use std::collections::{VecDeque, hash_map::RandomState};
use std::hash::BuildHasher as _;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use super::channel::{EVENT_QUEUE, ReplyPort, Request, Shared, lock};
pub(super) use super::channel::{Receiver, Sender};
use super::protocol::{Line, RawLine, read_raw, write_request};
use super::{BlockResult, Boundary, Delivery, Event};
use crate::internal::process::PersistentChild;
use crate::{ControlLimits, Error, TmuxText};

pub(super) const HELD_WHILE_AWAITING: usize = EVENT_QUEUE * 8;
// The private unknown-command line remains below this bound for every u64.
const FENCE_LINE_LIMIT: usize = 80;

/// An ordered parse-error callback that cannot run a command alias or hook.
#[derive(Clone, Debug)]
pub(super) struct ReplyFence {
    command: String,
    output: TmuxText,
}

impl ReplyFence {
    fn new() -> Self {
        static SALT: OnceLock<u64> = OnceLock::new();
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let salt = SALT.get_or_init(|| RandomState::new().hash_one(std::process::id()));
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        Self::named(format!("_lt{salt:016x}{sequence:x}"))
    }

    fn named(command: String) -> Self {
        let output = TmuxText::from(format!("parse error: unknown command: {command}"));
        Self { command, output }
    }

    fn matches(&self, block: &BlockResult) -> bool {
        !block.succeeded() && block.output() == std::slice::from_ref(&self.output)
    }

    #[cfg(any(test, feature = "unstable-fuzzing"))]
    pub(super) fn block(&self) -> BlockResult {
        BlockResult {
            number: 0,
            succeeded: false,
            output: vec![self.output.clone()],
            sensitive_input: false,
            failed_lines: vec![true],
        }
    }
}

/// Reply ownership in the order tmux will pass each completion boundary.
#[derive(Debug)]
pub(super) enum ReplySlot {
    Live {
        result: ReplyPort,
        deadline: Option<Instant>,
        boundary: Option<Boundary>,
        fence: ReplyFence,
        received: Option<BlockResult>,
        bytes: usize,
    },
    /// Consume this request's replies even though nobody can receive them.
    Tombstone {
        deadline: Option<Instant>,
        boundary: Option<Boundary>,
        fence: ReplyFence,
    },
}

impl ReplySlot {
    const fn deadline(&self) -> Option<Instant> {
        match self {
            Self::Live { deadline, .. } | Self::Tombstone { deadline, .. } => *deadline,
        }
    }

    const fn fence(&self) -> &ReplyFence {
        match self {
            Self::Live { fence, .. } | Self::Tombstone { fence, .. } => fence,
        }
    }

    fn take_boundary(&mut self) -> Option<Boundary> {
        match self {
            Self::Live { boundary, .. } | Self::Tombstone { boundary, .. } => boundary.take(),
        }
    }

    fn take(&mut self, block: BlockResult, limit: usize) -> Result<(), Error> {
        if let Self::Live {
            received, bytes, ..
        } = self
        {
            for line in block.output() {
                *bytes = bytes.saturating_add(line.as_bytes().len().saturating_add(1));
            }
            if *bytes > limit {
                return Err(Error::control_mode_frame_too_large("reply", limit));
            }
            *received = Some(match received.take() {
                Some(earlier) => earlier.followed_by(block),
                None => block,
            });
        }
        Ok(())
    }
}

/// Ordered requests, including replies whose callers were refused.
#[derive(Debug, Default)]
pub(super) struct ReplySlots {
    pub(super) slots: VecDeque<ReplySlot>,
    live: usize,
    earliest: Option<Instant>,
}

impl ReplySlots {
    #[cfg(any(test, feature = "unstable-fuzzing"))]
    pub(super) fn push(&mut self, result: ReplyPort, deadline: Option<Instant>) -> ReplyFence {
        let fence = ReplyFence::new();
        self.push_ordered(result, deadline, None, fence.clone());
        fence
    }

    fn push_ordered(
        &mut self,
        result: ReplyPort,
        deadline: Option<Instant>,
        boundary: Option<Boundary>,
        fence: ReplyFence,
    ) {
        self.earliest = earliest_deadline(self.earliest, deadline);
        if result.is_closed() {
            self.slots.push_back(ReplySlot::Tombstone {
                deadline,
                boundary,
                fence,
            });
        } else {
            self.slots.push_back(ReplySlot::Live {
                result,
                deadline,
                boundary,
                fence,
                received: None,
                bytes: 0,
            });
            self.live += 1;
        }
    }

    pub(super) const fn has_live(&self) -> bool {
        self.live != 0
    }

    fn has_slots(&self) -> bool {
        !self.slots.is_empty()
    }

    pub(super) const fn earliest_deadline(&self) -> Option<Instant> {
        self.earliest
    }

    fn block_deadline(&self, timeout: Duration) -> Option<Instant> {
        if self.has_slots() {
            self.earliest
        } else {
            Instant::now().checked_add(timeout)
        }
    }

    pub(super) fn refuse_live(&mut self) {
        for slot in &mut self.slots {
            let ReplySlot::Live {
                deadline,
                boundary,
                fence,
                ..
            } = slot
            else {
                continue;
            };
            let tombstone = ReplySlot::Tombstone {
                deadline: *deadline,
                boundary: *boundary,
                fence: fence.clone(),
            };
            if let ReplySlot::Live { result, .. } = std::mem::replace(slot, tombstone) {
                result.send(Err(Error::control_mode_unread()));
            }
        }
        self.live = 0;
    }

    pub(super) fn consume(
        &mut self,
        block: BlockResult,
        limit: usize,
    ) -> Result<Option<Boundary>, Error> {
        let Some(front) = self.slots.front_mut() else {
            return Ok(None);
        };
        if !front.fence().matches(&block) {
            let boundary = front.take_boundary();
            front.take(block, limit)?;
            return Ok(boundary);
        }
        let Some(slot) = self.slots.pop_front() else {
            return Ok(None);
        };
        let deadline = slot.deadline();
        if let ReplySlot::Live {
            result, received, ..
        } = slot
        {
            self.live -= 1;
            let empty = || BlockResult {
                number: block.number(),
                succeeded: true,
                output: Vec::new(),
                sensitive_input: false,
                failed_lines: Vec::new(),
            };
            result.send(Ok(received.unwrap_or_else(empty)));
        }
        if deadline == self.earliest {
            self.earliest = self.slots.iter().filter_map(ReplySlot::deadline).min();
        }
        Ok(None)
    }

    #[cfg(test)]
    pub(super) fn complete(&mut self, block: BlockResult) {
        let fence = self.slots.front().map(|slot| slot.fence().block());
        self.consume(block, usize::MAX)
            .expect("unbounded test reply fits");
        if let Some(fence) = fence {
            self.consume(fence, usize::MAX)
                .expect("fence completes request");
        }
    }

    fn fail_all(&mut self, mut reason: impl FnMut() -> Error) {
        while let Some(slot) = self.slots.pop_front() {
            if let ReplySlot::Live { result, .. } = slot {
                result.send(Err(reason()));
            }
        }
        self.live = 0;
        self.earliest = None;
    }
}

pub(super) struct OpenedConnection {
    pub(super) commands: Sender,
    pub(super) events: Receiver,
}

pub(super) fn open(
    mut child: PersistentChild,
    limits: ControlLimits,
    timeout: Duration,
    cancellation: &crate::CancellationToken,
) -> Result<OpenedConnection, Error> {
    let setup = (|| {
        let stdin = child.take_stdin().ok_or_else(Error::control_mode_pipes)?;
        let stdout = child.take_stdout().ok_or_else(Error::control_mode_pipes)?;
        let shared = Shared::new(
            stdin.try_clone().map_err(Error::control_mode)?,
            stdout.try_clone().map_err(Error::control_mode)?,
        );
        shared.watch_shutdown(&child.shutdown_token());
        Ok::<_, Error>((stdin, stdout, shared))
    })();
    let (stdin, stdout, shared) = match setup {
        Ok(parts) => parts,
        Err(error) => {
            child.terminate()?;
            return Err(error);
        }
    };
    let cancelled = child.cancelled_error();
    let weak = Arc::downgrade(&shared);
    let _attach_cancel = cancellation.on_cancel(move || {
        if let Some(shared) = weak.upgrade() {
            shared.interrupt(false);
        }
    });
    let read_shared = Arc::clone(&shared);
    let reader = match thread::Builder::new()
        .name("libtmux-control-reader".into())
        .spawn(move || read_wire(stdout, &read_shared, limits))
    {
        Ok(reader) => reader,
        Err(error) => {
            child.terminate()?;
            return Err(Error::control_mode(error));
        }
    };
    let parts = Arc::new(Mutex::new(Some((
        Connection {
            child,
            stdin,
            shared: Arc::clone(&shared),
            limits,
            timeout,
            opening_deadline: Instant::now().checked_add(timeout),
            opened: false,
            frame: None,
            awaiting: ReplySlots::default(),
            pending: VecDeque::new(),
        },
        reader,
    ))));
    let worker_parts = Arc::clone(&parts);
    let handle = thread::Builder::new()
        .name("libtmux-control".into())
        .spawn(move || {
            let Some((connection, reader)) = lock(&worker_parts).take() else {
                return Err(Error::control_mode_closed());
            };
            connection.run_to_completion(reader)
        });
    let handle = match handle {
        Ok(handle) => handle,
        Err(error) => {
            shared.interrupt(false);
            if let Some((mut connection, reader)) = lock(&parts).take() {
                let cleanup = connection.child.terminate();
                if let Err(panic) = reader.join() {
                    resume_unwind(panic);
                }
                cleanup?;
            }
            return Err(Error::control_mode(error));
        }
    };
    let commands = Sender {
        shared: Arc::clone(&shared),
    };
    let mut events = Receiver {
        shared,
        connection: Some(handle),
    };
    loop {
        let revision = events.revision();
        if cancellation.is_cancelled() {
            events.shutdown()?;
            return Err(cancelled);
        }
        let ready = lock(&events.shared.state).ready;
        if ready {
            return Ok(OpenedConnection { commands, events });
        }
        if events.finished() {
            return match events.finish() {
                Err(error) => Err(error),
                Ok(()) => Err(Error::control_mode_closed()),
            };
        }
        events.wait(revision, None);
    }
}

fn read_wire(stdout: UnixStream, shared: &Shared, limits: ControlLimits) {
    let result = catch_unwind(AssertUnwindSafe(|| read_lines(stdout, shared, limits)));
    if let Err(panic) = result {
        publish_line(shared, Err(Error::control_mode_closed()));
        resume_unwind(panic);
    }
}

fn read_lines(stdout: UnixStream, shared: &Shared, limits: ControlLimits) {
    let mut stdout = BufReader::new(stdout);
    let physical_limit = limits.max_line_bytes.max(FENCE_LINE_LIMIT);
    loop {
        if lock(&shared.state).reader_stop {
            return;
        }
        let line = read_raw(&mut stdout, physical_limit).map_err(|error| match error {
            Error::ControlModeFrameTooLarge { frame: "line", .. } => {
                Error::control_mode_frame_too_large("line", limits.max_line_bytes)
            }
            error => error,
        });
        let ended = !matches!(line, Ok(Some(_)));
        if !publish_line(shared, line) || ended {
            return;
        }
    }
}

fn publish_line(shared: &Shared, line: Result<Option<RawLine>, Error>) -> bool {
    let mut state = lock(&shared.state);
    while state.incoming.is_some() && !state.reader_stop {
        state = shared
            .changed
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    if state.reader_stop {
        return false;
    }
    state.incoming = Some(line);
    shared.notify(&mut state);
    true
}

#[derive(Clone, Copy)]
enum Terminal {
    Closed,
    Shutdown,
    Timeout,
    Frame(&'static str, usize),
}
impl Terminal {
    fn from_outcome(outcome: &Result<(), Error>) -> Self {
        match outcome {
            Err(Error::ControlModeFrameTooLarge { frame, limit }) => Self::Frame(frame, *limit),
            Err(Error::ControlMode {
                kind: crate::ControlModeErrorKind::TimedOut,
                ..
            }) => Self::Timeout,
            Err(Error::ExecutorShutdown { .. }) => Self::Shutdown,
            _ => Self::Closed,
        }
    }
    fn error(self, child: &PersistentChild) -> Error {
        match self {
            Self::Closed => Error::control_mode_closed(),
            Self::Shutdown => child.shutdown_error(),
            Self::Timeout => Error::control_mode_timeout(),
            Self::Frame(frame, limit) => Error::control_mode_frame_too_large(frame, limit),
        }
    }
}

struct Frame {
    number: u64,
    flags: u64,
    output: Vec<TmuxText>,
    bytes: usize,
    deadline: Option<Instant>,
}

struct Connection {
    child: PersistentChild,
    stdin: UnixStream,
    shared: Arc<Shared>,
    limits: ControlLimits,
    timeout: Duration,
    opening_deadline: Option<Instant>,
    opened: bool,
    frame: Option<Frame>,
    awaiting: ReplySlots,
    pending: VecDeque<Delivery>,
}

enum Step {
    Line(Result<Option<RawLine>, Error>),
    Send(Request),
    Stop,
    CoreStop,
    TimedOut,
}

impl Connection {
    fn run_to_completion(mut self, reader: thread::JoinHandle<()>) -> Result<(), Error> {
        let outcome = catch_unwind(AssertUnwindSafe(|| self.run()));
        if outcome.is_err() {
            self.fail_requests(Terminal::Closed);
        }
        self.shared.stop_reader();
        let cleanup = self.child.terminate();
        let read_join = reader.join();
        {
            let mut state = lock(&self.shared.state);
            state.finished = true;
            self.shared.notify(&mut state);
        }
        if let Err(panic) = read_join {
            resume_unwind(panic);
        }
        match outcome {
            Err(panic) => resume_unwind(panic),
            Ok(Err(error)) => Err(error),
            Ok(Ok(())) => cleanup,
        }
    }

    fn run(&mut self) -> Result<(), Error> {
        let outcome = self.serve();
        self.fail_requests(Terminal::from_outcome(&outcome));
        if self.opened {
            self.drain_pending();
        }
        outcome
    }

    fn fail_requests(&mut self, terminal: Terminal) {
        let requests = {
            let mut state = lock(&self.shared.state);
            state.accepting = false;
            let requests = state.commands.drain(..).collect::<Vec<_>>();
            self.shared.notify(&mut state);
            requests
        };
        self.awaiting.fail_all(|| terminal.error(&self.child));
        for request in requests {
            request.result.send(Err(terminal.error(&self.child)));
        }
    }

    fn flush_pending(&mut self, state: &mut super::channel::State) {
        if !state.receiver_open {
            self.pending.clear();
            return;
        }
        while state.events.len() < EVENT_QUEUE {
            let Some(delivery) = self.pending.pop_front() else {
                break;
            };
            state.events.push_back(delivery);
            self.shared.notify(state);
        }
    }

    fn drain_pending(&mut self) {
        while !self.pending.is_empty() {
            let shared = Arc::clone(&self.shared);
            let mut state = lock(&shared.state);
            if state.stop || !state.receiver_open {
                return;
            }
            self.flush_pending(&mut state);
            if self.pending.is_empty() {
                return;
            }
            drop(
                shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }
    }

    fn serve(&mut self) -> Result<(), Error> {
        loop {
            if self.awaiting.has_live() && self.pending.len() >= HELD_WHILE_AWAITING {
                self.awaiting.refuse_live();
            }
            let deadline = if self.opened {
                earliest_deadline(
                    self.awaiting.earliest_deadline(),
                    self.frame.as_ref().and_then(|frame| frame.deadline),
                )
            } else {
                self.opening_deadline
            };
            let step = {
                let shared = Arc::clone(&self.shared);
                let mut state = lock(&shared.state);
                self.flush_pending(&mut state);
                if state.core_stopped {
                    Step::CoreStop
                } else if state.stop || (state.senders == 0 && !state.receiver_open) {
                    Step::Stop
                } else if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
                    Step::TimedOut
                } else {
                    let held_back = if self.opened {
                        !self.awaiting.has_live() && self.pending.len() >= EVENT_QUEUE
                    } else {
                        state.events.len() >= EVENT_QUEUE
                    };
                    if !held_back && state.incoming.is_some() {
                        let line = state.incoming.take();
                        shared.notify(&mut state);
                        match line {
                            Some(line) => Step::Line(line),
                            None => continue,
                        }
                    } else if self.opened && !state.commands.is_empty() && self.frame.is_none() {
                        let request = state.commands.pop_front();
                        shared.notify(&mut state);
                        match request {
                            Some(request) => Step::Send(request),
                            None => continue,
                        }
                    } else {
                        let revision = state.revision;
                        drop(state);
                        shared.wait(revision, deadline);
                        continue;
                    }
                }
            };
            match step {
                Step::Stop => return Ok(()),
                Step::CoreStop => return Err(self.child.shutdown_error()),
                Step::TimedOut => return Err(Error::control_mode_timeout()),
                Step::Line(Err(error)) => return Err(error),
                Step::Line(Ok(None)) => return Err(Error::control_mode_closed()),
                Step::Line(Ok(Some(line))) => {
                    if !self.consume_line(&line)? {
                        return Ok(());
                    }
                }
                Step::Send(request) => self.send(request)?,
            }
        }
    }

    fn send(&mut self, request: Request) -> Result<(), Error> {
        if request
            .deadline
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            request
                .result
                .send(Err(Error::control_mode_dispatch_timeout()));
            return Ok(());
        }
        let Some(request) = admit_request(request, self.pending.len()) else {
            return Ok(());
        };
        let Some(request) = request.commit() else {
            return Ok(());
        };
        let fence = ReplyFence::new();
        let deadline = earliest_deadline(self.awaiting.earliest_deadline(), request.deadline);
        let written = write_request(&mut self.stdin, &request.line, &fence.command, deadline);
        if let Err(error) = written {
            let state = lock(&self.shared.state);
            let outcome = if state.core_stopped {
                Err(self.child.shutdown_error())
            } else if state.stop {
                Ok(())
            } else {
                Err(error)
            };
            drop(state);
            request
                .result
                .send(Err(Terminal::from_outcome(&outcome).error(&self.child)));
            return outcome;
        }
        self.awaiting
            .push_ordered(request.result, request.deadline, request.boundary, fence);
        Ok(())
    }

    fn report(&mut self, delivery: Delivery) {
        let mut state = lock(&self.shared.state);
        if !state.receiver_open {
            return;
        }
        if self.pending.is_empty() && state.events.len() < EVENT_QUEUE {
            state.events.push_back(delivery);
            self.shared.notify(&mut state);
        } else {
            self.pending.push_back(delivery);
        }
    }

    fn finish_frame(&mut self, frame: Frame, succeeded: bool) -> Result<(), Error> {
        if !self.opened {
            if !succeeded {
                return Err(Error::control_mode_closed());
            }
            self.opened = true;
            let mut state = lock(&self.shared.state);
            state.ready = true;
            self.shared.notify(&mut state);
            return Ok(());
        }
        let block = BlockResult {
            number: frame.number,
            succeeded,
            failed_lines: vec![!succeeded; frame.output.len()],
            output: frame.output,
            sensitive_input: false,
        };
        let fence = self
            .awaiting
            .slots
            .front()
            .is_some_and(|slot| slot.fence().matches(&block));
        if frame.bytes > self.limits.max_block_bytes && !fence {
            return Err(Error::control_mode_frame_too_large(
                "block",
                self.limits.max_block_bytes,
            ));
        }
        if frame.flags & 1 != 0
            && let Some(boundary) = self.awaiting.consume(block, self.limits.max_block_bytes)?
        {
            self.report(Delivery::Boundary(boundary));
        }
        Ok(())
    }

    fn consume_line(&mut self, line: &RawLine) -> Result<bool, Error> {
        if let Some(mut frame) = self.frame.take() {
            match Line::parse_within_block(&line.bytes, frame.number) {
                Line::BlockEnd { succeeded, .. } => self.finish_frame(frame, succeeded)?,
                Line::Text(text) => {
                    let fence = self.opened
                        && frame.output.is_empty()
                        && self
                            .awaiting
                            .slots
                            .front()
                            .is_some_and(|slot| text == slot.fence().output);
                    if line.wire_bytes > self.limits.max_line_bytes && !fence {
                        return Err(Error::control_mode_frame_too_large(
                            "line",
                            self.limits.max_line_bytes,
                        ));
                    }
                    frame.bytes = frame
                        .bytes
                        .saturating_add(text.as_bytes().len().saturating_add(1));
                    if frame.bytes > self.limits.max_block_bytes && !fence {
                        return Err(Error::control_mode_frame_too_large(
                            "block",
                            self.limits.max_block_bytes,
                        ));
                    }
                    if self.opened {
                        frame.output.push(text);
                    }
                    self.frame = Some(frame);
                }
                _ => unreachable!("within-block parsing returns only text or a terminator"),
            }
            return Ok(true);
        }
        if line.wire_bytes > self.limits.max_line_bytes {
            return Err(Error::control_mode_frame_too_large(
                "line",
                self.limits.max_line_bytes,
            ));
        }
        match Line::parse(&line.bytes) {
            Line::BlockStart { number, flags } => {
                self.frame = Some(Frame {
                    number,
                    flags,
                    output: Vec::new(),
                    bytes: 0,
                    deadline: if self.opened {
                        self.awaiting.block_deadline(self.timeout)
                    } else {
                        self.opening_deadline
                    },
                });
            }
            Line::Event(exit @ Event::Exit { .. }) => {
                self.report(Delivery::Event(exit));
                if !self.opened {
                    return Err(Error::control_mode_closed());
                }
                return Ok(false);
            }
            Line::Event(event) => self.report(Delivery::Event(event)),
            Line::Text(_) | Line::BlockEnd { .. } => {}
        }
        Ok(true)
    }
}

fn earliest_deadline(left: Option<Instant>, right: Option<Instant>) -> Option<Instant> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}

pub(super) fn admit_request(request: Request, pending_events: usize) -> Option<Request> {
    if pending_events < HELD_WHILE_AWAITING {
        Some(request)
    } else {
        request.result.send(Err(Error::control_mode_unread()));
        None
    }
}
