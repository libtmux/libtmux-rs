//! Errors returned by libtmux.

use std::fmt;
use std::io;
use std::time::Duration;

use crate::CommandSummary;
use crate::version::{ReleaseVersion, TmuxVersion};

mod classification;
mod refusal;

/// The category of an invalid [`crate::ServerBuilder`] configuration.
///
/// Rejected path and environment bytes are never retained by this value.
///
/// # Examples
///
/// ```
/// use libtmux::{Error, ServerConfigurationErrorKind};
///
/// // A socket name and a socket path are two ways to say the same thing, and
/// // tmux has no rule for which wins, so the builder refuses rather than picks.
/// let failure = libtmux::Server::builder()
///     .socket_name("named")
///     .socket_path("/tmp/libtmux-rs-dev/explicit")
///     .build()
///     .expect_err("two socket selectors");
///
/// assert!(matches!(
///     failure,
///     Error::InvalidServerConfiguration {
///         kind: ServerConfigurationErrorKind::ConflictingSocketSelectors,
///         ..
///     },
/// ));
///
/// // The rejected bytes are not carried in the error, so logging it cannot
/// // disclose a path.
/// assert!(!failure.to_string().contains("/tmp/"));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ServerConfigurationErrorKind {
    /// A socket name and an explicit socket path were both configured.
    ConflictingSocketSelectors,
    /// A socket name was not one non-empty path component.
    InvalidSocketName,
    /// An explicit socket path was empty or contained a NUL byte.
    InvalidSocketPath,
    /// A config path was empty or contained a NUL byte.
    InvalidConfigPath,
    /// The requested color mode was neither 88 nor 256 colors.
    InvalidColorMode,
    /// The process working directory could not be captured.
    WorkingDirectoryUnavailable,
    /// A stable socket root could not be captured.
    SocketRootUnavailable,
    /// There is no `TMUX` variable, so this process is not inside tmux.
    ///
    /// Distinct from [`Self::MalformedTmuxVariable`], which means the
    /// variable is there and does not say what tmux says: the first is an
    /// ordinary state a caller may branch on, the second is a broken
    /// environment worth reporting.
    NotInsideTmux,

    /// The `TMUX` variable is present but is not tmux's triple.
    ///
    /// tmux writes `socket,pid,session`. An empty value, or one with no
    /// socket before the first comma, means something rewrote it.
    MalformedTmuxVariable,
}

/// Why a control-mode connection failed.
///
/// The distinction matters to a caller: a connection that never opened is a
/// setup problem, whereas one that closed mid-command may simply mean the
/// session it was attached to has ended.
///
/// # Examples
///
// Gate the doc attribute: a `#[cfg]` inside a doctest reads the doctest's own
// crate, which has no features, so the example would pass vacuously.
#[cfg_attr(
    feature = "control-mode",
    doc = r#"```
use libtmux::{ControlModeErrorKind, Error};

// `Closed` means the far side ended, often just the session going away. The
// other variants distinguish setup failures, deadline expiry, and refusals.
// The enum is `#[non_exhaustive]`, so a caller matches it rather than building
// one.
fn session_ended(failure: &Error) -> bool {
    matches!(
        failure,
        Error::ControlMode { kind: ControlModeErrorKind::Closed, .. },
    )
}

let unrelated = libtmux::Server::builder()
    .socket_name("named")
    .socket_path("/tmp/libtmux-rs-dev/explicit")
    .build()
    .expect_err("two socket selectors");
assert!(!session_ended(&unrelated));
```"#
)]
#[cfg(feature = "control-mode")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ControlModeErrorKind {
    /// The live control connection could not read from or write to its pipes.
    Transport,
    /// tmux started without giving the crate the pipes it asked for.
    ///
    /// Nothing a caller does causes this; it means the process could not be
    /// set up as requested.
    MissingPipes,
    /// The connection closed before the command was answered.
    ///
    /// This connection cannot reopen; attach another one to continue.
    Closed,
    /// A command's deadline elapsed before it was committed for writing.
    ///
    /// Nothing was written, this timeout does not close the connection, and
    /// retrying the same command is safe once the delay clears.
    DispatchTimedOut,
    /// The attach opening or a committed command exceeded its deadline.
    ///
    /// A command's deadline starts before queue admission and runs until tmux
    /// closes its response block. Once the connection commits the command for
    /// writing, tmux may execute it. The connection ends rather than reuse a
    /// possibly partial line, so this does not prove a mutation is safe to retry.
    TimedOut,
    /// The caller stopped reading events, so the connection could not reach
    /// this command's reply.
    ///
    /// A reply arrives on the connection the events arrive on. The connection
    /// holds what a caller has not taken and keeps reading while a reply is
    /// outstanding, but not without limit, and past that limit it stops rather
    /// than growing. The events remain held and the connection carries on once
    /// they are taken.
    ///
    /// A new request can be refused before it is written, but an already-live
    /// request gets the same error after crossing the write boundary. This
    /// kind therefore does not prove that a mutation is safe to replay. Drain
    /// the events before sending more work, or watch from a task of its own so
    /// the two never contend.
    Unread,
    /// The command contains an argument no control-mode line can carry.
    ///
    /// Control mode is a text protocol, so an argument that is not UTF-8
    /// cannot be sent over it even though the same command would run fine as
    /// a subprocess.
    UnrepresentableCommand,
    /// A subscription name was empty, or contained a colon.
    ///
    /// tmux splits the subscription argument on its first colon, so a name
    /// carrying one names something other than what was asked for and takes
    /// the rest of the request with it. Refused here rather than sent,
    /// because tmux accepts the result and reports no error.
    InvalidSubscriptionName,
}

/// What tmux says when it holds no session to resolve a target against.
pub(crate) const NO_CURRENT_TARGET: &str = "no current target";

pub(crate) const SENSITIVE_OUTPUT_WITHHELD: &str =
    "tmux output withheld because the request contained sensitive input";

/// What tmux says when a move has nowhere to go.
///
/// Reported as a command failure, but it is an ordinary state rather than a
/// fault: a session holding one window has no next window and never will.
/// Navigation reports it as absence so a caller does not have to tell the two
/// apart by reading text.
pub(crate) const NO_SUCH_NEIGHBOUR: [&str; 4] = [
    "no next window",
    "no previous window",
    "no last window",
    "no last pane",
];

/// Which way tmux would not accept an option.
///
/// # Examples
///
/// Each kind points at a different fix, so a caller can act instead of
/// re-reading tmux's wording:
///
/// ```
/// use libtmux::{Error, OptionErrorKind};
///
/// fn advise(error: &Error) -> &'static str {
///     match error {
///         Error::OptionRejected { kind, .. } => match kind {
///             OptionErrorKind::Unknown => "check the spelling",
///             OptionErrorKind::Ambiguous => "write more of the name",
///             OptionErrorKind::BadValue => "the option will not hold that",
///             _ => "tmux refused it",
///         },
///         _ => "not an option problem",
///     }
/// }
///
/// let refused = Error::OptionRejected {
///     kind: OptionErrorKind::Ambiguous,
///     detail: "status-l".to_owned(),
/// };
/// assert_eq!(advise(&refused), "write more of the name");
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OptionErrorKind {
    /// No option goes by that name.
    Unknown,
    /// The name is a prefix of more than one option, so tmux will not guess.
    Ambiguous,
    /// The option exists and will not hold that value.
    BadValue,
}

