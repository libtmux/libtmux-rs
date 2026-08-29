//! Errors returned by libtmux.

use std::fmt;
use std::io;
use std::time::Duration;

use crate::CommandSummary;
use crate::version::{ReleaseVersion, TmuxVersion};

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
    /// The attach opening or a command response exceeded the server deadline.
    ///
    /// A command's deadline starts before it is written and runs until tmux
    /// closes its response block.
    /// The connection ends at this boundary; attach another one to continue.
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
    /// The caller passed something that cannot be sent to tmux.
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

    /// The server is already running as much work as it admits.
    ///
    /// The dispatch never started, so retrying it is safe: nothing was sent
    /// to tmux and no state changed. Distinct from
    /// [`Self::Timeout`](Self::Timeout), which means the work may have run.
    #[non_exhaustive]
    #[error(
        "a dispatch was not admitted: {in_flight} already running is this endpoint's limit, \
         and nothing was sent, so retrying is safe (request {request_id}, {command})"
    )]
    Overloaded {
        /// Core-scoped dispatch-request identity.
        request_id: u64,
        /// Sanitized command context.
        command: CommandSummary,
        /// How many dispatches the server admits at once.
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

/// Say whether a target tmux echoed back names an object or a place.
///
/// tmux gives every object an id carrying a sigil -- `$` a session, `@` a
/// window, `%` a pane -- unique for the life of the server. Every other
/// spelling is scoped to something that can be renumbered or reused: an index
/// belongs to a session, a window name belongs to a session, and neither
/// survives as a way to name one particular object.
///
/// A session is the exception, because `-t` takes a session's name and tmux
/// keeps those unique, so a bare word there is still an identity. So is a
/// client, which tmux names by its terminal.
///
/// Measured against tmux 3.2a through 3.7b; see `docs/design.md`.
const fn is_identity(kind: ObjectKind, target: &str) -> bool {
    match kind {
        ObjectKind::Session | ObjectKind::Client => true,
        ObjectKind::Window => matches!(target.as_bytes().first(), Some(b'@')),
        ObjectKind::Pane => matches!(target.as_bytes().first(), Some(b'%')),
    }
}

impl Error {
    /// Mark this failure as following an effect that tmux accepted.
    ///
    /// Use a fixed operation name without targets or argument values. Calling
    /// this method on an already marked error leaves its existing operation
    /// intact, so nested composed operations do not obscure the more specific
    /// replay boundary.
    ///
    /// The returned error has [`ErrorKind::PartialEffect`] and
    /// [`Self::is_transient`] returns `false`, regardless of the source.
    #[must_use]
    pub fn after_effect(self, operation: &'static str) -> Self {
        match self {
            Self::AfterEffect { .. } => self,
            source => Self::AfterEffect {
                operation,
                source: Box::new(source),
            },
        }
    }

    /// Classify a refused tmux command, recognizing a target that has gone.
    ///
    /// tmux reports a missing target as `can't find <kind>: <target>` and
    /// exits 1, the same status it uses for an argument it did not like, so
    /// the message is the only thing that separates them. It is not
    /// localized -- tmux has no message catalogue -- and the wording has been
    /// stable across every supported release.
    ///
    /// Anything that does not match stays a refusal, so a future rewording
    /// costs the distinction rather than correctness.
    /// `target` is the request's own `-t`, when it had one. tmux reports a
    /// server holding no sessions as `no current target` even for a target it
    /// was given, so the request is what recovers the name.
    pub(crate) fn refused(
        command: &'static str,
        exit_code: Option<i32>,
        stderr: String,
        target: Option<&std::ffi::OsStr>,
    ) -> Self {
        // The wording is tmux's own and is identical on every supported
        // release. None of these say the request was wrong, so they are read
        // before anything that does.
        const GONE: [(&str, ServerGoneKind); 4] = [
            ("no server running on", ServerGoneKind::NotRunning),
            ("error connecting to", ServerGoneKind::Unreachable),
            // Before the shorter one, which it starts with and does not mean.
            ("server exited unexpectedly", ServerGoneKind::Lost),
            ("server exited", ServerGoneKind::Stopped),
        ];

        // tmux has two vocabularies for the same fact and they come from
        // different files. `cmd-find.c` resolves a target and says "can't
        // find"; `options.c` and the environment commands resolve their own
        // and say "no such". A caller asking `is_object_gone` about one dead
        // session got `true` from `windows()` and `false` from `get_option`
        // until both were matched here.
        const MISSING: [(&str, ObjectKind); 7] = [
            ("can't find session:", ObjectKind::Session),
            ("can't find window:", ObjectKind::Window),
            ("can't find pane:", ObjectKind::Pane),
            ("can't find client:", ObjectKind::Client),
            ("no such session:", ObjectKind::Session),
            ("no such window:", ObjectKind::Window),
            ("no such pane:", ObjectKind::Pane),
        ];

        // tmux spells "no such option name" two ways. `set-option` and
        // `show-options` resolve the name with `options_match` first, which
        // says "invalid option"; the "unknown option" in `options_scope_from_name`
        // sits behind that call and so is unreachable from the CLI on every
        // supported release. Both mean the same thing, so both map to the same
        // kind rather than leaving a hole if tmux ever reorders the two.
        const OPTION: [(&str, OptionErrorKind); 5] = [
            ("invalid option:", OptionErrorKind::Unknown),
            ("unknown option:", OptionErrorKind::Unknown),
            ("ambiguous option:", OptionErrorKind::Ambiguous),
            ("bad value:", OptionErrorKind::BadValue),
            ("value is invalid:", OptionErrorKind::BadValue),
        ];

        for (prefix, kind) in GONE {
            if stderr.trim_end().starts_with(prefix) {
                return Self::ServerGone { command, kind };
            }
        }

        for (prefix, kind) in OPTION {
            if let Some(detail) = stderr.trim_end().strip_prefix(prefix) {
                return Self::OptionRejected {
                    kind,
                    detail: detail.trim().to_owned(),
                };
            }
        }

        if let Some(name) = stderr.trim_end().strip_prefix("duplicate session:") {
            return Self::SessionExists {
                name: name.trim().to_owned(),
            };
        }

        if let Some(target) = target.filter(|_| stderr.trim_end() == NO_CURRENT_TARGET) {
            return Self::object_gone(&target.to_string_lossy());
        }

        for (prefix, kind) in MISSING {
            let Some(echo) = stderr.trim_end().strip_prefix(prefix) else {
                continue;
            };
            let echo = echo.trim();

            // tmux echoes the part of the target it could not resolve, and
            // that echo says which fact it established. A coordinate -- an
            // index, or a window name -- is scoped to one session, so its
            // absence means that session holds nothing there and says nothing
            // about any object. Reporting it as an identity would name a
            // different object, and `is_object_gone` would tell the caller to
            // drop a handle that still works.
            if is_identity(kind, echo) {
                return Self::ObjectGone {
                    kind,
                    id: echo.to_owned(),
                };
            }

            // The echo drops the session, so the request is what still knows
            // the whole target. Without one, the echo alone is what is true.
            return Self::LinkGone {
                kind,
                target: target.map_or_else(
                    || echo.to_owned(),
                    |sent| sent.to_string_lossy().into_owned(),
                ),
            };
        }

        Self::CommandFailed {
            command,
            exit_code,
            stderr,
        }
    }

    /// Report a refusal without retaining tmux output.
    pub(crate) fn refused_withheld(command: &'static str, exit_code: Option<i32>) -> Self {
        Self::CommandFailed {
            command,
            exit_code,
            stderr: SENSITIVE_OUTPUT_WITHHELD.to_owned(),
        }
    }

    /// Classify a nonzero result, withholding output after sensitive input.
    pub(crate) fn from_refused_result(
        command: &'static str,
        result: &crate::CommandResult,
        target: Option<&std::ffi::OsStr>,
    ) -> Self {
        let stderr = result.stderr_lossy().into_owned();
        if result.command().sensitive_argument_count() > 0 {
            let classified = Self::refused(command, result.exit_code(), stderr, target);
            if matches!(
                &classified,
                Self::ObjectGone { id, .. }
                    if target.is_some_and(|target| id == &target.to_string_lossy())
            ) || matches!(classified, Self::ServerGone { .. })
            {
                return classified;
            }
            Self::refused_withheld(command, result.exit_code())
        } else {
            Self::refused(command, result.exit_code(), stderr, target)
        }
    }

    /// Report a tmux target that could not be resolved.
    ///
    /// The kind comes from the sigil, which is how tmux names its objects.
    /// A target that is a name rather than an ID is reported as a session,
    /// because a name is what `-t` accepts for one.
    fn object_gone(target: &str) -> Self {
        Self::ObjectGone {
            kind: match target.as_bytes().first() {
                Some(b'@') => ObjectKind::Window,
                Some(b'%') => ObjectKind::Pane,
                _ => ObjectKind::Session,
            },
            id: target.to_owned(),
        }
    }

    /// Return what this failure means for the caller.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// # let guard = libtmux::test::TestServer::new().await?;
    /// # let server = guard.server();
    /// use libtmux::ErrorKind;
    ///
    /// // The shape this exists for: use it if it is there, make it if not.
    /// let session = match server.session("work").await? {
    ///     Some(session) => session,
    ///     None => server.new_session("work").await?,
    /// };
    ///
    /// // And when an operation races something else removing it. The handle
    /// // is cloned because killing consumes one, which is how the crate
    /// // stops you from using a window you just destroyed.
    /// let window = session.new_window("doomed").await?;
    /// let mut stale = window.clone();
    /// window.kill().await?;
    ///
    /// let error = stale.rename("gone").await.expect_err("the window was killed");
    /// assert_eq!(error.kind(), ErrorKind::ObjectGone);
    /// assert!(error.is_object_gone());
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::AfterEffect { .. } => ErrorKind::PartialEffect,
            // A replaced daemon reissues ids from the start, so every handle
            // captured from the previous one names something that is not
            // there. That is the same decision as a missing object, and the
            // same branch a caller already writes for one.
            Self::ObjectGone { .. } | Self::ServerGenerationChanged { .. } => ErrorKind::ObjectGone,
            // Not `ObjectGone`: the object may still exist, so a caller must
            // not read this as a reason to drop the handle.
            Self::LinkGone { .. } => ErrorKind::Refused,
            // tmux carried out neither a read nor a change: the client is
            // there but not answering. `Refused` rather than `ObjectGone`
            // so a caller keeps the handle.
            Self::ClientSuspended { .. } => ErrorKind::Refused,
            Self::ServerGone { .. } => ErrorKind::ServerGone,
            Self::CommandFailed { .. }
            | Self::OutputLimitExceeded { .. }
            | Self::Overloaded { .. }
            | Self::SessionExists { .. }
            | Self::OptionRejected { .. } => ErrorKind::Refused,
            Self::Timeout { .. } => ErrorKind::Timeout,
            Self::ExecutableNotFound { .. }
            | Self::InvalidServerConfiguration { .. }
            | Self::RuntimeUnavailable { .. } => ErrorKind::Unreachable,
            // The call is wrong, not the environment: the same future awaited
            // directly would work.
            Self::RuntimeNested => ErrorKind::InvalidInput,
            Self::UnsupportedTmuxVersion { .. }
            | Self::UnsupportedCapability { .. }
            | Self::CapabilityDefective { .. } => ErrorKind::UnsupportedVersion,
            Self::InvalidCommandInput { .. } => ErrorKind::InvalidInput,
            #[cfg(feature = "plan")]
            Self::InvalidPlan { .. } => ErrorKind::InvalidInput,
            Self::Spawn { .. }
            | Self::ReadOutput { .. }
            | Self::WaitChild { .. }
            | Self::VersionProbeFailed { .. }
            | Self::ExecutorShutdown { .. }
            | Self::DuplicateRequest { .. }
            | Self::SupervisorLost { .. } => ErrorKind::Transport,
            Self::InvalidVersionOutput { .. }
            | Self::DecodeListing { .. }
            | Self::UnreadableFormatValue { .. } => ErrorKind::Decode,
            #[cfg(feature = "control-mode")]
            Self::ControlModeFrameTooLarge { .. } => ErrorKind::Decode,
            #[cfg(feature = "control-mode")]
            Self::ControlMode { kind, .. } => match kind {
                ControlModeErrorKind::UnrepresentableCommand
                | ControlModeErrorKind::InvalidSubscriptionName => ErrorKind::InvalidInput,
                // A limit was reached and the command was not carried out,
                // which is what `Refused` says. The connection is fine.
                ControlModeErrorKind::Unread => ErrorKind::Refused,
                ControlModeErrorKind::TimedOut => ErrorKind::Timeout,
                ControlModeErrorKind::Transport
                | ControlModeErrorKind::MissingPipes
                | ControlModeErrorKind::Closed => ErrorKind::Transport,
            },
        }
    }

    /// Report whether tmux no longer has the object the call named.
    ///
    /// The most common branch a caller writes, and the one that is easy to
    /// get wrong: an object disappearing is an ordinary race, not a failure
    /// of the request.
    #[must_use]
    pub fn is_object_gone(&self) -> bool {
        self.kind() == ErrorKind::ObjectGone
    }

    /// Report whether retrying the same call unchanged is safe and may succeed.
    ///
    /// `true` means this error proves that the requested mutation did not run,
    /// or came from an operation that only reads state. The condition may need
    /// to clear first: capacity can become available, a client can resume, a
    /// resource-limited spawn can be retried, or a server can answer again.
    ///
    /// `false` does not mean that the handle is unusable. A subprocess timeout,
    /// output-reader failure, child-wait failure, or lost supervisor can leave
    /// the executor ready for another call, but tmux may already have carried
    /// out the first one. Replaying a mutation could duplicate its effect. A
    /// shut down executor and a closed or timed-out control connection are also
    /// false because they require a new server or connection.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Overloaded { .. } | Self::ClientSuspended { .. } => true,
            Self::Spawn { source, .. } => matches!(
                source.kind(),
                io::ErrorKind::Interrupted
                    | io::ErrorKind::WouldBlock
                    | io::ErrorKind::OutOfMemory
                    | io::ErrorKind::ResourceBusy
                    | io::ErrorKind::ExecutableFileBusy
            ),
            Self::ServerGone {
                kind: ServerGoneKind::NotRunning | ServerGoneKind::Unreachable,
                ..
            } => true,
            Self::AfterEffect { .. }
            | Self::InvalidServerConfiguration { .. }
            | Self::InvalidVersionOutput { .. }
            | Self::UnsupportedTmuxVersion { .. }
            | Self::OptionRejected { .. }
            | Self::UnreadableFormatValue { .. }
            | Self::ServerGenerationChanged { .. }
            | Self::OutputLimitExceeded { .. }
            | Self::SessionExists { .. }
            | Self::UnsupportedCapability { .. }
            | Self::CapabilityDefective { .. }
            | Self::VersionProbeFailed { .. }
            | Self::InvalidCommandInput { .. }
            | Self::ExecutableNotFound { .. }
            | Self::ExecutorShutdown { .. }
            | Self::DuplicateRequest { .. }
            | Self::ReadOutput { .. }
            | Self::WaitChild { .. }
            | Self::Timeout { .. }
            | Self::SupervisorLost { .. }
            | Self::ObjectGone { .. }
            | Self::LinkGone { .. }
            | Self::RuntimeUnavailable { .. }
            | Self::RuntimeNested
            | Self::ServerGone {
                kind: ServerGoneKind::Lost | ServerGoneKind::Stopped,
                ..
            }
            | Self::CommandFailed { .. }
            | Self::DecodeListing { .. } => false,
            #[cfg(feature = "plan")]
            Self::InvalidPlan { .. } => false,
            #[cfg(feature = "control-mode")]
            Self::ControlModeFrameTooLarge { .. } | Self::ControlMode { .. } => false,
        }
    }

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

    /// The connection did not attach or answer a command in time.
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
mod tests {
    use std::error::Error as StdError;
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;

