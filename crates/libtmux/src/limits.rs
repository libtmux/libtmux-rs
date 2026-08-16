//! Budgets that bound what one server may consume.
//!
//! tmux is not an adversary, but it is an unbounded producer: a pane with a
//! large history, a buffer someone pasted a file into, or a `run-shell` that
//! keeps printing all answer with as many bytes as they have. A library that
//! reads those into memory without a ceiling makes the operating system decide
//! when to stop, which it does by killing the caller's process.
//!
//! These are deliberately generous rather than tight. The point is that a
//! ceiling exists and is named in an error, not that it is small.

use std::time::Duration;

/// How many bytes one dispatch may read from each stream.
///
/// Exceeding a budget fails the dispatch with
/// [`Error::OutputLimitExceeded`](crate::Error::OutputLimitExceeded) rather
/// than truncating, because a truncated tmux listing is a shorter listing: it
/// decodes cleanly and says something false. A caller who wants the first N
/// bytes of a pane asks tmux for them.
///
/// # Examples
///
/// ```
/// use libtmux::OutputLimits;
///
/// // The default is roomy enough for a pane with a very long history.
/// assert_eq!(OutputLimits::default().stdout_bytes(), 32 * 1024 * 1024);
///
/// // Tighten it where the caller knows the answer is small.
/// let limits = OutputLimits::default().max_stdout_bytes(64 * 1024);
/// assert_eq!(limits.stdout_bytes(), 64 * 1024);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputLimits {
    pub(crate) max_stdout_bytes: usize,
    pub(crate) max_stderr_bytes: usize,
}

impl OutputLimits {
    /// 32 MiB of stdout, which is more history than a pane usually holds.
    pub const DEFAULT_STDOUT_BYTES: usize = 32 * 1024 * 1024;

    /// 1 MiB of stderr. tmux's errors are one line; anything approaching this
    /// is a runaway rather than a message.
    pub const DEFAULT_STDERR_BYTES: usize = 1024 * 1024;

    /// Set the stdout budget for one dispatch.
    #[must_use]
    pub const fn max_stdout_bytes(self, bytes: usize) -> Self {
        Self {
            max_stdout_bytes: bytes,
            ..self
        }
    }

    /// Set the stderr budget for one dispatch.
    #[must_use]
    pub const fn max_stderr_bytes(self, bytes: usize) -> Self {
        Self {
            max_stderr_bytes: bytes,
            ..self
        }
    }

    /// The stdout budget for one dispatch.
    #[must_use]
    pub const fn stdout_bytes(self) -> usize {
        self.max_stdout_bytes
    }

    /// The stderr budget for one dispatch.
    #[must_use]
    pub const fn stderr_bytes(self) -> usize {
        self.max_stderr_bytes
    }
}

impl Default for OutputLimits {
    fn default() -> Self {
        Self {
            max_stdout_bytes: Self::DEFAULT_STDOUT_BYTES,
            max_stderr_bytes: Self::DEFAULT_STDERR_BYTES,
        }
    }
}

/// How much work one server may have in flight at once.
///
/// Every dispatch is a tmux client process with its own pipes and reader
/// tasks. Without a ceiling, a caller that fans out -- an agent driving the
/// MCP server, a reconciler sweeping every pane -- turns its own concurrency
/// into process, descriptor, and memory pressure on the machine, and tmux
/// itself serializes on the far side regardless.
///
/// # Examples
///
/// ```
/// use libtmux::DispatchLimits;
///
/// let limits = DispatchLimits::default().max_in_flight(4);
/// assert_eq!(limits.in_flight(), 4);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DispatchLimits {
    pub(crate) max_in_flight: usize,
    pub(crate) acquire_timeout: Option<Duration>,
}

impl DispatchLimits {
    /// How many dispatches may run at once by default.
    ///
    /// tmux serializes commands on its own thread, so more clients than this
    /// buys queueing rather than throughput.
    pub const DEFAULT_IN_FLIGHT: usize = 16;

    /// Set how many dispatches may run at once.
    ///
    /// Zero is treated as one: a server that admits nothing cannot answer.
    #[must_use]
    pub const fn max_in_flight(self, permits: usize) -> Self {
        Self {
            max_in_flight: if permits == 0 { 1 } else { permits },
            ..self
        }
    }

    /// How long a dispatch waits for a permit before giving up.
    ///
    /// `None` waits as long as the dispatch's own timeout allows. A value
    /// here makes overload distinguishable from slowness:
    /// [`Error::Overloaded`](crate::Error::Overloaded) says the server never
    /// started the work, so retrying it is safe.
    #[must_use]
    pub const fn acquire_timeout(self, timeout: Option<Duration>) -> Self {
        Self {
            acquire_timeout: timeout,
            ..self
        }
    }

    /// How many dispatches may run at once.
    #[must_use]
    pub const fn in_flight(self) -> usize {
        self.max_in_flight
    }
}

impl Default for DispatchLimits {
    fn default() -> Self {
        Self {
            max_in_flight: Self::DEFAULT_IN_FLIGHT,
            acquire_timeout: None,
        }
    }
}
