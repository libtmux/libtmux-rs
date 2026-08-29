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
    /// `None` waits as long as the dispatch's own deadline allows. A value
    /// may shorten that wait but never extends the dispatch deadline. It
    /// makes overload distinguishable from slowness:
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

/// How many persistent control-mode clients one server may hold.
///
/// Kept separate from [`DispatchLimits`]: a watcher may live for minutes,
/// while an ordinary command should still get a short-lived client process.
#[cfg(feature = "control-mode")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlClientLimits {
    pub(crate) max_clients: usize,
    pub(crate) acquire_timeout: Option<Duration>,
}

#[cfg(feature = "control-mode")]
impl ControlClientLimits {
    /// How many persistent clients one server admits by default.
    pub const DEFAULT_CLIENTS: usize = 16;

    /// Set how many persistent clients may remain attached.
    ///
    /// Zero is treated as one: a server that admits nothing cannot observe.
    #[must_use]
    pub const fn max_clients(self, clients: usize) -> Self {
        Self {
            max_clients: if clients == 0 { 1 } else { clients },
            ..self
        }
    }

    /// Set how long a client waits for a place before reporting overload.
    ///
    /// `None` uses the server's ordinary request deadline. A shorter value
    /// makes saturation fail promptly without extending that deadline.
    #[must_use]
    pub const fn acquire_timeout(self, timeout: Option<Duration>) -> Self {
        Self {
            acquire_timeout: timeout,
            ..self
        }
    }

    /// How many persistent clients may remain attached.
    #[must_use]
    pub const fn clients(self) -> usize {
        self.max_clients
    }
}

#[cfg(feature = "control-mode")]
impl Default for ControlClientLimits {
    fn default() -> Self {
        Self {
            max_clients: Self::DEFAULT_CLIENTS,
            acquire_timeout: None,
        }
    }
}

/// What one control-mode connection may accumulate before it gives up.
///
/// Control mode is a framed text protocol read from a process that keeps
/// running, so the framing is the only thing bounding memory: a line that
/// never ends, or a `%begin` block whose `%end` never arrives, otherwise grows
/// until the machine notices. A subprocess dispatch at least ends when the
/// process does.
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "control-mode")] {
/// use libtmux::ControlLimits;
///
/// let limits = ControlLimits::default().max_line_bytes(4096);
/// assert_eq!(limits.line_bytes(), 4096);
/// # }
/// ```
#[cfg(feature = "control-mode")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlLimits {
    pub(crate) max_line_bytes: usize,
    pub(crate) max_block_bytes: usize,
}

#[cfg(feature = "control-mode")]
impl ControlLimits {
    /// 8 MiB for one line. A pane printing a single enormous line is the
    /// realistic way to reach this, and it is well past anything a terminal
    /// displays.
    pub const DEFAULT_LINE_BYTES: usize = 8 * 1024 * 1024;

    /// 64 MiB for one `%begin`/`%end` block, which is a whole command's
    /// answer rather than one line.
    pub const DEFAULT_BLOCK_BYTES: usize = 64 * 1024 * 1024;

    /// Set the budget for a single protocol line.
    #[must_use]
    pub const fn max_line_bytes(self, bytes: usize) -> Self {
        Self {
            max_line_bytes: bytes,
            ..self
        }
    }

    /// Set the budget for one command's response block.
    #[must_use]
    pub const fn max_block_bytes(self, bytes: usize) -> Self {
        Self {
            max_block_bytes: bytes,
            ..self
        }
    }

    /// The budget for a single protocol line.
    #[must_use]
    pub const fn line_bytes(self) -> usize {
        self.max_line_bytes
    }

    /// The budget for one command's response block.
    #[must_use]
    pub const fn block_bytes(self) -> usize {
        self.max_block_bytes
    }
}

#[cfg(feature = "control-mode")]
impl Default for ControlLimits {
    fn default() -> Self {
        Self {
            max_line_bytes: Self::DEFAULT_LINE_BYTES,
            max_block_bytes: Self::DEFAULT_BLOCK_BYTES,
        }
    }
}