    use super::{Error, ErrorKind, SENSITIVE_OUTPUT_WITHHELD, ServerGoneKind};
    use crate::Command;
    use crate::command::{CommandResult, ProcessStatus, RequestId};

    /// The three server-gone wordings a live fixture cannot produce on demand.
    ///
    /// Only "no server running" is reachable from a test, because the other
    /// three need the server to die between the client connecting and the
    /// command finishing. They are read from tmux's `client.c`, so they are
    /// asserted against the classifier rather than against tmux.
    #[test]
    fn a_server_that_is_not_there_is_not_a_refusal() {
        for (stderr, expected, retryable) in [
            (
                "no server running on /tmp/libtmux-rs-dev/absent",
                ServerGoneKind::NotRunning,
                true,
            ),
            (
                "error connecting to /tmp/libtmux-rs-dev/absent (Connection refused)",
                ServerGoneKind::Unreachable,
                true,
            ),
            ("server exited unexpectedly", ServerGoneKind::Lost, false),
            ("server exited", ServerGoneKind::Stopped, false),
        ] {
            let error = Error::refused("list-sessions", Some(1), stderr.to_owned(), None);
            assert_eq!(error.kind(), ErrorKind::ServerGone, "{stderr}");
            assert!(
                matches!(&error, Error::ServerGone { kind, .. } if *kind == expected),
                "{stderr} should be {expected:?}, got {error:?}",
            );
            assert!(!error.is_object_gone(), "{stderr}");
            assert_eq!(
                error.is_transient(),
                retryable,
                "only a failure before connecting proves replay safe: {stderr}",
            );
        }
    }

