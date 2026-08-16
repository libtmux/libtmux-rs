//! Errors returned by libtmux.

use std::fmt;
use std::io;
use std::time::Duration;

use crate::CommandSummary;
use crate::version::{ReleaseVersion, TmuxVersion};

/// The category of an invalid [`crate::ServerBuilder`] configuration.
///
/// Rejected path and environment bytes are never retained by this value.
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
#[cfg(feature = "control-mode")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ControlModeErrorKind {
    /// The tmux client could not be started, or its pipes failed.
    Transport,
    /// tmux started without giving the crate the pipes it asked for.
    ///
    /// Nothing a caller does causes this; it means the process could not be
    /// set up as requested.
    MissingPipes,
    /// The connection closed before the command was answered.
    Closed,
    /// The command contains an argument no control-mode line can carry.
    ///
    /// Control mode is a text protocol, so an argument that is not UTF-8
    /// cannot be sent over it even though the same command would run fine as
    /// a subprocess.
    UnrepresentableCommand,
}

/// What tmux says when it holds no session to resolve a target against.
pub(crate) const NO_CURRENT_TARGET: &str = "no current target";

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

/// What tmux says when it has no client to act on.
pub(crate) const NO_CURRENT_CLIENT: &str = "no current client";

/// What a failure means for the caller.
///
/// [`Error`] carries the detail; this carries the decision. Each variant is a
/// different thing to do about it, which is why there are fewer of these than
/// there are error variants.
///
/// New kinds may be added, so match with a `_` arm.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The object is not on the server. Look it up again, or create it.
    ObjectGone,
    /// tmux ran the command and refused it. The arguments were wrong.
    Refused,
    /// The command did not finish in time. Retry, or allow longer.
    Timeout,
    /// tmux could not be run at all: not installed, or not where the server
    /// was told to look. Nothing about the request will change this.
    Unreachable,
    /// The tmux that answered is older than this crate supports.
    UnsupportedVersion,
    /// The caller passed something that cannot be sent to tmux.
    InvalidInput,
    /// The process or connection carrying the command failed. Usually the
    /// environment rather than the request, so retrying may work.
    Transport,
    /// tmux answered in a shape the crate could not read. Worth reporting.
    Decode,
}

/// An invalid scope-specific tmux object ID.
///
/// The error records the expected sigil but never retains the rejected input.
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
#[derive(thiserror::Error)]
#[non_exhaustive]
pub enum Error {
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
        /// The name tmux could not resolve, or the value it would not take.
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
    #[non_exhaustive]
    #[error("tmux no longer has {kind} {id}")]
    ObjectGone {
        /// The kind of object that disappeared.
        kind: ObjectKind,
        /// The tmux identity that is no longer present.
        id: String,
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
        /// The message tmux printed, which explains the refusal.
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
        const MISSING: [(&str, ObjectKind); 4] = [
            ("can't find session:", ObjectKind::Session),
            ("can't find window:", ObjectKind::Window),
            ("can't find pane:", ObjectKind::Pane),
            ("can't find client:", ObjectKind::Client),
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
            if let Some(id) = stderr.trim_end().strip_prefix(prefix) {
                return Self::ObjectGone {
                    kind,
                    id: id.trim().to_owned(),
                };
            }
        }

        Self::CommandFailed {
            command,
            exit_code,
            stderr,
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
    /// # async fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
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
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::ObjectGone { .. } => ErrorKind::ObjectGone,
            Self::CommandFailed { .. }
            | Self::SessionExists { .. }
            | Self::OptionRejected { .. }
            | Self::ServerGenerationChanged { .. } => ErrorKind::Refused,
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
            Self::ControlMode { kind, .. } => match kind {
                ControlModeErrorKind::UnrepresentableCommand => ErrorKind::InvalidInput,
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

    /// Report whether making the same call again could succeed.
    ///
    /// True for a timeout and for a transport failure, which are usually the
    /// machine rather than the request. False for anything tmux answered,
    /// which will be answered the same way again.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self.kind(), ErrorKind::Timeout | ErrorKind::Transport)
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

    /// tmux closed the connection before answering.
    #[cfg(feature = "control-mode")]
    /// A command carries an argument no control-mode line can express.
    #[cfg(feature = "control-mode")]
    pub(crate) const fn control_mode_unrepresentable() -> Self {
        Self::ControlMode {
            kind: ControlModeErrorKind::UnrepresentableCommand,
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
            #[cfg(feature = "control-mode")]
            Self::ControlMode { kind, source } => formatter
                .debug_struct("ControlMode")
                .field("kind", kind)
                .field("kind", &source.as_ref().map(io::Error::kind))
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
        let mut window = session.try_windows().await.expect("windows").remove(0);
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
}
