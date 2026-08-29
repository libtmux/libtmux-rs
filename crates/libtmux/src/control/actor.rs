use std::collections::VecDeque;
use std::time::Duration;

use tokio::io::BufReader;
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use super::protocol::{Line, read_line, read_line_within, write_line};
use super::{BlockResult, Event};
use crate::Error;
use crate::internal::process::PersistentChild;
use crate::limits::ControlLimits;

/// How many commands may queue before a sender waits.
const COMMAND_QUEUE: usize = 16;

/// How many events may buffer before the connection stops reading tmux.
const EVENT_QUEUE: usize = 256;

/// How many events may be held while a reply is outstanding.
///
/// Reading continues while something is waiting for a reply, because the reply
/// arrives on the connection that would otherwise pause. That is bounded by
/// how long a reply takes, and tmux answers most commands at once -- but not
/// all. `run-shell` without `-b` answers its own block immediately and then
/// parks the queue for as long as its shell command runs, so the next command
/// sent is outstanding for that long and this end would hold events for the
/// duration. A ceiling turns that into a pause rather than a memory leak.
pub(super) const HELD_WHILE_AWAITING: usize = EVENT_QUEUE * 8;

/// The public handles backed by one running connection actor.
pub(super) struct OpenedConnection {
    pub(super) commands: mpsc::Sender<Request>,
    pub(super) events: mpsc::Receiver<Event>,
    pub(super) stop: watch::Sender<()>,
    pub(super) connection: tokio::task::JoinHandle<Result<(), Error>>,
}

/// Take ownership of a control process and wait until tmux has attached it.
pub(super) async fn open(
    mut child: PersistentChild,
    limits: ControlLimits,
    timeout: Duration,
) -> Result<OpenedConnection, Error> {
    let Some(stdin) = child.take_stdin() else {
        let _ = child.terminate().await;
        return Err(Error::control_mode_pipes());
    };
    let Some(stdout) = child.take_stdout() else {
        let _ = child.terminate().await;
        return Err(Error::control_mode_pipes());
    };
    let core_stopped = child.stopped();

    let (commands, queue) = mpsc::channel(COMMAND_QUEUE);
    let (events, received) = mpsc::channel(EVENT_QUEUE);
    let (stop, stopped) = watch::channel(());
    let actor = Connection {
        child,
        stdin,
        stdout: BufReader::new(stdout),
        limits,
        timeout,
        line: Vec::new(),
        commands: queue,
        events,
        stopped,
        core_stopped,
        awaiting: ReplySlots::default(),
        pending: VecDeque::new(),
    };

    let (ready, mut opened) = oneshot::channel();
    let mut connection = tokio::spawn(actor.run(ready));
    tokio::select! {
        biased;
        result = &mut opened => {
            if result.is_err() {
                return match connection.await {
                    Ok(Err(error)) => Err(error),
                    Ok(Ok(())) | Err(_) => Err(Error::control_mode_closed()),
                };
            }
        }
        result = &mut connection => {
            return match result {
                Ok(Err(error)) => Err(error),
                Ok(Ok(())) | Err(_) => Err(Error::control_mode_closed()),
            };
        }
    }

    Ok(OpenedConnection {
        commands,
        events: received,
        stop,
        connection,
    })
}

/// One command waiting for its result block.
#[derive(Debug)]
pub(super) struct Request {
    pub(super) line: String,
    pub(super) deadline: Option<Instant>,
    pub(super) result: oneshot::Sender<Result<BlockResult, Error>>,
    pub(super) commit: oneshot::Sender<()>,
}

/// A request whose caller can no longer prevent the first write.
#[derive(Debug)]
pub(super) struct CommittedRequest {
    pub(super) line: String,
    pub(super) deadline: Option<Instant>,
    pub(super) result: oneshot::Sender<Result<BlockResult, Error>>,
}

impl Request {
    pub(super) fn commit(self) -> Option<CommittedRequest> {
        let Self {
            line,
            deadline,
            result,
            commit,
        } = self;
        commit.send(()).ok()?;
        Some(CommittedRequest {
            line,
            deadline,
            result,
        })
    }
}

pub(super) fn admit_request(request: Request, pending_events: usize) -> Option<Request> {
    if pending_events < HELD_WHILE_AWAITING {
        return Some(request);
    }

    let _ = request.result.send(Err(Error::control_mode_unread()));
    None
}

