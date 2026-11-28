use std::collections::VecDeque;
use std::mem;

use super::events::Line;
use super::{
    Completion, ControlBlock, ControlFault, ControlIdentity, ControlLimits, ControlNotice,
    ControlProbe, ControlReply, Event, FrameHeader, RequestKind, fence_diagnostic,
};
use crate::{CreationNonce, PaneId, SessionId, parse_number};

#[derive(Debug)]
struct Request {
    token: u64,
    kind: RequestKind,
    fence: Vec<u8>,
    cancelled: bool,
    blocks: Vec<ControlBlock>,
    wire_bytes: usize,
    reply_limit: usize,
    block_count: usize,
    input_blocks: usize,
    last_empty_success: bool,
    succeeded: bool,
    challenge: Option<Challenge>,
}

#[derive(Debug)]
struct Challenge {
    probe: ControlProbe,
    expected: ControlIdentity,
    seen: bool,
}

#[derive(Debug)]
struct ReadingBlock {
    header: FrameHeader,
    bytes: Vec<u8>,
    wire_bytes: usize,
    fence_candidate: bool,
    empty: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Opening,
    Attached,
    Qualified,
    Closed,
}

/// Bounded framing, reply drains and notices without native I/O.
///
/// The driver registers a command before its first write and follows it with
/// [`super::fence_command`]. Completions must be consumed before their bounded
/// admission slots can be reused. Notice consumers never own reply progress.
#[doc(hidden)]
pub struct Tracker {
    limits: ControlLimits,
    session: SessionId,
    pending: VecDeque<Request>,
    completed: VecDeque<Completion>,
    line: Vec<u8>,
    block: Option<ReadingBlock>,
    notices: VecDeque<(Event, usize)>,
    notice_bytes: usize,
    dropped: usize,
    dropped_pause: bool,
    flow: VecDeque<(PaneId, bool)>,
    opening_block: bool,
    opening_session: bool,
    phase: Phase,
}

impl Tracker {
    /// Wait for the opening block and the requested session's notification.
    #[must_use]
    pub fn new(session: SessionId, limits: ControlLimits) -> Self {
        Self {
            limits,
            session,
            pending: VecDeque::new(),
            completed: VecDeque::new(),
            line: Vec::new(),
            block: None,
            notices: VecDeque::new(),
            notice_bytes: 0,
            dropped: 0,
            dropped_pause: false,
            flow: VecDeque::new(),
            opening_block: false,
            opening_session: false,
            phase: Phase::Opening,
        }
    }

    /// Reserve one reply drain before writing any of the request's bytes.
    ///
    /// # Errors
    ///
    /// Refuses closed, unopened, duplicate or capacity-exceeding requests.
    pub fn register(
        &mut self,
        token: u64,
        nonce: CreationNonce,
        kind: RequestKind,
    ) -> Result<(), ControlFault> {
        self.register_with_reply_limit(token, nonce, kind, self.limits.max_reply_bytes)
    }

    /// Reserve a drain with a caller bound no larger than the connection's.
    ///
    /// # Errors
    ///
    /// Returns the same admission errors as [`Self::register`].
    pub fn register_with_reply_limit(
        &mut self,
        token: u64,
        nonce: CreationNonce,
        kind: RequestKind,
        reply_limit: usize,
    ) -> Result<(), ControlFault> {
        if self.is_closed() || !self.is_established() {
            return Err(ControlFault::Closed);
        }
        if self.pending.len().saturating_add(self.completed.len())
            >= self.limits.max_pending_requests
        {
            return Err(ControlFault::Busy {
                limit: self.limits.max_pending_requests,
            });
        }
        let fence = fence_diagnostic(nonce);
        if self
            .pending
            .iter()
            .any(|request| request.token == token || request.fence == fence)
        {
            return Err(ControlFault::InvalidCommand);
        }
        self.pending.push_back(Request {
            token,
            kind,
            fence,
            cancelled: false,
            blocks: Vec::new(),
            wire_bytes: 0,
            reply_limit: reply_limit.min(self.limits.max_reply_bytes),
            block_count: 0,
            input_blocks: 0,
            last_empty_success: false,
            succeeded: true,
            challenge: None,
        });
        Ok(())
    }