    #[test]
    fn only_resource_limited_spawn_failures_invite_unchanged_replay() {
        let spawn = |kind| {
            Error::spawn(
                1,
                Command::new("display-message").summary(),
                std::io::Error::from(kind),
                false,
            )
        };

        for kind in [
            std::io::ErrorKind::Interrupted,
            std::io::ErrorKind::WouldBlock,
            std::io::ErrorKind::OutOfMemory,
            std::io::ErrorKind::ResourceBusy,
            std::io::ErrorKind::ExecutableFileBusy,
        ] {
            assert!(spawn(kind).is_transient(), "{kind:?} may clear on retry");
        }
        for kind in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::InvalidData,
        ] {
            assert!(
                !spawn(kind).is_transient(),
                "{kind:?} needs repair, not replay",
            );
        }
    }

    #[cfg(feature = "control-mode")]
    #[test]
    fn control_mode_debug_distinguishes_the_source_kind() {
        let error = Error::control_mode(std::io::Error::from(std::io::ErrorKind::PermissionDenied));

        assert_eq!(
            format!("{error:?}"),
            "ControlMode { kind: Transport, source_kind: Some(PermissionDenied) }",
        );
    }

    #[test]
    fn an_error_after_an_effect_cannot_invite_replay_or_be_relabelled() {
        let error = Error::Overloaded {
            request_id: 7,
            command: Command::new("list-panes").summary(),
            in_flight: 1,
        }
        .after_effect("resize-pane");

        assert_eq!(error.kind(), ErrorKind::PartialEffect);
        assert!(!error.is_object_gone());
        assert!(!error.is_transient());

        let error = error.after_effect("select-pane");
        assert!(
            matches!(
                &error,
                Error::AfterEffect { operation: "resize-pane", source }
                    if source.kind() == ErrorKind::Refused && source.is_transient()
            ),
            "the existing, more specific effect is the useful replay boundary: {error:?}",
        );
    }

    #[test]
    fn an_effect_boundary_preserves_a_redacted_source_without_growing() {
        let secret = "sentinel-after-effect-secret";
        let command = Command::new("send-keys").sensitive_arg(secret);
        let result = CommandResult::new(
            RequestId::new(9),
            command.summary(),
            ProcessStatus::from_exit_status(ExitStatus::from_raw(1 << 8)),
            Vec::new(),
            format!("bad key: {secret}\n").into_bytes(),
        );
        let error = result
            .refusal_for("send-keys")
            .expect("the sensitive command was refused")
            .after_effect("send-keys")
            .after_effect("plan");

        let source = StdError::source(&error).expect("the boundary exposes its source");
        let source = source
            .downcast_ref::<Box<Error>>()
            .expect("the source owns a libtmux::Error")
            .as_ref();
        assert_eq!(source.kind(), ErrorKind::Refused);
        assert!(source.source().is_none(), "the source chain has one edge");
        assert!(matches!(
            &error,
            Error::AfterEffect {
                operation: "send-keys",
                ..
            }
        ));

        for diagnostic in [
            error.to_string(),
            format!("{error:?}"),
            source.to_string(),
            format!("{source:?}"),
        ] {
            assert!(!diagnostic.contains(secret), "{diagnostic}");
        }
    }

    /// The order the two server-exit wordings are read in is load-bearing.
    ///
    /// A lost server says `server exited unexpectedly`, which starts with the
    /// `server exited` of one that shut down and does not mean it.
    #[test]
    fn a_lost_server_is_not_read_as_one_that_stopped() {
        let error = Error::refused(
            "new-session",
            Some(1),
            "server exited unexpectedly".to_owned(),
            None,
        );
        assert!(
            matches!(&error, Error::ServerGone { kind, .. } if *kind == ServerGoneKind::Lost),
            "{error:?}",
        );
    }

    /// A refusal that says nothing about the server stays a refusal, so the
    /// classification is not simply calling everything gone.
    #[test]
    fn a_refusal_that_names_no_server_stays_a_refusal() {
        let error = Error::refused(
            "delete-buffer",
            Some(1),
            "no buffer never-existed".to_owned(),
            None,
        );
        assert_eq!(error.kind(), ErrorKind::Refused, "{error:?}");
    }

    #[test]
    fn withheld_refusal_uses_the_payload_appropriate_variant() {
        let error = Error::refused_withheld("set-option", Some(1));

        assert!(matches!(&error, Error::CommandFailed { .. }), "{error:?}");
        assert_eq!(error.kind(), ErrorKind::Refused);
    }

    #[test]
    fn a_sensitive_mismatched_target_echo_stays_withheld() {
        let secret = "sentinel-sensitive-echo";
        let target = std::ffi::OsStr::new("%9");
        let command = Command::new("send-keys")
            .arg("-t")
            .arg(target)
            .sensitive_arg("sentinel-sensitive-input");
        let result = CommandResult::new(
            RequestId::new(1),
            command.summary(),
            ProcessStatus::from_exit_status(ExitStatus::from_raw(1 << 8)),
            Vec::new(),
            format!("can't find pane: %9 {secret}\n").into_bytes(),
        );

        let error = Error::from_refused_result("send-keys", &result, Some(target));

        assert!(
            matches!(
                &error,
                Error::CommandFailed { stderr, .. } if stderr == SENSITIVE_OUTPUT_WITHHELD
            ),
            "{error:?}",
        );
        let diagnostic = format!("{error:?} {error}");
        for sensitive in [secret, "sentinel-sensitive-input"] {
            assert!(!diagnostic.contains(sensitive), "{diagnostic}");
        }
    }

    /// What tmux echoes back decides whether an object died or a place is
    /// empty, and only one of those lets a caller drop a handle.
    ///
    /// A window or pane coordinate -- an index, or a window name -- is scoped
    /// to one session and is not unique on the server, so its absence cannot
    /// mean the object is gone. Reporting one as an identity answered
    /// `is_object_gone` with `true` for a window that was alive and merely
    /// renumbered, which is the one predicate a caller consults before
    /// discarding a handle.
    ///
    /// A session is the exception: `-t` takes a session's name, so a bare word
    /// there is still an identity. Wording measured on tmux 3.2a to 3.7b.
    #[test]
    fn a_missing_coordinate_is_not_a_missing_object() {
        for (stderr, gone) in [
            // A sigil means tmux resolved a name that belongs to one object.
            ("can't find window: @99", true),
            ("can't find pane: %99", true),
            ("can't find session: $99", true),
            // A name is how tmux lets a caller target a session.
            ("can't find session: nosuchsession", true),
            // Coordinates: a place within one session, not an object.
            ("can't find window: 9", false),
            ("can't find window: nosuchname", false),
            ("can't find pane: 9", false),
        ] {
            let error = Error::refused(
                "unlink-window",
                Some(1),
                stderr.to_owned(),
                Some(std::ffi::OsStr::new("home:9")),
            );
            assert_eq!(
                error.is_object_gone(),
                gone,
                "{stderr} should report gone={gone}, got {error:?}",
            );
        }
    }

    /// tmux drops the session half of a coordinate, so the request keeps it.
    ///
    /// `-t home:9` is answered by `can't find window: 9`. Reporting the echo
    /// alone would leave a reader holding half a target they cannot act on.
    #[test]
    fn a_missing_coordinate_reports_the_target_that_was_sent() {
        let error = Error::refused(
            "unlink-window",
            Some(1),
            "can't find window: 9".to_owned(),
            Some(std::ffi::OsStr::new("home:9")),
        );
        assert!(
            matches!(&error, Error::LinkGone { target, .. } if target == "home:9"),
            "{error:?}",
        );
        assert_eq!(error.to_string(), "tmux has no window at home:9");
    }
}