/// Reply ownership in the order tmux will answer it.
#[derive(Debug)]
pub(super) enum ReplySlot {
    Live {
        result: oneshot::Sender<Result<BlockResult, Error>>,
        deadline: Option<Instant>,
    },
    /// Consume this block without giving it to a later caller.
    Tombstone { deadline: Option<Instant> },
}

impl ReplySlot {
    const fn deadline(&self) -> Option<Instant> {
        match self {
            Self::Live { deadline, .. } | Self::Tombstone { deadline } => *deadline,
        }
    }
}

/// Ordered reply slots, including blocks whose callers were refused.
#[derive(Debug, Default)]
pub(super) struct ReplySlots {
    pub(super) slots: VecDeque<ReplySlot>,
    live: usize,
    earliest: Option<Instant>,
}

impl ReplySlots {
    pub(super) fn push(
        &mut self,
        result: oneshot::Sender<Result<BlockResult, Error>>,
        deadline: Option<Instant>,
    ) {
        self.earliest = earliest_deadline(self.earliest, deadline);
        if result.is_closed() {
            self.slots.push_back(ReplySlot::Tombstone { deadline });
        } else {
            self.slots.push_back(ReplySlot::Live { result, deadline });
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
            let deadline = slot.deadline();
            let ReplySlot::Live { result, .. } =
                std::mem::replace(slot, ReplySlot::Tombstone { deadline })
            else {
                continue;
            };
            let _ = result.send(Err(Error::control_mode_unread()));
        }
        self.live = 0;
    }

    pub(super) fn complete(&mut self, block: BlockResult) {
        let Some(slot) = self.slots.pop_front() else {
            return;
        };
        let deadline = slot.deadline();
        if let ReplySlot::Live { result, .. } = slot {
            self.live -= 1;
            let _ = result.send(Ok(block));
        }
        if deadline == self.earliest {
            self.earliest = self.slots.iter().filter_map(ReplySlot::deadline).min();
        }
    }

    fn fail_all(&mut self, mut reason: impl FnMut() -> Error) {
        while let Some(slot) = self.slots.pop_front() {
            if let ReplySlot::Live { result, .. } = slot {
                let _ = result.send(Err(reason()));
            }
        }
        self.live = 0;
        self.earliest = None;
    }
}

/// What one turn of the connection loop found to do.
enum Step {
    /// The watching half has room for one held event, or has gone.
    Deliver(bool),
    Read(Result<Option<Line>, Error>),
    Send(Option<Request>),
    /// The watching half asked to stop, or went away.
    Unwatched {
        asked: bool,
    },
    CoreStopped,
    TimedOut,
}

enum BlockRead {
    Complete(BlockResult),
    Stopped,
}