    /// Reserve the bootstrap challenge before exposing a native connection.
    ///
    /// # Errors
    ///
    /// Refuses a nonempty or already qualified request stream.
    pub fn register_challenge(
        &mut self,
        token: u64,
        nonce: CreationNonce,
        probe: ControlProbe,
        expected: ControlIdentity,
    ) -> Result<(), ControlFault> {
        if self.is_qualified() || !self.pending.is_empty() || !self.completed.is_empty() {
            return Err(ControlFault::InvalidChallenge);
        }
        self.register(token, nonce, RequestKind::Ordinary)?;
        let request = self
            .pending
            .back_mut()
            .ok_or(ControlFault::InvalidChallenge)?;
        request.challenge = Some(Challenge {
            probe,
            expected,
            seen: false,
        });
        Ok(())
    }

    /// Discard a caller's data while retaining its bounded FIFO drain.
    pub fn cancel(&mut self, token: u64) {
        for (index, request) in self.pending.iter_mut().enumerate() {
            if request.token != token {
                continue;
            }
            request.cancelled = true;
            request.blocks.clear();
            if index == 0 {
                if let Some(block) = &mut self.block {
                    if !request.fence.starts_with(&block.bytes) {
                        block.bytes.clear();
                        block.fence_candidate = false;
                    }
                }
            }
            break;
        }
    }