#[cfg(test)]
mod compat_tests {

    /// Pin the tmux wording that says how an option was refused.
    ///
    /// The three answers need three different fixes, and tmux distinguishes
    /// them only in stderr: every one of these exits 1. It also spells a
    /// rejected value two ways, "bad value" for a flag and "value is invalid"
    /// for a number, which is why the kind exists rather than the text.
    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn real_tmux_compat_error_option_refusal_wording_is_recognized() {
        use crate::test::TestServer;
        use crate::{Error, ErrorKind, OptionErrorKind};

        let guard = TestServer::builder().start().await.expect("tmux starts");
        let server = guard.server();

        for (name, value, expected) in [
            ("no-such-option", "x", OptionErrorKind::Unknown),
            // A prefix of `status-left`, `status-left-length`, and
            // `status-left-style` on every supported release, so tmux will not
            // choose. A release that left only one of them would turn this
            // answer into a different kind, which is the point of pinning it.
            ("status-l", "x", OptionErrorKind::Ambiguous),
            ("mouse", "notabool", OptionErrorKind::BadValue),
            (
                "status-left-length",
                "notanumber",
                OptionErrorKind::BadValue,
            ),
        ] {
            let error = server
                .set_global_option(name, value)
                .await
                .expect_err("tmux refuses it");
            assert!(
                matches!(&error, Error::OptionRejected { kind, .. } if *kind == expected),
                "{name}={value} should be {expected:?}, got {error:?}",
            );
            assert_eq!(error.kind(), ErrorKind::Refused);
            assert!(!error.is_object_gone(), "a refusal is not a missing object");
        }

        guard.shutdown().await.expect("tmux fixture shuts down");
    }