#[derive(Clone, Copy)]
enum TerminalError {
    Closed,
    Frame(&'static str, usize),
    Shutdown,
    TimedOut,
}

impl TerminalError {
    fn build(self, child: &PersistentChild) -> Error {
        match self {
            Self::Closed => Error::control_mode_closed(),
            Self::Frame(frame, limit) => Error::control_mode_frame_too_large(frame, limit),
            Self::Shutdown => child.shutdown_error(),
            Self::TimedOut => Error::control_mode_timeout(),
        }
    }
}

/// The task that owns the pipes and multiplexes both directions.
struct Connection {
    child: PersistentChild,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// What one line and one block may accumulate before this gives up.
    limits: ControlLimits,
    timeout: Duration,
    /// Bytes of a line that is not complete yet.
    ///
    /// This outlives one read because a cancelled read leaves what it got
    /// here, and the next read continues from it.
    line: Vec<u8>,
    commands: mpsc::Receiver<Request>,
    events: mpsc::Sender<Event>,
    /// Resolves when the watching half asks to stop, or is dropped.
    stopped: watch::Receiver<()>,
    core_stopped: watch::Receiver<bool>,
    /// Commands whose result block has not arrived yet.
    ///
    /// tmux answers in order and blocks do not nest, so the front of this
    /// queue owns the next block that completes.
    awaiting: ReplySlots,
    /// Events tmux has reported that the caller has not taken yet.
    ///
    /// The reader puts an event here rather than waiting for the caller to
    /// have room, because waiting would stop it reading the connection, and
    /// the connection is where a caller's reply comes from.
    pending: VecDeque<Event>,
}

impl Connection {
    async fn run(mut self, mut ready: oneshot::Sender<()>) -> Result<(), Error> {
        let opening_deadline = Instant::now().checked_add(self.timeout);
        let outcome = match self
            .discard_opening_block(&mut ready, opening_deadline)
            .await
        {
            Ok(true) if ready.send(()).is_ok() => self.serve().await,
            Ok(_) => Ok(()),
            Err(error) => Err(error),
        };

        // Whatever is still waiting will never be answered. It is told why
        // where the reason is more specific than "closed": a caller who blew
        // a frame budget can raise it, where one who merely lost the
        // connection can only reconnect.
        let reason = match &outcome {
            Err(Error::ControlModeFrameTooLarge { frame, limit }) => {
                TerminalError::Frame(frame, *limit)
            }
            Err(Error::ControlMode {
                kind: crate::ControlModeErrorKind::TimedOut,
                ..
            }) => TerminalError::TimedOut,
            Err(Error::ExecutorShutdown { .. }) => TerminalError::Shutdown,
            _ => TerminalError::Closed,
        };
        let child = &self.child;
        self.awaiting.fail_all(|| reason.build(child));
        while let Ok(request) = self.commands.try_recv() {
            let _ = request.result.send(Err(reason.build(child)));
        }
        drop(self.stdin);
        let cleanup = self.child.terminate().await;

        match outcome {
            Err(error) => Err(error),
            Ok(()) => cleanup,
        }
    }

    async fn serve(&mut self) -> Result<(), Error> {
        // The connection outlives either half on its own: a caller who only
        // watches drops the sender, and a caller who only sends drops the
        // events. It ends when both are gone, when the watcher asks, or when
        // tmux hangs up.
        let mut sending = true;
        let mut watching = true;

        while sending || watching {
            if self.awaiting.has_live() && self.pending.len() >= HELD_WHILE_AWAITING {
                self.awaiting.refuse_live();
            }

            let held_back = !self.awaiting.has_live() && self.pending.len() >= EVENT_QUEUE;
            let reply_deadline = self.awaiting.earliest_deadline();

            let step = tokio::select! {
                line = read_line(&mut self.stdout, &mut self.line, self.limits.max_line_bytes),
                    if !held_back => Step::Read(line),
                room = self.events.reserve(), if !self.pending.is_empty() => Step::Deliver(room.is_ok()),
                request = self.commands.recv(), if sending => Step::Send(request),
                asked = self.stopped.changed(), if watching => Step::Unwatched {
                    asked: asked.is_ok(),
                },
                () = cancellation_requested(&mut self.core_stopped) => Step::CoreStopped,
                () = deadline_elapsed(reply_deadline), if self.awaiting.has_slots() => Step::TimedOut,
            };

            match step {
                Step::Read(Err(error)) => return Err(error),
                // tmux hung up, or the watcher asked to stop. Either ends the
                // connection whatever the other half is doing.
                Step::Read(Ok(None)) | Step::Unwatched { asked: true } => {
                    return Ok(());
                }
                Step::CoreStopped => return Err(self.child.shutdown_error()),
                Step::TimedOut => return Err(Error::control_mode_timeout()),
                Step::Read(Ok(Some(line))) => {
                    if !self.dispatch(line, &mut watching).await? {
                        return Ok(());
                    }
                }
                Step::Send(Some(request)) => {
                    if request
                        .deadline
                        .is_some_and(|deadline| deadline <= Instant::now())
                    {
                        let _ = request
                            .result
                            .send(Err(Error::control_mode_dispatch_timeout()));
                        continue;
                    }
                    let Some(request) = admit_request(request, self.pending.len()) else {
                        continue;
                    };
                    let Some(request) = request.commit() else {
                        continue;
                    };
                    let write_deadline = earliest_deadline(reply_deadline, request.deadline);
                    let write = write_line(&mut self.stdin, &request.line);
                    tokio::pin!(write);
                    loop {
                        let result = tokio::select! {
                            biased;
                            () = cancellation_requested(&mut self.core_stopped) => {
                                let _ = request.result.send(Err(self.child.shutdown_error()));
                                return Err(self.child.shutdown_error());
                            }
                            changed = self.stopped.changed(), if watching => {
                                if changed.is_ok() {
                                    let _ = request.result.send(Err(Error::control_mode_closed()));
                                    return Ok(());
                                }
                                watching = false;
                                continue;
                            }
                            () = deadline_elapsed(write_deadline) => {
                                let _ = request.result.send(Err(Error::control_mode_timeout()));
                                return Err(Error::control_mode_timeout());
                            }
                            result = &mut write => result,
                        };
                        if let Err(error) = result {
                            let _ = request.result.send(Err(Error::control_mode_closed()));
                            return Err(error);
                        }
                        break;
                    }
                    self.awaiting.push(request.result, request.deadline);
                }
                // The caller took one, so the next one can go.
                Step::Deliver(true) => {
                    if let Some(event) = self.pending.pop_front() {
                        let _ = self.events.try_send(event);
                    }
                }
                // Nobody is watching any more. What is held becomes
                // unreachable rather than undelivered, and the connection
                // carries on for whoever is still sending.
                Step::Deliver(false) => self.pending.clear(),
                // Every sender is gone, so no further commands can arrive.
                Step::Send(None) => sending = false,
                // The watching handle was dropped rather than asked to stop,
                // which leaves any sender still working.
                Step::Unwatched { asked: false } => watching = false,
            }
        }

        Ok(())
    }