/// Which way a tmux server was not there.
///
/// The four are one decision -- there is no server -- and four different
/// stories about how, which is the difference between a socket nobody has
/// started and a server that died under the command being run.
///
/// # Examples
///
/// ```
/// use libtmux::{Error, ServerGoneKind};
///
/// fn advise(error: &Error) -> &'static str {
///     match error {
///         Error::ServerGone { kind, .. } => match kind {
///             ServerGoneKind::NotRunning => "start one",
///             ServerGoneKind::Unreachable => "check the socket path",
///             ServerGoneKind::Lost | ServerGoneKind::Stopped => "it went away mid-command",
///             _ => "there is no server",
///         },
///         _ => "not a server problem",
///     }
/// }
///
/// let absent = Error::ServerGone {
///     command: "list-sessions",
///     kind: ServerGoneKind::NotRunning,
/// };
/// assert_eq!(advise(&absent), "start one");
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ServerGoneKind {
    /// Nothing was listening on the socket.
    NotRunning,
    /// The socket was there and the connection to it failed.
    Unreachable,
    /// The connection was lost with the command in flight, which is a server
    /// that crashed or was killed.
    Lost,
    /// The server shut down with the command in flight.
    Stopped,
}

impl fmt::Display for ServerGoneKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotRunning => "nothing is listening",
            Self::Unreachable => "the endpoint is unreachable",
            Self::Lost => "the connection was lost",
            Self::Stopped => "the server stopped",
        })
    }
}

/// What tmux says when it has no client to act on.
pub(crate) const NO_CURRENT_CLIENT: &str = "no current client";

/// A coarse failure category for reporting and routing.
///
/// [`Error`] carries the recovery detail. One category can contain failures
/// with different retry scopes: overload and a bad argument are both refused,
/// while executor shutdown and a failed child pipe are both transport errors.
/// Use [`Error::is_transient`] before repeating a call unchanged.
///
/// New kinds may be added, so match with a `_` arm.
///
/// # Examples
///
/// ```
/// use libtmux::ErrorKind;
///
/// fn target_is_stale(kind: ErrorKind) -> bool {
///     matches!(kind, ErrorKind::ObjectGone)
/// }
///
/// assert!(target_is_stale(ErrorKind::ObjectGone));
/// assert!(!target_is_stale(ErrorKind::InvalidInput));
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// tmux accepted an effectful step before a later part of the operation
    /// failed.
    PartialEffect,
    /// The object is not on the server. Look it up again, or create it.
    ObjectGone,
    /// The operation was refused before or by tmux.
    Refused,
    /// No tmux server answered. Start one, or name the socket that has it.
    ServerGone,
    /// The operation did not finish in time.
    Timeout,
    /// tmux could not be run at all: not installed, or not where the server
    /// was told to look. Nothing about the request will change this.
    Unreachable,
    /// The tmux that answered is older than this crate supports.
    UnsupportedVersion,
    /// The caller supplied inputs that cannot form a valid operation.
    InvalidInput,
    /// The process, connection, or executor carrying the command failed.
    Transport,
    /// tmux answered in a shape the crate could not read. Worth reporting.
    Decode,
}

/// An invalid scope-specific tmux object ID.
///
/// The error records the expected sigil but never retains the rejected input.
///
/// # Examples
///
/// ```
/// use libtmux::SessionId;
///
/// // The sigil is the whole difference between the id types, so a mistake
/// // names the one that was expected.
/// let error = "@1".parse::<SessionId>().expect_err("@ denotes a window");
/// assert_eq!(error.expected_sigil(), '$');
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct IdParseError {
    expected_sigil: char,
}

impl IdParseError {
    pub(crate) const fn new(expected_sigil: char) -> Self {
        Self { expected_sigil }
    }

    /// Return the sigil required by the requested ID scope.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::SessionId;
    ///
    /// let error = "@1".parse::<SessionId>().expect_err("@ denotes a window");
    /// assert_eq!(error.expected_sigil(), '$');
    /// ```
    #[must_use]
    pub const fn expected_sigil(self) -> char {
        self.expected_sigil
    }
}

impl fmt::Display for IdParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid tmux ID: expected {} followed by an integer from 0 through {}",
            self.expected_sigil,
            u32::MAX,
        )
    }
}

impl std::error::Error for IdParseError {}