    /// Pin the tmux wording that separates a missing target from a refusal.
    ///
    /// `Error::refused` reads tmux's stderr because tmux exits 1 for both, so
    /// this asserts against the tmux the lane is running rather than against
    /// the source this was written from. Every compatibility lane runs it, so
    /// a release that rewords these is a failure here rather than a silently
    /// wrong `is_object_gone` in the field.
    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn real_tmux_compat_error_missing_target_wording_is_recognized() {
        use crate::ErrorKind;
        use crate::test::TestServer;

        let guard = TestServer::builder().start().await.expect("tmux starts");
        let server = guard.server();
        let session = server.new_session("compat-missing").await.expect("session");

        // One live session, so tmux can resolve a current target and reports
        // the specific object it could not find.
        for (label, error) in [
            (
                "window",
                server
                    .window_by_id(&"@4242".parse().expect("a window id"))
                    .await
                    .map(|found| assert!(found.is_none(), "the window does not exist"))
                    .err(),
            ),
            (
                "pane",
                server
                    .pane_by_id(&"%4242".parse().expect("a pane id"))
                    .await
                    .map(|found| assert!(found.is_none(), "the pane does not exist"))
                    .err(),
            ),
        ] {
            assert!(error.is_none(), "a lookup reports absence, not {label}");
        }

        // A mutation against a target tmux does not have is where the wording
        // matters: it is the only signal separating this from a bad argument.
        let mut window = session.windows().await.expect("windows").remove(0);
        let doomed = session
            .new_window(crate::NewWindowOptions::new("doomed").command("sleep 300"))
            .await
            .expect("window");
        let mut stale = doomed.clone();
        doomed.kill().await.expect("the window is killed");

        let error = stale.rename("gone").await.expect_err("the window is gone");
        assert_eq!(
            error.kind(),
            ErrorKind::ObjectGone,
            "tmux 'can't find window' is recognized: {error}",
        );

        // And a refusal that is not a missing target stays a refusal, so the
        // classification is not simply calling everything gone.
        let refused = server
            .delete_buffer("never-existed")
            .await
            .expect_err("tmux has no such buffer");
        assert_eq!(refused.kind(), ErrorKind::Refused, "{refused}");

        // With no session left, tmux cannot resolve a current target and says
        // so instead, for the same request. Both wordings mean gone.
        window
            .rename("last")
            .await
            .expect("the window still exists");
        session.kill().await.expect("the session is killed");

        let error = stale
            .rename("still gone")
            .await
            .expect_err("the window is gone");
        assert_eq!(
            error.kind(),
            ErrorKind::ObjectGone,
            "tmux 'no current target' is recognized: {error}",
        );

        guard.shutdown().await.expect("tmux fixture shuts down");
    }

