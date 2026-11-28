//! Bounded protocol state shared by native control-mode drivers.

use std::fmt;
use std::time::Duration;

use crate::{Command, CreationNonce, PaneId};

mod events;
mod qualification;
mod tracker;

pub use events::Event;
pub use qualification::{ControlIdentity, ControlProbe};
#[doc(hidden)]
pub use tracker::Tracker;

/// Memory and admission bounds for a persistent control connection.
///
/// Reply limits still apply while a cancelled caller's response is discarded.
/// Event overflow produces a gap and never prevents reply parsing.
///
/// # Examples
///
/// ```
/// use libtmux_core::control::ControlLimits;
///
/// let limits = ControlLimits { max_events: 8, ..ControlLimits::default() };
/// assert_eq!(limits.max_events, 8);
/// ```
#[derive(Clone, Copy, Debug)]
pub struct ControlLimits {
    /// Maximum bytes in one protocol record, including its newline.
    pub max_line_bytes: usize,
    /// Maximum wire bytes in one complete request drain, including hooks.
    pub max_reply_bytes: usize,
    /// Maximum reply blocks before the request's private fence.
    pub max_reply_blocks: usize,
    /// Maximum queued or completed requests held by the protocol tracker.
    pub max_pending_requests: usize,
    /// Maximum queued notices, before overflow coalesces into a gap.
    pub max_events: usize,
    /// Maximum wire bytes represented by queued notices.
    pub max_event_bytes: usize,
    /// Time allowed to drain a cancelled request before closing the connection.
    pub drain_timeout: Duration,
}

impl Default for ControlLimits {
    fn default() -> Self {
        Self {
            max_line_bytes: 1024 * 1024,
            max_reply_bytes: 4 * 1024 * 1024,
            max_reply_blocks: 256,
            max_pending_requests: 16,
            max_events: 256,
            max_event_bytes: 4 * 1024 * 1024,
            drain_timeout: Duration::from_secs(1),
        }
    }
}

/// A protocol or admission failure without reply payload bytes.
///
/// # Examples
///
/// ```
/// use libtmux_core::control::ControlFault;
///
/// let fault = ControlFault::LineLimit { limit: 64 };
/// assert!(fault.to_string().contains("64"));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ControlFault {
    /// The connection has stopped accepting requests.
    #[error("control connection is closed")]
    Closed,
    /// All bounded request slots are occupied.
    #[error("all {limit} control request slots are in use")]
    Busy {
        /// The request capacity.
        limit: usize,
    },
    /// A protocol record exceeded its bound.
    #[error("control record exceeded {limit} bytes")]
    LineLimit {
        /// The record byte limit.
        limit: usize,
    },
    /// A reply drain exceeded its bound.
    #[error("control reply drain exceeded {limit} bytes")]
    ReplyLimit {
        /// The reply wire-byte limit.
        limit: usize,
    },
    /// A reply drain contained too many blocks.
    #[error("control reply drain exceeded {limit} blocks")]
    BlockLimit {
        /// The block-count limit.
        limit: usize,
    },
    /// Framing did not contain one valid timestamp, number and flags tuple.
    #[error("invalid control frame header")]
    InvalidHeader,
    /// A terminator did not match the complete open frame tuple.
    #[error("control frame terminator did not match its header")]
    WrongTerminator,
    /// A command block arrived without an admitted request.
    #[error("control reply arrived without an admitted request")]
    UnownedReply,
    /// A fence arrived without the action and fence blocks it must follow.
    #[error("control request fence did not follow a complete action")]
    InvalidFence,
    /// The attached connection did not return its exact challenged identity.
    #[error("control connection identity challenge did not match its pinned endpoint")]
    InvalidChallenge,
    /// The initial attachment did not succeed.
    #[error("tmux refused the control attachment")]
    AttachRejected,
    /// The initial session notice named another session.
    #[error("control attachment selected another session")]
    WrongSession,
    /// The stream ended before all admitted requests were drained.
    #[error("control connection ended before its replies were drained")]
    Disconnected,
    /// A command could not be encoded without losing its bytes.
    #[error("command cannot be encoded as one control input line")]
    InvalidCommand,
    /// A command line exceeded the configured bound.
    #[error("control command exceeded {limit} bytes")]
    CommandLimit {
        /// The command byte limit.
        limit: usize,
    },
    /// A command or cancellation drain reached its deadline.
    #[error("control request deadline elapsed")]
    Deadline,
    /// The caller cancelled its command.
    #[error("control request was cancelled")]
    Cancelled,
}