/// An error returned by libtmux.
///
/// Request-bearing variants expose a Core-scoped dispatch-request identity.
/// The Core allocates it before validation, so an error may carry an identity
/// even when no process was spawned. Clones of one [`crate::Server`] share the
/// allocating Core; independently constructed servers do not share its scope.
/// The identity is not globally unique, a process ID, an internal attempt ID,
/// or a control-mode protocol-block ID.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::ErrorKind;
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let doomed = guard.server().new_session("doomed").await?;
/// let mut stale = doomed.clone();
/// guard.server().new_session("survivor").await?;
/// doomed.kill().await?;
///
/// // A handle outliving its object is the normal way this fails, so
/// // `is_object_gone` is the branch most callers write.
/// let failure = stale.rename("renamed").await.expect_err("the session is gone");
/// assert!(failure.is_object_gone());
/// assert_eq!(failure.kind(), ErrorKind::ObjectGone);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// tmux accepted an effectful step before a later part of the operation
    /// failed.
    ///
    /// Repeating the whole operation may repeat the accepted effect. Inspect
    /// `source` to diagnose the later failure, but do not use its retryability
    /// as evidence that the whole operation is safe to replay.
    #[non_exhaustive]
    #[error("tmux accepted an effect in {operation} before a later step failed: {source}")]
    AfterEffect {
        /// A fixed operation name, without targets or argument values.
        operation: &'static str,
        /// The failure that followed the accepted effect.
        #[source]
        source: Box<Error>,
    },

    /// A server builder value was invalid.
    #[non_exhaustive]
    #[error("invalid server configuration ({kind:?})")]
    InvalidServerConfiguration {
        /// The path-free failure category.
        kind: ServerConfigurationErrorKind,
    },

    /// The output from `tmux -V` did not match a supported shape.
    #[error("invalid tmux version output")]
    InvalidVersionOutput {
        /// The number of bytes returned by tmux.
        output_len: usize,
    },

    /// The detected tmux version does not meet the supported floor.
    #[error("tmux {found} is below the minimum supported version {minimum}")]
    UnsupportedTmuxVersion {
        /// The detected tmux version.
        found: TmuxVersion,
        /// The minimum supported release.
        minimum: ReleaseVersion,
    },

    /// tmux would not accept an option name or value.
    ///
    /// Classified because the three answers call for different fixes: an
    /// unknown name is a typo, an ambiguous one needs more of the name, and a
    /// rejected value needs a different value. A caller reading stderr would
    /// have to know that tmux says "bad value" for a flag and "value is
    /// invalid" for a number.
    #[error("tmux rejected the option: {detail}")]
    OptionRejected {
        /// Which of the three answers tmux gave.
        kind: OptionErrorKind,
        /// The option name tmux could not resolve or whose value it rejected.
        detail: String,
    },

    /// The handle's scope is not one tmux keeps this option in.
    ///
    /// Raised instead of sending the write, because tmux would not refuse it.
    /// tmux resolves an option by name rather than by the flags it was sent
    /// with, so `mouse` through a pane handle becomes the whole session's
    /// `mouse` and reports success. Reading it back through the same handle
    /// resolves the same way and agrees, so nothing downstream notices.
    ///
    /// [`crate::OptionSchema::accepts`] answers ahead of the call, for a
    /// caller choosing a handle rather than reacting to a refusal.
    #[error(
        "tmux keeps {option} in {declared:?}, so writing it through a \
         {requested:?} handle would land there instead"
    )]
    OptionScopeMismatch {
        /// The option that was asked for.
        option: String,
        /// The scope the handle implies.
        requested: crate::OptionScope,
        /// Every scope tmux will actually store the option at.
        declared: &'static [crate::OptionScope],
    },

    /// tmux answered a format query with a value this crate cannot read.
    ///
    /// Reports a disagreement between the crate and the tmux that answered,
    /// not a caller mistake: the crate asked for an ID and tmux returned
    /// something that is not one. Worth reporting.
    #[non_exhaustive]
    #[error("tmux answered {format} with a value that is not an id: {detail}")]
    UnreadableFormatValue {
        /// The format the crate asked for.
        format: &'static str,
        /// What was wrong with the answer. Never retains the value.
        detail: IdParseError,
    },

    /// A different tmux daemon now holds this endpoint.
    ///
    /// The socket path is unchanged, so nothing about the address says the
    /// server was replaced. Ids are reissued from the start by the
    /// replacement, so a handle held across the restart names an object that
    /// exists and is not the one it meant.
    #[error("the tmux server was replaced: expected {expected}, found {found}")]
    ServerGenerationChanged {
        /// The daemon the caller captured.
        expected: crate::ServerGeneration,
        /// The daemon answering now.
        found: crate::ServerGeneration,
    },

    /// tmux produced more output than the dispatch was allowed to read.
    ///
    /// Not a truncation. A shortened tmux listing decodes cleanly and says
    /// something false -- fewer panes than exist -- so the dispatch fails
    /// instead, and the caller either asks tmux for less or raises
    /// [`crate::OutputLimits`].
    #[non_exhaustive]
    #[error("{command} produced more than {limit} bytes on {stream} (request {request_id})")]
    OutputLimitExceeded {
        /// Core-scoped dispatch-request identity.
        request_id: u64,
        /// Sanitized command context.
        command: CommandSummary,
        /// Which stream ran past its budget.
        stream: &'static str,
        /// The budget in bytes.
        limit: usize,
    },

    /// The server is already running as much work of this kind as it admits.
    ///
    /// The work never started, so retrying it is safe: nothing was sent
    /// to tmux and no state changed. Distinct from
    /// [`Self::Timeout`](Self::Timeout), which means the work may have run.
    #[non_exhaustive]
    #[error(
        "work was not admitted: {in_flight} already running is this kind's limit, \
         and nothing was sent, so retrying is safe (request {request_id}, {command})"
    )]
    Overloaded {
        /// Core-scoped dispatch-request identity.
        request_id: u64,
        /// Sanitized command context.
        command: CommandSummary,
        /// How many operations of this kind the server admits at once.
        in_flight: usize,
    },

    /// A control-mode frame grew past what the connection admits.
    ///
    /// Control mode reads from a process that keeps running, so the framing is
    /// the only thing bounding memory. The connection cannot be resynchronized
    /// after this -- the parser is mid-frame and does not know where the next
    /// one begins -- so it is finished, and a caller who wants to continue
    /// attaches again.
    #[cfg(feature = "control-mode")]
    #[non_exhaustive]
    #[error("a control-mode {frame} grew past its {limit} byte budget")]
    ControlModeFrameTooLarge {
        /// Which frame: a line, or a command's response block.
        frame: &'static str,
        /// The budget in bytes.
        limit: usize,
    },

    /// A session of this name already exists.
    ///
    /// Classified rather than left as a generic refusal because it is the one
    /// creation failure a caller routinely expects and handles: it means "pick
    /// another name", not "tmux is broken". Checking with `has-session` first
    /// would race, since another process can take the name in between.
    #[error("a session named {name} already exists")]
    SessionExists {
        /// The name that was already taken.
        name: String,
    },

    /// The running tmux is too old for a capability the caller asked for.
    ///
    /// Distinct from [`Error::UnsupportedTmuxVersion`], which is about the
    /// crate's own floor: this one says the crate works here and the *feature*
    /// does not. tmux itself would usually accept the flag and quietly ignore
    /// it, which turns "your tmux is too old" into "the command did nothing",
    /// so it is reported rather than passed through.
    #[error("{capability} needs tmux {needs} or newer, and this is {found}")]
    UnsupportedCapability {
        /// What the caller asked for, named as a caller would say it.
        capability: &'static str,
        /// The first release that has it.
        needs: ReleaseVersion,
        /// The release actually running.
        found: TmuxVersion,
    },

    /// tmux has this capability and the running release gets it wrong.
    ///
    /// Distinct from [`Self::UnsupportedCapability`], which means the release
    /// predates the feature and the answer is to upgrade. Here releases on
    /// both sides work, so neither "upgrade" nor "the floor is too low" is
    /// the fix: the caller has to leave a specific range.
    ///
    /// Raised rather than returning what the release reports, because what it
    /// reports is wrong in a way the caller cannot see.
    #[error(
        "tmux {found} does not implement {capability} correctly; \
         releases from {broken_in} up to but not including {fixed_in} are affected"
    )]
    CapabilityDefective {
        /// What the caller asked for, named as a caller would say it.
        capability: &'static str,
        /// The release actually running.
        found: TmuxVersion,
        /// The first release that gets it wrong.
        broken_in: ReleaseVersion,
        /// The first release that gets it right again.
        fixed_in: ReleaseVersion,
    },

    /// The version probe process returned a non-zero status.
    #[non_exhaustive]
    #[error(
        "tmux version probe request {request_id} ({command}) failed with exit code {exit_code:?} and signal {signal:?}"
    )]
    VersionProbeFailed {
        /// The Core-scoped dispatch-request identity.
        request_id: u64,
        /// The sanitized logical version-probe command.
        command: CommandSummary,
        /// The process exit code, when it exited normally.
        exit_code: Option<i32>,
        /// The terminating signal, when it did not exit normally.
        signal: Option<i32>,
    },

    /// A command or executable contained a byte that cannot be passed to a process.
    #[non_exhaustive]
    #[error("invalid {input} for tmux request {request_id}")]
    InvalidCommandInput {
        /// The Core-scoped dispatch-request identity.
        request_id: u64,
        /// The validated input category.
        input: &'static str,
    },

    /// Handles passed to one operation belong to different tmux endpoints.
    ///
    /// An endpoint here is a socket path, which is what separates two servers
    /// running at once. It does not separate a server from the one that
    /// replaced it on the same socket: that daemon reissues ids from the
    /// start, so a handle held across the restart names something live and is
    /// not refused. [`crate::Server::require_generation`] is what tells those two
    /// apart, and it costs the round trip this check does not spend.
    #[non_exhaustive]
    #[error("{operation} requires handles from the same tmux server endpoint")]
    ServerMismatch {
        /// The operation that rejected the foreign handle.
        operation: &'static str,
    },

    /// A plan has a dependency that cannot be resolved before dispatch.
    #[cfg(feature = "plan")]
    #[error("invalid plan: {source}")]
    InvalidPlan {
        /// The payload-free dependency failure.
        #[source]
        source: crate::plan::PlanValidationError,
    },

    /// The configured tmux executable was not found.
    #[non_exhaustive]
    #[error("tmux executable was not found for request {request_id} ({command})")]
    ExecutableNotFound {
        /// The Core-scoped dispatch-request identity.
        request_id: u64,
        /// The sanitized logical command.
        command: CommandSummary,
        /// The operating-system spawn error.
        #[source]
        source: io::Error,
    },

    /// The tmux process could not be started.
    #[non_exhaustive]
    #[error("failed to start tmux request {request_id} ({command})")]
    Spawn {
        /// The Core-scoped dispatch-request identity.
        request_id: u64,
        /// The sanitized logical command.
        command: CommandSummary,
        /// The operating-system spawn error.
        #[source]
        source: io::Error,
    },

    /// A captured output stream could not be drained.
    #[non_exhaustive]
    #[error("failed to read {stream} for tmux request {request_id} ({command})")]
    ReadOutput {
        /// The Core-scoped dispatch-request identity.
        request_id: u64,
        /// The sanitized logical command.
        command: CommandSummary,
        /// The output stream that failed.
        stream: &'static str,
        /// The source error category without its potentially unsafe message.
        kind: io::ErrorKind,
    },

    /// The direct tmux child could not be awaited.
    #[non_exhaustive]
    #[error("failed to wait for tmux request {request_id} ({command})")]
    WaitChild {
        /// The Core-scoped dispatch-request identity.
        request_id: u64,
        /// The sanitized logical command.
        command: CommandSummary,
        /// The operating-system wait error.
        #[source]
        source: io::Error,
    },

    /// A tmux request exceeded its configured deadline.
    #[non_exhaustive]
    #[error("tmux request {request_id} ({command}) timed out after {timeout:?}")]
    Timeout {
        /// The Core-scoped dispatch-request identity.
        request_id: u64,
        /// The sanitized logical command.
        command: CommandSummary,
        /// The configured deadline.
        timeout: Duration,
    },

    /// The executor has stopped accepting requests.
    ///
    /// Shutdown is permanent for every handle sharing this Core. Build another
    /// [`crate::Server`] to issue more requests.
    #[non_exhaustive]
    #[error("tmux executor is shut down for request {request_id} ({command})")]
    ExecutorShutdown {
        /// The Core-scoped dispatch-request identity.
        request_id: u64,
        /// The sanitized logical command.
        command: CommandSummary,
    },

    /// A Core-scoped dispatch-request identity is already active in this
    /// executor.
    #[non_exhaustive]
    #[error("tmux request {request_id} is already active ({command})")]
    DuplicateRequest {
        /// The duplicate Core-scoped dispatch-request identity.
        request_id: u64,
        /// The sanitized logical command.
        command: CommandSummary,
    },

    /// The independent supervisor ended unexpectedly after cleaning up its child.
    #[non_exhaustive]
    #[error("tmux supervisor was lost for request {request_id} ({command})")]
    SupervisorLost {
        /// The Core-scoped dispatch-request identity.
        request_id: u64,
        /// The sanitized logical command.
        command: CommandSummary,
    },

    /// A refresh could not find the object it was asked to update.
    ///
    /// This is distinct from a connection failure: tmux answered, and the
    /// object was not among the results. It has been closed or killed since
    /// the handle was created.
    ///
    /// For a client, absence from a listing has one other cause that is not
    /// this: a stopped client is left out of the same listing and comes back.
    /// That is [`Self::ClientSuspended`], reported separately so that
    /// [`Self::is_object_gone`] keeps meaning "stop using this handle".
    #[non_exhaustive]
    #[error("tmux no longer has {kind} {id}")]
    ObjectGone {
        /// The kind of object that disappeared.
        kind: ObjectKind,
        /// The tmux identity that is no longer present.
        id: String,
    },

    /// A target found nothing, which does not prove the object is gone.
    ///
    /// Distinct from [`Self::ObjectGone`], and the distinction decides whether
    /// a caller may discard a handle: the object may be perfectly alive,
    /// linked into another session or sitting at another index. What is known
    /// is only that this target resolved to nothing.
    ///
    /// tmux answers a missing target by echoing it back, and the echo settles
    /// which of the two happened for some target forms and not others. A
    /// coordinate -- an index, or a window name, written `session:index` --
    /// is scoped to one session and is not unique on the server, so its
    /// absence is always this error and never the other. An identity carries
    /// its kind's sigil (`@` a window, `%` a pane) and echoes identically
    /// whether the object died or merely lives under another session's link,
    /// so telling those apart costs a lookup and belongs to the caller that
    /// can afford one.
    ///
    /// [`Self::is_object_gone`] answers `false`: a handle whose object is
    /// still running must not be dropped on this evidence.
    #[non_exhaustive]
    #[error("tmux has no {kind} at {target}")]
    LinkGone {
        /// The kind of object the target named.
        kind: ObjectKind,
        /// The target that found nothing, spelled as it was sent.
        target: String,
    },

    /// A client is stopped rather than gone, so listings leave it out.
    ///
    /// Distinct from [`Self::ObjectGone`], and the distinction decides whether
    /// to drop the handle: a suspended client is still on the server and is
    /// listed again once it resumes, so the same handle keeps working.
    /// [`Self::ObjectGone`] means it will not.
    ///
    /// tmux omits a suspended client from `list-clients` while still resolving
    /// it as a command target, which is what makes the two tellable apart. The
    /// listing filters the dead, the exiting and the suspended together, so
    /// absence from it does not say which of the three happened.
    ///
    /// Both [`crate::Client::suspend`] and locking a client arrive here,
    /// because tmux marks them with one flag. A client resumes when its
    /// process continues -- the suspended one on `SIGCONT`, the locked one
    /// when its `lock-command` exits.
    #[non_exhaustive]
    #[error("client {name} is suspended, not gone")]
    ClientSuspended {
        /// The client's tmux name, which is the path of its terminal.
        name: String,
    },

    /// A control-mode connection failed.
    #[cfg(feature = "control-mode")]
    #[non_exhaustive]
    #[error("control mode connection failed ({kind:?})")]
    ControlMode {
        /// Which stage of the connection failed.
        kind: ControlModeErrorKind,
        /// The operating-system error, when there was one.
        #[source]
        source: Option<io::Error>,
    },

    /// A blocking runtime could not be created.
    #[non_exhaustive]
    #[error("could not build a runtime")]
    RuntimeUnavailable {
        /// The operating-system error.
        #[source]
        source: io::Error,
    },

    /// A blocking runtime was driven from inside another runtime.
    ///
    /// A runtime cannot be driven from within one, so [`crate::blocking::Runtime::run`]
    /// panics here. [`crate::blocking::Runtime::try_run`] returns this instead,
    /// for callers who would rather handle it: await the future directly.
    #[error("a blocking runtime cannot be driven from inside an async context")]
    RuntimeNested,

    /// The tmux server the command needed was not there.
    ///
    /// tmux exits 1 for this and for a command it refused, and separates them
    /// only in stderr, so this is read from the message rather than the
    /// status. [`ServerGoneKind`] says which way it was missing; the raw
    /// stderr remains available only through [`crate::Server::cmd`].
    #[error("tmux found no server for {command}: {kind}")]
    ServerGone {
        /// The tmux command that found no server.
        command: &'static str,
        /// Which way the server was not there.
        kind: ServerGoneKind,
    },

    /// tmux rejected a command that the crate requires to succeed.
    ///
    /// The raw [`crate::Server::cmd`] boundary keeps a nonzero status as data.
    /// This variant is for operations whose whole purpose is the effect, so a
    /// refusal is a failure rather than a result.
    #[non_exhaustive]
    #[error("tmux rejected {command} (exit {exit_code:?}): {stderr}")]
    CommandFailed {
        /// The tmux command that was rejected.
        command: &'static str,
        /// The process exit code, when it exited normally.
        exit_code: Option<i32>,
        /// The message tmux printed, or a fixed explanation when retaining it
        /// could disclose sensitive input.
        stderr: String,
    },

    /// tmux listing output could not be decoded into typed snapshots.
    ///
    /// This reports a disagreement between the crate and the tmux that
    /// answered, not an ordinary tmux failure. A command that merely reports a
    /// nonzero status stays raw data at the [`crate::Server::cmd`] boundary.
    #[non_exhaustive]
    #[error("failed to decode {list_command} output: {detail}")]
    DecodeListing {
        /// The tmux list command whose output failed to decode.
        list_command: &'static str,
        /// Payload-free decoding metadata.
        detail: ListingDecodeError,
    },
}