    /// Pin the tmux wording that says the server, not the request, is the
    /// problem.
    ///
    /// tmux exits 1 for a command it refused and for a command that found no
    /// server, and separates them only in stderr. Reading the second as the
    /// first tells a caller to fix arguments that were never the trouble.
    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn real_tmux_compat_error_absent_server_wording_is_recognized() {
        use std::time::Duration;

        use crate::test::{TestServer, retry_until};
        use crate::{Command, ErrorKind, ServerGoneKind};

        let mut guard = TestServer::builder().start().await.expect("tmux starts");
        guard.session("compat-gone").await.expect("session");

        guard
            .server()
            .cmd(Command::new("kill-server"))
            .await
            .expect("the server is killed");

        // tmux stops answering on the socket before the kernel has a status
        // for the process behind it, so this waits for the daemon rather than
        // for a duration.
        retry_until(Duration::from_secs(5), async || {
            !guard.daemon_state().is_running()
        })
        .await
        .expect("the daemon exits");

        let error = guard
            .server()
            .sessions()
            .await
            .expect_err("there is no server to list");
        assert_eq!(
            error.kind(),
            ErrorKind::ServerGone,
            "tmux 'no server running' is recognized: {error}",
        );
        assert!(
            matches!(&error, crate::Error::ServerGone { kind, .. } if *kind == ServerGoneKind::NotRunning),
            "the absence is named: {error:?}",
        );
        assert!(
            !error.is_object_gone(),
            "an absent server is not a missing object: {error}",
        );

        guard.shutdown().await.expect("tmux fixture shuts down");
    }
    /// Pin the half of tmux's answer that says how much a miss proves.
    ///
    /// tmux echoes back the part of a target it could not resolve, and drops
    /// the session from it when that part is a coordinate. `Error::refused`
    /// reads the sigil on what comes back to tell a place from an object, so a
    /// release that echoed the whole target, or that stripped the sigil, would
    /// change what `is_object_gone` answers -- and that is the predicate a
    /// caller consults before discarding a handle. Asserted against whichever
    /// tmux the lane is running.
    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn real_tmux_compat_a_coordinate_miss_is_not_an_object_miss() {
        use crate::test::TestServer;

        let guard = TestServer::builder().start().await.expect("tmux starts");
        let server = guard.server();
        let session = server.new_session("compat-echo").await.expect("session");

        for (target, gone) in [
            // A place in a session that holds nothing there.
            (format!("{}:9", session.id()), false),
            // A window id no server ever issued.
            (format!("{}:@4242", session.id()), true),
        ] {
            let result = server
                .cmd(
                    crate::Command::new("unlink-window")
                        .arg("-t")
                        .arg(target.clone()),
                )
                .await
                .expect("the command runs");
            assert!(!result.success(), "{target} resolves to nothing");

            let stderr = result.stderr_lossy().into_owned();
            let error = crate::Error::refused(
                "unlink-window",
                result.exit_code(),
                stderr.clone(),
                Some(std::ffi::OsStr::new(&target)),
            );
            assert_eq!(
                error.is_object_gone(),
                gone,
                "{target} answered {stderr:?}, classified {error:?}",
            );
        }

        guard.shutdown().await.expect("tmux fixture shuts down");
    }
}