    /// Consume the block tmux answers an attach with.
    ///
    /// tmux writes this once the client is attached, before it has read
    /// anything from this end, so it replies to nothing. Correlation is by
    /// arrival order, and leaving this block to the serving loop would hand it
    /// to the first command's caller as that command's result -- an empty
    /// success, whatever the command was.
    async fn discard_opening_block(
        &mut self,
        ready: &mut oneshot::Sender<()>,
        deadline: Option<Instant>,
    ) -> Result<bool, Error> {
        loop {
            let held_back = self.events.capacity() == 0;
            let line = tokio::select! {
                biased;
                () = ready.closed() => return Ok(false),
                () = cancellation_requested(&mut self.core_stopped) => {
                    return Err(self.child.shutdown_error());
                }
                () = deadline_elapsed(deadline) => {
                    return Err(Error::control_mode_timeout());
                }
                line = read_line(
                    &mut self.stdout,
                    &mut self.line,
                    self.limits.max_line_bytes,
                ), if !held_back => line?,
            };
            match line {
                Some(Line::BlockStart(number)) => {
                    return match self.read_opening_block(number, ready, deadline).await? {
                        Some(true) => Ok(true),
                        Some(false) => Err(Error::control_mode_closed()),
                        None => Ok(false),
                    };
                }
                Some(Line::Event(exit @ Event::Exit { .. })) => {
                    let _ = self.events.try_send(exit);
                    return Err(Error::control_mode_closed());
                }
                Some(Line::Event(event)) => {
                    let _ = self.events.try_send(event);
                }
                Some(Line::Text(_) | Line::BlockEnd { .. }) => {}
                None => return Err(Error::control_mode_closed()),
            }
        }
    }

    async fn read_opening_block(
        &mut self,
        number: u64,
        ready: &mut oneshot::Sender<()>,
        deadline: Option<Instant>,
    ) -> Result<Option<bool>, Error> {
        let mut accumulated = 0usize;
        loop {
            let line = tokio::select! {
                biased;
                () = ready.closed() => return Ok(None),
                () = cancellation_requested(&mut self.core_stopped) => {
                    return Err(self.child.shutdown_error());
                }
                () = deadline_elapsed(deadline) => {
                    return Err(Error::control_mode_timeout());
                }
                line = read_line_within(
                    &mut self.stdout,
                    &mut self.line,
                    self.limits.max_line_bytes,
                    Some(number),
                ) => line?,
            };
            match line {
                Some(Line::BlockEnd {
                    number: end,
                    succeeded,
                }) if end == number => return Ok(Some(succeeded)),
                Some(Line::Text(text)) => {
                    accumulated = accumulated.saturating_add(text.as_bytes().len());
                    if accumulated > self.limits.max_block_bytes {
                        return Err(Error::control_mode_frame_too_large(
                            "block",
                            self.limits.max_block_bytes,
                        ));
                    }
                }
                Some(Line::Event(_) | Line::BlockStart(_) | Line::BlockEnd { .. }) => {}
                None => return Err(Error::control_mode_closed()),
            }
        }
    }