/// The kind of tmux object a failure refers to.
///
/// # Examples
///
/// ```
/// use libtmux::ObjectKind;
///
/// // Carried by `Error::ObjectGone` so a caller can say what disappeared
/// // without parsing the message.
/// assert_eq!(ObjectKind::Pane.to_string(), "pane");
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ObjectKind {
    /// A tmux session.
    Session,
    /// A tmux window.
    Window,
    /// A tmux pane.
    Pane,
    /// A client attached to the server.
    Client,
}

impl fmt::Display for ObjectKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Session => "session",
            Self::Window => "window",
            Self::Pane => "pane",
            Self::Client => "client",
        })
    }
}

/// Payload-free metadata describing why tmux output could not be decoded.
///
/// This never retains row bytes, snapshot text, or decoded values, so it is
/// safe to log wherever the rest of [`Error`] is.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use libtmux::Error;
///
/// // Reached through the one variant that carries it. Both accessors are
/// // optional because tmux does not always give enough to locate the row.
/// fn locate(failure: &Error) -> Option<(&'static str, Option<usize>)> {
///     match failure {
///         Error::DecodeListing { list_command, detail, .. } => {
///             Some((*list_command, detail.row()))
///         }
///         _ => None,
///     }
/// }
///
/// // The payload is metadata only: no tmux bytes are retained, so logging it
/// // cannot leak a pane's contents.
/// let other = libtmux::Server::builder()
///     .socket_name("named")
///     .socket_path("/tmp/libtmux-rs-dev/explicit")
///     .build()
///     .expect_err("two socket selectors");
/// assert_eq!(locate(&other), None);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListingDecodeError {
    inner: crate::formats::FormatCodecError,
}