/// The complete tuple delimiting one tmux control reply block.
///
/// # Examples
///
/// ```
/// use libtmux_core::control::FrameHeader;
///
/// let header = FrameHeader { timestamp: 1, number: 2, flags: 1 };
/// assert_eq!(header.number, 2);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameHeader {
    /// The tmux frame timestamp.
    pub timestamp: u64,
    /// The command number assigned by this connection.
    pub number: u64,
    /// The protocol flags; input commands use one and hooks use zero.
    pub flags: u64,
}

/// Bytes and acknowledgement from one input command block.
///
/// Debug output reports byte counts without printing the command's data.
///
/// # Examples
///
/// ```
/// use libtmux_core::control::ControlBlock;
///
/// # fn inspect(block: &ControlBlock) {
/// let bytes: &[u8] = block.output();
/// # let _ = bytes;
/// # }
/// ```
#[derive(Clone, Eq, PartialEq)]
pub struct ControlBlock {
    pub(super) header: FrameHeader,
    pub(super) succeeded: bool,
    pub(super) bytes: Vec<u8>,
}

impl ControlBlock {
    /// The complete framing tuple.
    #[must_use]
    pub const fn header(&self) -> FrameHeader {
        self.header
    }

    /// Whether tmux ended this block successfully.
    #[must_use]
    pub const fn succeeded(&self) -> bool {
        self.succeeded
    }

    /// The exact newline-terminated bytes carried by this block.
    #[must_use]
    pub fn output(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for ControlBlock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlBlock")
            .field("header", &self.header)
            .field("succeeded", &self.succeeded)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

/// Input-command blocks collected before a request's private drain fence.
///
/// Hook blocks and the private fence are excluded. Nested input commands
/// remain separate blocks; their status is never guessed from a block count.
///
/// # Examples
///
/// ```
/// use libtmux_core::control::ControlReply;
///
/// # fn inspect(reply: &ControlReply) {
/// for block in reply.blocks() { let _ = block.succeeded(); }
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlReply {
    pub(super) blocks: Vec<ControlBlock>,
}

impl ControlReply {
    /// The acknowledged input blocks, in daemon order.
    #[must_use]
    pub fn blocks(&self) -> &[ControlBlock] {
        &self.blocks
    }

    /// Whether every input block succeeded.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        !self.blocks.is_empty() && self.blocks.iter().all(ControlBlock::succeeded)
    }
}

/// A notification or an explicit gap in a bounded subscriber queue.
///
/// # Examples
///
/// ```
/// use libtmux_core::control::ControlNotice;
///
/// let gap = ControlNotice::Gap { dropped: 2, included_pause: true };
/// assert!(matches!(gap, ControlNotice::Gap { included_pause: true, .. }));
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ControlNotice {
    /// A modelled or unrecognised tmux notification.
    Event(Event),
    /// Notices were discarded without blocking command replies.
    Gap {
        /// The number of discarded notices, saturating at `usize::MAX`.
        dropped: usize,
        /// Whether discarded data included a pause boundary.
        included_pause: bool,
    },
}

/// The flow-control acknowledgement attached to a typed request.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequestKind {
    /// No flow-control state is inferred from opaque command output.
    Ordinary,
    /// A successful request pauses this pane.
    Pause(PaneId),
    /// A successful request continues this pane.
    Continue(PaneId),
}

/// A bounded tracker completion consumed promptly by its native driver.
#[doc(hidden)]
#[derive(Debug)]
pub enum Completion {
    /// Both the opening block and requested session notice were verified.
    Opened,
    /// A live caller's complete input reply.
    Reply {
        /// The native driver's request token.
        token: u64,
        /// Input blocks through the verified fence.
        reply: ControlReply,
    },
    /// A cancelled caller's place was drained without retaining its data.
    Drained {
        /// The native driver's request token.
        token: u64,
    },
    /// A live caller cannot receive a verified complete reply.
    Lost {
        /// The native driver's request token.
        token: u64,
        /// The terminal protocol fault.
        fault: ControlFault,
    },
}

pub(super) fn fence_diagnostic(nonce: CreationNonce) -> Vec<u8> {
    format!(
        "unknown command: libtmux-control-fence:{}\n",
        nonce.encoded()
    )
    .into_bytes()
}

/// Build the hook-free fence following one already-admitted input command.
#[doc(hidden)]
pub fn fence_command(nonce: CreationNonce) -> Command {
    Command::new("run-shell")
        .arg("-C")
        .arg(format!("libtmux-control-fence:{}", nonce.encoded()))
}

#[cfg(test)]
mod challenge_tests;
#[cfg(test)]
mod tests;