    /// Act on one protocol line, reporting whether to keep reading.
    async fn dispatch(&mut self, line: Line, watching: &mut bool) -> Result<bool, Error> {
        match line {
            Line::BlockStart(number) => {
                let deadline = self.awaiting.block_deadline(self.timeout);
                match self.read_block(number, deadline, watching).await? {
                    BlockRead::Complete(block) => {
                        self.awaiting.complete(block);
                        Ok(true)
                    }
                    BlockRead::Stopped => Ok(false),
                }
            }
            Line::Event(exit @ Event::Exit { .. }) => {
                self.report(exit);
                Ok(false)
            }
            Line::Event(event) => {
                self.report(event);
                Ok(true)
            }
            // A block terminator with no block open, or output outside one.
            Line::Text(_) | Line::BlockEnd { .. } => Ok(true),
        }
    }

    /// Hand an event to the receiver, if one is still listening.
    ///
    /// A receiver that has gone away is not a reason to stop: commands may
    /// still be in flight, and a caller who only sends is a valid caller.
    fn report(&mut self, event: Event) {
        // Anything already held goes first. The channel can drain between one
        // event and the next, so handing this one straight over while older
        // ones wait would deliver them out of order, and a pane's output is a
        // byte stream where that reads exactly like loss.
        if !self.pending.is_empty() {
            self.pending.push_back(event);
            return;
        }

        // Never `send().await`: this runs on the task that reads the
        // connection, so waiting here stops the reads that a reply arrives on.
        //
        // Anything but a full channel is finished with here. A receiver that
        // has gone away is not a reason to stop, because commands may still be
        // in flight and a caller who only sends is a valid caller.
        let Err(mpsc::error::TrySendError::Full(event)) = self.events.try_send(event) else {
            return;
        };

        self.pending.push_back(event);
    }

    /// Read to the end of a block that has already begun.
    async fn read_block(
        &mut self,
        number: u64,
        deadline: Option<Instant>,
        watching: &mut bool,
    ) -> Result<BlockRead, Error> {
        let mut output = Vec::new();
        let mut accumulated = 0usize;
        loop {
            let line = tokio::select! {
                biased;
                () = cancellation_requested(&mut self.core_stopped) => {
                    return Err(self.child.shutdown_error());
                }
                changed = self.stopped.changed(), if *watching => {
                    if changed.is_ok() {
                        return Ok(BlockRead::Stopped);
                    }
                    *watching = false;
                    continue;
                }
                () = deadline_elapsed(deadline) => {
                    return Err(Error::control_mode_timeout());
                }
                line = read_line_within(
                    &mut self.stdout,
                    &mut self.line,
                    self.limits.max_line_bytes,
                    Some(number),
                ) => line?,
            };
            match line {
                Some(Line::BlockEnd {
                    number: end,
                    succeeded,
                }) if end == number => {
                    return Ok(BlockRead::Complete(BlockResult {
                        number,
                        succeeded,
                        output,
                        sensitive_input: false,
                    }));
                }
                Some(Line::Text(text)) => {
                    // A block whose `%end` never arrives grows without bound,
                    // and unlike a line it can do so one valid line at a time.
                    accumulated = accumulated.saturating_add(text.as_bytes().len());
                    if accumulated > self.limits.max_block_bytes {
                        return Err(Error::control_mode_frame_too_large(
                            "block",
                            self.limits.max_block_bytes,
                        ));
                    }
                    output.push(text);
                }
                // Inside a block every other line is output, so once one is
                // open its reply never waits on a caller draining events.
                // Only once it is open: the `%begin` that opens it is read by
                // the loop above, which does report events.
                Some(Line::Event(_) | Line::BlockStart(_) | Line::BlockEnd { .. }) => {}
                None => return Err(Error::control_mode_closed()),
            }
        }
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

pub(super) async fn deadline_elapsed(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn earliest_deadline(left: Option<Instant>, right: Option<Instant>) -> Option<Instant> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}