impl ListingDecodeError {
    pub(crate) const fn new(inner: crate::formats::FormatCodecError) -> Self {
        Self { inner }
    }

    /// Return the zero-based row that failed, when the failure reached a row.
    ///
    /// Plan-construction failures happen before any row is read and report
    /// `None`.
    #[must_use]
    pub const fn row(&self) -> Option<usize> {
        self.inner.row()
    }

    /// Return the stable tmux format name that failed, when one is known.
    #[must_use]
    pub const fn field_name(&self) -> Option<&'static str> {
        self.inner.field_name()
    }
}

impl fmt::Display for ListingDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(formatter)
    }
}

impl std::error::Error for ListingDecodeError {}

impl Error {
    #[cfg(feature = "control-mode")]
    pub(crate) const fn control_mode(source: io::Error) -> Self {
        Self::ControlMode {
            kind: ControlModeErrorKind::Transport,
            source: Some(source),
        }
    }

    /// tmux started but did not provide the pipes to talk over.
    #[cfg(feature = "control-mode")]
    pub(crate) const fn control_mode_pipes() -> Self {
        Self::ControlMode {
            kind: ControlModeErrorKind::MissingPipes,
            source: None,
        }
    }

    /// A command carries an argument no control-mode line can express.
    #[cfg(feature = "control-mode")]
    pub(crate) const fn control_mode_unrepresentable() -> Self {
        Self::ControlMode {
            kind: ControlModeErrorKind::UnrepresentableCommand,
            source: None,
        }
    }