    /// Consume arbitrary read chunks without exceeding a record budget.
    ///
    /// # Errors
    ///
    /// Closes and fails outstanding callers on malformed or oversized data.
    /// Already queued completions and notices remain available for delivery.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), ControlFault> {
        if self.is_closed() {
            return Err(ControlFault::Closed);
        }
        for &byte in bytes {
            if self.line.len() >= self.limits.max_line_bytes {
                return self.fail(ControlFault::LineLimit {
                    limit: self.limits.max_line_bytes,
                });
            }
            if let Some(block) = &self.block {
                let partial = self.line.len().saturating_add(1);
                if block.wire_bytes.saturating_add(partial) > self.limits.max_reply_bytes
                    || self.pending.front().is_some_and(|request| {
                        request.wire_bytes.saturating_add(partial) > request.reply_limit
                    })
                {
                    let limit = self
                        .pending
                        .front()
                        .map_or(self.limits.max_reply_bytes, |request| request.reply_limit);
                    return self.fail(ControlFault::ReplyLimit { limit });
                }
            }
            if byte == b'\n' {
                let line = mem::take(&mut self.line);
                if let Err(fault) = self.record(line) {
                    return self.fail(fault);
                }
                if self.is_closed() {
                    break;
                }
            } else {
                self.line.push(byte);
            }
        }
        Ok(())
    }

    /// Confirm EOF only after complete framing and all admitted drains.
    ///
    /// # Errors
    ///
    /// Fails live callers when a line, block, opening or reply is incomplete.
    pub fn eof(&mut self) -> Result<(), ControlFault> {
        let incomplete = !self.is_established()
            || !self.line.is_empty()
            || self.block.is_some()
            || !self.pending.is_empty();
        self.close(ControlFault::Disconnected);
        if incomplete {
            Err(ControlFault::Disconnected)
        } else {
            Ok(())
        }
    }

    /// Fail only live outstanding callers, retaining already buffered notices.
    pub fn close(&mut self, fault: ControlFault) {
        if self.is_closed() {
            return;
        }
        self.phase = Phase::Closed;
        self.line.clear();
        self.block = None;
        for request in self.pending.drain(..) {
            self.completed.push_back(if request.cancelled {
                Completion::Drained {
                    token: request.token,
                }
            } else {
                Completion::Lost {
                    token: request.token,
                    fault,
                }
            });
        }
    }

    /// Remove one completion and release its bounded admission slot.
    pub fn next_completion(&mut self) -> Option<Completion> {
        self.completed.pop_front()
    }

    /// Remove the oldest notice or the gap following previously queued events.
    pub fn next_notice(&mut self) -> Option<ControlNotice> {
        if let Some((event, bytes)) = self.notices.pop_front() {
            self.notice_bytes = self.notice_bytes.saturating_sub(bytes);
            return Some(ControlNotice::Event(event));
        }
        (self.dropped != 0).then(|| ControlNotice::Gap {
            dropped: mem::take(&mut self.dropped),
            included_pause: mem::take(&mut self.dropped_pause),
        })
    }

    /// Stop retaining notices after the sole subscriber has gone away.
    pub fn discard_notices(&mut self) {
        self.notices.clear();
        self.notice_bytes = 0;
        self.dropped = 0;
        self.dropped_pause = false;
    }

    /// Whether both independent attachment acknowledgements were observed.
    #[must_use]
    pub const fn is_established(&self) -> bool {
        self.opening_block && self.opening_session
    }

    /// Whether framing or the remote endpoint has closed the tracker.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        matches!(self.phase, Phase::Closed)
    }

    /// Whether the attached identity and the same request's fence were verified.
    #[must_use]
    pub const fn is_qualified(&self) -> bool {
        matches!(self.phase, Phase::Qualified)
    }

    fn fail<T>(&mut self, fault: ControlFault) -> Result<T, ControlFault> {
        self.close(fault);
        Err(fault)
    }

    fn account_wire(&mut self, bytes: usize, new_block: bool) -> Result<(), ControlFault> {
        if let Some(request) = self.pending.front_mut() {
            request.wire_bytes = request.wire_bytes.saturating_add(bytes);
            if request.wire_bytes > request.reply_limit {
                return Err(ControlFault::ReplyLimit {
                    limit: request.reply_limit,
                });
            }
            if new_block {
                request.block_count = request.block_count.saturating_add(1);
                if request.block_count > self.limits.max_reply_blocks {
                    return Err(ControlFault::BlockLimit {
                        limit: self.limits.max_reply_blocks,
                    });
                }
            }
        }
        Ok(())
    }

    fn record(&mut self, line: Vec<u8>) -> Result<(), ControlFault> {
        let wire_bytes = line.len().saturating_add(1);
        if self.block.is_some() {
            return self.block_record(&line, wire_bytes);
        }
        if let Some(header) = line.strip_prefix(b"%begin ") {
            let header = parse_header(header)?;
            if header.flags == 1 && self.pending.is_empty() {
                return Err(ControlFault::UnownedReply);
            }
            self.account_wire(wire_bytes, true)?;
            self.block = Some(ReadingBlock {
                header,
                bytes: Vec::new(),
                wire_bytes,
                fence_candidate: true,
                empty: true,
            });
            return Ok(());
        }
        if line == b"%begin" {
            return Err(ControlFault::InvalidHeader);
        }
        if terminator(&line).is_some() || line == b"%end" || line == b"%error" {
            return Err(ControlFault::WrongTerminator);
        }
        if line.starts_with(b"unknown command: libtmux-control-challenge:") {
            self.account_wire(wire_bytes, false)?;
            let mut report = line;
            report.push(b'\n');
            if self.consume_challenge(&report)? {
                return Ok(());
            }
            return Err(ControlFault::InvalidChallenge);
        }
        if line.starts_with(b"unknown command: libtmux-control-fence:") {
            self.account_wire(wire_bytes, false)?;
            let request = self.pending.front().ok_or(ControlFault::InvalidFence)?;
            if request.fence.strip_suffix(b"\n") != Some(line.as_slice())
                || request.input_blocks < 2
                || !request.last_empty_success
            {
                return Err(ControlFault::InvalidFence);
            }
            return self.complete(true);
        }
        match Line::parse(&line) {
            Line::Event(Event::SessionChanged { session }) if !self.is_established() => {
                if session != self.session {
                    return Err(ControlFault::WrongSession);
                }
                self.opening_session = true;
                self.publish(Event::SessionChanged { session }, wire_bytes);
                self.open_if_ready();
            }
            Line::Event(Event::Paused { pane }) => self.set_flow(pane, true, wire_bytes),
            Line::Event(Event::Continued { pane }) => self.set_flow(pane, false, wire_bytes),
            Line::Event(event @ Event::Exit { .. }) => {
                self.publish(event, wire_bytes);
                self.close(ControlFault::Disconnected);
            }
            Line::Event(event) => self.publish(event, wire_bytes),
            Line::Text(text) => self.publish(
                Event::Other {
                    name: "unframed".into(),
                    rest: text,
                },
                wire_bytes,
            ),
        }
        Ok(())
    }

    fn block_record(&mut self, line: &[u8], wire_bytes: usize) -> Result<(), ControlFault> {
        self.account_wire(wire_bytes, false)?;
        let block = self.block.as_mut().ok_or(ControlFault::InvalidHeader)?;
        block.wire_bytes = block.wire_bytes.saturating_add(wire_bytes);
        if block.wire_bytes > self.limits.max_reply_bytes {
            return Err(ControlFault::ReplyLimit {
                limit: self.limits.max_reply_bytes,
            });
        }
        if let Some((succeeded, header)) = terminator(line) {
            if parse_header(header)? != block.header {
                return Err(ControlFault::WrongTerminator);
            }
            let block = self.block.take().ok_or(ControlFault::InvalidHeader)?;
            return self.finish_block(block, succeeded);
        }
        block.empty = false;
        if block.header.flags != 1 {
            return Ok(());
        }
        let request = self.pending.front().ok_or(ControlFault::UnownedReply)?;
        if !request.cancelled {
            block.bytes.extend_from_slice(line);
            block.bytes.push(b'\n');
        } else if block.fence_candidate {
            if block.bytes.len().saturating_add(wire_bytes) > request.fence.len() {
                block.bytes.clear();
                block.fence_candidate = false;
            } else {
                block.bytes.extend_from_slice(line);
                block.bytes.push(b'\n');
                if !request.fence.starts_with(&block.bytes) {
                    block.bytes.clear();
                    block.fence_candidate = false;
                }
            }
        }
        Ok(())
    }

    fn finish_block(&mut self, block: ReadingBlock, succeeded: bool) -> Result<(), ControlFault> {
        if block.header.flags == 0 {
            if !self.opening_block {
                if !succeeded {
                    return Err(ControlFault::AttachRejected);
                }
                self.opening_block = true;
                self.open_if_ready();
            }
            return Ok(());
        }
        if block
            .bytes
            .starts_with(b"unknown command: libtmux-control-challenge:")
        {
            self.consume_challenge(&block.bytes)?;
        }
        let request = self.pending.front_mut().ok_or(ControlFault::UnownedReply)?;
        if !succeeded && block.bytes == request.fence {
            if request.input_blocks == 0 {
                return Err(ControlFault::InvalidFence);
            }
            return self.complete(false);
        }
        request.input_blocks = request.input_blocks.saturating_add(1);
        request.last_empty_success = succeeded && block.empty;
        request.succeeded &= succeeded;
        if !request.cancelled {
            request.blocks.push(ControlBlock {
                header: block.header,
                succeeded,
                bytes: block.bytes,
            });
        }
        Ok(())
    }

    fn complete(&mut self, remove_empty_fence: bool) -> Result<(), ControlFault> {
        if self
            .pending
            .front()
            .and_then(|request| request.challenge.as_ref())
            .is_some_and(|challenge| !challenge.seen)
        {
            return Err(ControlFault::InvalidChallenge);
        }
        let mut request = self.pending.pop_front().ok_or(ControlFault::InvalidFence)?;
        if request.challenge.is_some() {
            self.phase = Phase::Qualified;
        }
        if remove_empty_fence && !request.cancelled {
            request.blocks.pop();
        }
        if request.succeeded {
            match request.kind {
                RequestKind::Ordinary => {}
                RequestKind::Pause(pane) => self.set_flow(pane, true, 32),
                RequestKind::Continue(pane) => self.set_flow(pane, false, 32),
            }
        }
        self.completed.push_back(if request.cancelled {
            Completion::Drained {
                token: request.token,
            }
        } else {
            Completion::Reply {
                token: request.token,
                reply: ControlReply {
                    blocks: request.blocks,
                },
            }
        });
        Ok(())
    }

    fn open_if_ready(&mut self) {
        if self.phase == Phase::Opening && self.opening_block && self.opening_session {
            self.phase = Phase::Attached;
            self.completed.push_back(Completion::Opened);
        }
    }

    fn consume_challenge(&mut self, report: &[u8]) -> Result<bool, ControlFault> {
        let Some(challenge) = self
            .pending
            .front_mut()
            .and_then(|request| request.challenge.as_mut())
        else {
            return Ok(false);
        };
        if challenge.seen || !challenge.probe.matches_report_prefix(report) {
            return Err(ControlFault::InvalidChallenge);
        }
        let actual = challenge
            .probe
            .decode_control_report(report)
            .map_err(|_| ControlFault::InvalidChallenge)?;
        if actual != challenge.expected {
            return Err(ControlFault::InvalidChallenge);
        }
        challenge.seen = true;
        Ok(true)
    }

    fn set_flow(&mut self, pane: PaneId, paused: bool, bytes: usize) {
        if let Some((_, prior)) = self.flow.iter_mut().find(|(id, _)| *id == pane) {
            if *prior == paused {
                return;
            }
            *prior = paused;
        } else {
            if self.flow.len() >= self.limits.max_events.max(1) {
                if let Some((_, was_paused)) = self.flow.pop_front() {
                    self.drop_notice(was_paused);
                }
            }
            self.flow.push_back((pane.clone(), paused));
        }
        self.publish(
            if paused {
                Event::Paused { pane }
            } else {
                Event::Continued { pane }
            },
            bytes,
        );
    }

    fn publish(&mut self, event: Event, bytes: usize) {
        if self.dropped != 0
            || self.notices.len() >= self.limits.max_events
            || bytes
                > self
                    .limits
                    .max_event_bytes
                    .saturating_sub(self.notice_bytes)
        {
            self.drop_notice(matches!(event, Event::Paused { .. }));
        } else {
            self.notice_bytes += bytes;
            self.notices.push_back((event, bytes));
        }
    }

    fn drop_notice(&mut self, included_pause: bool) {
        self.dropped = self.dropped.saturating_add(1);
        self.dropped_pause |= included_pause;
    }
}

impl std::fmt::Debug for Tracker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Tracker")
            .field("pending", &self.pending.len())
            .field("completed", &self.completed.len())
            .field("partial_bytes", &self.line.len())
            .field("notices", &self.notices.len())
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}

fn terminator(line: &[u8]) -> Option<(bool, &[u8])> {
    line.strip_prefix(b"%end ")
        .map(|header| (true, header))
        .or_else(|| line.strip_prefix(b"%error ").map(|header| (false, header)))
}

fn parse_header(bytes: &[u8]) -> Result<FrameHeader, ControlFault> {
    let text = std::str::from_utf8(bytes).map_err(|_| ControlFault::InvalidHeader)?;
    let fields: Vec<_> = text.split(' ').collect();
    let [timestamp, number, flags] = fields.as_slice() else {
        return Err(ControlFault::InvalidHeader);
    };
    let timestamp = parse_number(timestamp).ok_or(ControlFault::InvalidHeader)?;
    let number = parse_number(number).ok_or(ControlFault::InvalidHeader)?;
    let flags = parse_number(flags)
        .filter(|flags| *flags <= 1)
        .ok_or(ControlFault::InvalidHeader)?;
    Ok(FrameHeader {
        timestamp,
        number,
        flags,
    })
}