    /// A protocol frame ran past its budget.
    ///
    /// Not recoverable in place: the parser is mid-frame and cannot know where
    /// the next one starts, so the connection is finished and the caller
    /// reopens.
    #[cfg(feature = "control-mode")]
    pub(crate) const fn control_mode_frame_too_large(frame: &'static str, limit: usize) -> Self {
        Self::ControlModeFrameTooLarge { frame, limit }
    }

    /// Nobody took the events, so the reply could not be reached.
    #[cfg(feature = "control-mode")]
    pub(crate) const fn control_mode_unread() -> Self {
        Self::ControlMode {
            kind: ControlModeErrorKind::Unread,
            source: None,
        }
    }

    /// A subscription name tmux would read as something else.
    #[cfg(feature = "control-mode")]
    pub(crate) const fn control_mode_invalid_subscription() -> Self {
        Self::ControlMode {
            kind: ControlModeErrorKind::InvalidSubscriptionName,
            source: None,
        }
    }

    /// The connection closed before the command was answered.
    #[cfg(feature = "control-mode")]
    pub(crate) const fn control_mode_closed() -> Self {
        Self::ControlMode {
            kind: ControlModeErrorKind::Closed,
            source: None,
        }
    }

    /// The command deadline elapsed before the connection committed a write.
    #[cfg(feature = "control-mode")]
    pub(crate) const fn control_mode_dispatch_timeout() -> Self {
        Self::ControlMode {
            kind: ControlModeErrorKind::DispatchTimedOut,
            source: None,
        }
    }

    /// The connection did not attach or resolve a command in time.
    #[cfg(feature = "control-mode")]
    pub(crate) const fn control_mode_timeout() -> Self {
        Self::ControlMode {
            kind: ControlModeErrorKind::TimedOut,
            source: None,
        }
    }

    #[cfg(feature = "blocking")]
    pub(crate) const fn runtime_unavailable(source: io::Error) -> Self {
        Self::RuntimeUnavailable { source }
    }

    pub(crate) const fn invalid_server_configuration(kind: ServerConfigurationErrorKind) -> Self {
        Self::InvalidServerConfiguration { kind }
    }

    pub(crate) fn version_probe_failed(
        request_id: u64,
        command: CommandSummary,
        exit_code: Option<i32>,
        signal: Option<i32>,
    ) -> Self {
        Self::VersionProbeFailed {
            request_id,
            command,
            exit_code,
            signal,
        }
    }

    pub(crate) fn from_invalid_version_output(output_len: usize) -> Self {
        Self::InvalidVersionOutput { output_len }
    }

    pub(crate) fn unsupported_tmux_version(found: TmuxVersion, minimum: ReleaseVersion) -> Self {
        Self::UnsupportedTmuxVersion { found, minimum }
    }

    pub(crate) fn invalid_command_input(request_id: u64, input: &'static str) -> Self {
        Self::InvalidCommandInput { request_id, input }
    }

    pub(crate) const fn server_mismatch(operation: &'static str) -> Self {
        Self::ServerMismatch { operation }
    }

    pub(crate) fn spawn(
        request_id: u64,
        command: CommandSummary,
        source: io::Error,
        executable_not_found: bool,
    ) -> Self {
        if executable_not_found {
            Self::ExecutableNotFound {
                request_id,
                command,
                source,
            }
        } else {
            Self::Spawn {
                request_id,
                command,
                source,
            }
        }
    }

    pub(crate) fn read_output(
        request_id: u64,
        command: CommandSummary,
        stream: &'static str,
        kind: io::ErrorKind,
    ) -> Self {
        Self::ReadOutput {
            request_id,
            command,
            stream,
            kind,
        }
    }

    pub(crate) fn wait_child(request_id: u64, command: CommandSummary, source: io::Error) -> Self {
        Self::WaitChild {
            request_id,
            command,
            source,
        }
    }

    pub(crate) fn timeout(request_id: u64, command: CommandSummary, timeout: Duration) -> Self {
        Self::Timeout {
            request_id,
            command,
            timeout,
        }
    }

    pub(crate) fn executor_shutdown(request_id: u64, command: CommandSummary) -> Self {
        Self::ExecutorShutdown {
            request_id,
            command,
        }
    }

    pub(crate) fn duplicate_request(request_id: u64, command: CommandSummary) -> Self {
        Self::DuplicateRequest {
            request_id,
            command,
        }
    }

    pub(crate) fn supervisor_lost(request_id: u64, command: CommandSummary) -> Self {
        Self::SupervisorLost {
            request_id,
            command,
        }
    }

    /// Return the length of the invalid `tmux -V` output, when present.
    ///
    /// The error never retains the process output itself.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::TmuxVersion;
    ///
    /// let output = b"invalid\n";
    /// let error = TmuxVersion::parse_output(output).expect_err("output is invalid");
    /// assert_eq!(error.invalid_version_output_len(), Some(output.len()));
    /// ```
    #[must_use]
    pub fn invalid_version_output_len(&self) -> Option<usize> {
        match self {
            Self::InvalidVersionOutput { output_len } => Some(*output_len),
            _ => None,
        }
    }

    /// Return the detected version for a minimum-version error.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::TmuxVersion;
    ///
    /// let version = TmuxVersion::parse_output(b"tmux 3.2\n")?;
    /// let error = version.ensure_supported().expect_err("3.2 is unsupported");
    /// assert_eq!(error.found_version(), Some(&version));
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use]
    pub fn found_version(&self) -> Option<&TmuxVersion> {
        match self {
            Self::UnsupportedTmuxVersion { found, .. }
            | Self::UnsupportedCapability { found, .. } => Some(found),
            _ => None,
        }
    }

    /// Return the required release for a minimum-version error.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::TmuxVersion;
    ///
    /// let version = TmuxVersion::parse_output(b"tmux 3.2\n")?;
    /// let error = version.ensure_supported().expect_err("3.2 is unsupported");
    /// assert_eq!(error.minimum_version(), Some(&TmuxVersion::MIN_SUPPORTED));
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use]
    pub fn minimum_version(&self) -> Option<&ReleaseVersion> {
        match self {
            Self::UnsupportedTmuxVersion { minimum, .. } => Some(minimum),
            Self::UnsupportedCapability { needs, .. } => Some(needs),
            _ => None,
        }
    }
}

impl fmt::Debug for Error {
    #[allow(
        clippy::too_many_lines,
        reason = "exhaustive safe formatting keeps every public error variant byte-free"
    )]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AfterEffect { operation, source } => formatter
                .debug_struct("AfterEffect")
                .field("operation", operation)
                .field("source", source)
                .finish(),
            Self::OptionScopeMismatch {
                option,
                requested,
                declared,
            } => formatter
                .debug_struct("OptionScopeMismatch")
                .field("option", option)
                .field("requested", requested)
                .field("declared", declared)
                .finish(),
            Self::RuntimeNested => formatter.debug_struct("RuntimeNested").finish(),
            Self::InvalidServerConfiguration { kind } => formatter
                .debug_struct("InvalidServerConfiguration")
                .field("kind", kind)
                .finish(),
            Self::UnsupportedCapability {
                capability,
                needs,
                found,
            } => formatter
                .debug_struct("UnsupportedCapability")
                .field("capability", capability)
                .field("needs", needs)
                .field("found", found)
                .finish(),
            Self::CapabilityDefective {
                capability,
                found,
                broken_in,
                fixed_in,
            } => formatter
                .debug_struct("CapabilityDefective")
                .field("capability", capability)
                .field("found", found)
                .field("broken_in", broken_in)
                .field("fixed_in", fixed_in)
                .finish(),
            Self::UnreadableFormatValue { format, detail } => formatter
                .debug_struct("UnreadableFormatValue")
                .field("format", format)
                .field("detail", detail)
                .finish(),
            #[cfg(feature = "control-mode")]
            Self::ControlModeFrameTooLarge { frame, limit } => formatter
                .debug_struct("ControlModeFrameTooLarge")
                .field("frame", frame)
                .field("limit", limit)
                .finish(),
            Self::OutputLimitExceeded {
                request_id,
                command,
                stream,
                limit,
            } => formatter
                .debug_struct("OutputLimitExceeded")
                .field("request_id", request_id)
                .field("command", command)
                .field("stream", stream)
                .field("limit", limit)
                .finish(),
            Self::Overloaded {
                request_id,
                command,
                in_flight,
            } => formatter
                .debug_struct("Overloaded")
                .field("request_id", request_id)
                .field("command", command)
                .field("in_flight", in_flight)
                .finish(),
            Self::ServerGenerationChanged { expected, found } => formatter
                .debug_struct("ServerGenerationChanged")
                .field("expected", expected)
                .field("found", found)
                .finish(),
            Self::OptionRejected { kind, detail } => formatter
                .debug_struct("OptionRejected")
                .field("kind", kind)
                .field("detail", detail)
                .finish(),
            Self::SessionExists { name } => formatter
                .debug_struct("SessionExists")
                .field("name", name)
                .finish(),
            Self::InvalidVersionOutput { output_len } => formatter
                .debug_struct("InvalidVersionOutput")
                .field("output_len", output_len)
                .finish(),
            Self::UnsupportedTmuxVersion { found, minimum } => formatter
                .debug_struct("UnsupportedTmuxVersion")
                .field("found", found)
                .field("minimum", minimum)
                .finish(),
            Self::VersionProbeFailed {
                request_id,
                command,
                exit_code,
                signal,
            } => formatter
                .debug_struct("VersionProbeFailed")
                .field("request_id", request_id)
                .field("command", command)
                .field("exit_code", exit_code)
                .field("signal", signal)
                .finish_non_exhaustive(),
            Self::InvalidCommandInput { request_id, input } => formatter
                .debug_struct("InvalidCommandInput")
                .field("request_id", request_id)
                .field("input", input)
                .finish(),
            Self::ServerMismatch { operation } => formatter
                .debug_struct("ServerMismatch")
                .field("operation", operation)
                .finish(),
            #[cfg(feature = "plan")]
            Self::InvalidPlan { source } => formatter
                .debug_struct("InvalidPlan")
                .field("source", source)
                .finish(),
            Self::ExecutableNotFound {
                request_id,
                command,
                source,
            } => formatter
                .debug_struct("ExecutableNotFound")
                .field("request_id", request_id)
                .field("command", command)
                .field("source", source)
                .finish(),
            Self::Spawn {
                request_id,
                command,
                source,
            } => formatter
                .debug_struct("Spawn")
                .field("request_id", request_id)
                .field("command", command)
                .field("source", source)
                .finish(),
            Self::ReadOutput {
                request_id,
                command,
                stream,
                kind,
            } => formatter
                .debug_struct("ReadOutput")
                .field("request_id", request_id)
                .field("command", command)
                .field("stream", stream)
                .field("kind", kind)
                .finish(),
            Self::WaitChild {
                request_id,
                command,
                source,
            } => formatter
                .debug_struct("WaitChild")
                .field("request_id", request_id)
                .field("command", command)
                .field("source", source)
                .finish(),
            Self::Timeout {
                request_id,
                command,
                timeout,
            } => formatter
                .debug_struct("Timeout")
                .field("request_id", request_id)
                .field("command", command)
                .field("timeout", timeout)
                .finish(),
            Self::ExecutorShutdown {
                request_id,
                command,
            } => formatter
                .debug_struct("ExecutorShutdown")
                .field("request_id", request_id)
                .field("command", command)
                .finish(),
            Self::DuplicateRequest {
                request_id,
                command,
            } => formatter
                .debug_struct("DuplicateRequest")
                .field("request_id", request_id)
                .field("command", command)
                .finish(),
            Self::SupervisorLost {
                request_id,
                command,
            } => formatter
                .debug_struct("SupervisorLost")
                .field("request_id", request_id)
                .field("command", command)
                .finish(),
            Self::LinkGone { kind, target } => formatter
                .debug_struct("LinkGone")
                .field("kind", kind)
                .field("target", target)
                .finish(),
            Self::ClientSuspended { name } => formatter
                .debug_struct("ClientSuspended")
                .field("name", name)
                .finish(),
            #[cfg(feature = "control-mode")]
            Self::ControlMode { kind, source } => formatter
                .debug_struct("ControlMode")
                .field("kind", kind)
                .field("source_kind", &source.as_ref().map(io::Error::kind))
                .finish(),
            Self::RuntimeUnavailable { source } => formatter
                .debug_struct("RuntimeUnavailable")
                .field("kind", &source.kind())
                .finish(),
            Self::CommandFailed {
                command,
                exit_code,
                stderr,
            } => formatter
                .debug_struct("CommandFailed")
                .field("command", command)
                .field("exit_code", exit_code)
                .field("stderr", stderr)
                .finish(),
            Self::ObjectGone { kind, id } => formatter
                .debug_struct("ObjectGone")
                .field("kind", kind)
                .field("id", id)
                .finish(),
            Self::ServerGone { command, kind } => formatter
                .debug_struct("ServerGone")
                .field("command", command)
                .field("kind", kind)
                .finish(),
            Self::DecodeListing {
                list_command,
                detail,
            } => formatter
                .debug_struct("DecodeListing")
                .field("list_command", list_command)
                .field("detail", detail)
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod compat_tests;
