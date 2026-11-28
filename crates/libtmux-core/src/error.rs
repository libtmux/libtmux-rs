use std::fmt;

use crate::{CreatedSession, PaneId};

/// The tmux operation identified by a failed native-driver reply.
///
/// # Examples
///
/// ```
/// use libtmux_core::CommandKind;
///
/// assert_eq!(CommandKind::NewSession.to_string(), "new-session");
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandKind {
    /// Create a detached session and its first window and pane.
    NewSession,
    /// Observe the current daemon PID and start time.
    ServerGeneration,
    /// List observed sessions.
    ListSessions,
    /// List observed windows across all sessions.
    ListWindows,
    /// List attached clients.
    ListClients,
    /// Observe an exact pane's generation and identity.
    ListPanes,
    /// Capture bytes from a generation-checked pane.
    CapturePane,
    /// Kill the session bound to a creation receipt.
    KillSession,
    /// Remove one observed session placement of a window.
    UnlinkWindow,
}

impl fmt::Display for CommandKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NewSession => "new-session",
            Self::ServerGeneration => "server generation",
            Self::ListSessions => "list-sessions",
            Self::ListWindows => "list-windows",
            Self::ListClients => "list-clients",
            Self::ListPanes => "list-panes",
            Self::CapturePane => "capture-pane",
            Self::KillSession => "kill-session",
            Self::UnlinkWindow => "unlink-window",
        })
    }
}

/// Evidence about the requested operation before a caller considers replay.
///
/// Owner-configured tmux hooks can have additional effects. `Created` requires
/// a verified identity receipt, even when a later step failed. A submitted
/// creation without that receipt remains `Unknown`.
///
/// # Examples
///
/// ```
/// use libtmux_core::{Effect, Error};
///
/// assert_eq!(Error::BeforeSpawn.effect(), Effect::NotSubmitted);
/// assert_eq!(Error::Deadline.effect(), Effect::Unknown);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    /// The request was not submitted, so retrying cannot repeat it.
    NotSubmitted,
    /// The requested operation only observed state.
    Observation,
    /// A verified identity receipt acknowledges creation; replay may duplicate it.
    Created,
    /// A submitted mutation may or may not have taken effect.
    Unknown,
}

impl Effect {
    /// Stable spelling for machine-readable error reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotSubmitted => "not_submitted",
            Self::Observation => "observation",
            Self::Created => "created",
            Self::Unknown => "unknown",
        }
    }
}

/// A rejected request or a failure with classified evidence about its effects.
///
/// Receipt and diagnostic payload bytes are never included in decoding errors.
/// A typed creation receipt can survive a deadline or loss of later output.
///
/// # Examples
///
/// ```
/// use libtmux_core::{Effect, Error};
///
/// let failure = Error::CancelledBeforeSpawn;
/// assert_eq!(failure.effect(), Effect::NotSubmitted);
/// ```
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// All subprocess slots are occupied; the request was not submitted.
    #[error("all {limit} subprocess slots are in use")]
    Busy {
        /// The configured subprocess limit.
        limit: usize,
    },
    /// A snapshot listing returned an invalid row. The error carries only coordinates.
    #[error(transparent)]
    SnapshotRow(#[from] crate::formats::FormatCodecError),
    /// The server handle has stopped accepting new requests.
    #[error("server is closed to new requests")]
    Closed,
    /// Child cleanup failed while shutting down.
    #[error("child cleanup failed during shutdown: {0}")]
    Shutdown(String),
    /// The request deadline elapsed before a process could start.
    #[error("deadline elapsed before spawn")]
    BeforeSpawn,
    /// The caller cancelled before a process could start.
    #[error("caller cancelled before spawn")]
    CancelledBeforeSpawn,
    /// The version policy refused one-shot control framing before dispatch.
    #[error("this tmux version is not qualified for one-shot control framing")]
    UnsupportedVersion,
    /// A version report or feature check refused dispatch with its exact cause.
    #[error(transparent)]
    Version(Box<crate::VersionError>),
    /// A qualification probe failed before the requested operation was submitted.
    #[error(
        "tmux version probe failed (status {status:?}, {diagnostic_len} diagnostic bytes, late={late})"
    )]
    VersionProbeFailed {
        /// The probe client exit status, if one was observed.
        status: Option<i32>,
        /// The diagnostic byte count, without its contents.
        diagnostic_len: usize,
        /// Whether the probe completed after the request deadline.
        late: bool,
    },
    /// Preflight failed before the requested operation was submitted.
    #[error("tmux request admission failed before submission: {source}")]
    AdmissionFailed {
        /// The original preflight failure, including cleanup evidence.
        #[source]
        source: Box<Error>,
    },
    /// The deadline elapsed after dispatch; a mutation may have run.
    #[error("deadline elapsed; a dispatched tmux command may have run")]
    Deadline,
    /// The creation reply arrived after the request deadline.
    #[error("{}", if receipt.is_some() {
        "tmux acknowledged session creation after the deadline"
    } else {
        "the late creation reply had no verified receipt; creation may have run; inspect the server before repeating"
    })]
    LateCreation {
        /// Object identities, when the receipt was readable.
        receipt: Option<Box<CreatedSession>>,
    },
    /// A creation frame preceded a failed hook, client exit or trailing frame.
    #[error("{} ({}; late={late})", if receipt.is_some() {
        "session created; follow-up failed or was incomplete"
    } else {
        "creation was not acknowledged; it may have run; inspect the server before repeating"
    }, creation_status(*status))]
    CreationFollowup {
        /// Object identities, when the receipt was readable.
        receipt: Option<Box<CreatedSession>>,
        /// The client exit status, if one was observed.
        status: Option<i32>,
        /// Whether completion was observed after the deadline.
        late: bool,
    },
    /// A complete creation receipt arrived before the client was interrupted.
    #[error("tmux acknowledged session creation before the client was interrupted: {source}")]
    CreationInterrupted {
        /// Identities from the unique, complete initial action frame.
        receipt: Box<CreatedSession>,
        /// The original failure, including any cleanup failure.
        #[source]
        source: Box<Error>,
    },
    /// No complete, unambiguous initial action frame confirmed creation.
    #[error("session creation was not acknowledged ({}; late={late}); creation may have run; inspect the server before repeating", creation_status(*status))]
    UnconfirmedCreation {
        /// The client exit status, if one was observed.
        status: Option<i32>,
        /// Whether completion was observed after the deadline.
        late: bool,
    },
    /// An observation lost its reply after dispatch.
    #[error("{operation} failed: {source}")]
    ObservationFailed {
        /// The observation being attempted.
        operation: CommandKind,
        /// The original failure and its cleanup evidence.
        #[source]
        source: Box<Error>,
    },
    /// An observation completed after its deadline; its bytes are discarded.
    #[error("{operation} completed after the deadline")]
    LateObservation {
        /// The observation being attempted.
        operation: CommandKind,
    },
    /// A collected stream exceeded its byte budget after dispatch.
    #[error("{stream} exceeded {limit} bytes after dispatch")]
    OutputLimit {
        /// The stream name, without payload bytes.
        stream: &'static str,
        /// The per-stream byte limit.
        limit: usize,
    },
    /// The caller cancelled after dispatch.
    #[error("caller cancelled after dispatch")]
    Cancelled,
    /// Cleanup failed after an earlier request failure.
    #[error("{primary}; child cleanup failed: {errors}")]
    Cleanup {
        /// The request failure and any typed creation evidence.
        primary: Box<Error>,
        /// The failures of the cleanup system calls.
        errors: crate::supervision::CleanupErrors,
    },
    /// Cleanup has not been confirmed after an earlier request failure.
    #[error("{0}; child cleanup has not been confirmed")]
    CleanupPending(Box<Error>),
    /// The OS refused to start the tmux client.
    #[error("could not start the tmux client")]
    Spawn(#[source] std::io::Error),
    /// The adapter could not confirm whether the OS started the client.
    #[error("could not confirm tmux client startup")]
    UncertainStart(#[source] std::io::Error),
    /// The client reply was lost after dispatch.
    #[error("lost the tmux client reply after dispatch")]
    LostReply(#[source] std::io::Error),
    /// The client refused or failed an operation.
    #[error(
        "{operation} exited with status {status:?} ({diagnostic_len} diagnostic bytes, late={late})"
    )]
    Command {
        /// The operation being attempted.
        operation: CommandKind,
        /// The client exit status, if one was observed.
        status: Option<i32>,
        /// The diagnostic byte count, without diagnostic contents.
        diagnostic_len: usize,
        /// Whether completion was observed after the deadline.
        late: bool,
    },
    /// The creation reply had no readable identity receipt.
    #[error(
        "the creation reply had no verified receipt; creation may have run; inspect the server before repeating"
    )]
    AfterCreate,
    /// An observation returned an unreadable or inconsistent pane key.
    #[error("tmux returned an unreadable pane key")]
    InvalidKey,
    /// An observation returned incomplete or ambiguous control framing.
    #[error("tmux returned an ambiguous or incomplete read frame")]
    FramedRead,
    /// The requested pane was absent from an exact listing.
    #[error("tmux did not list pane {0}")]
    PaneNotFound(PaneId),
    /// A pane key belongs to a different captured socket path.
    #[error("pane belongs to a different socket")]
    WrongEndpoint,
    /// The generation or pane identity changed since discovery.
    #[error("pane identity changed since discovery")]
    StalePane,
}

fn creation_status(status: Option<i32>) -> String {
    status.map_or_else(
        || "client exit status unavailable".to_owned(),
        |status| format!("client exited with status {status}"),
    )
}

impl Error {
    /// Borrow a creation receipt verified before a later failure.
    ///
    /// Wrapping an error for admission, observation or child cleanup retains
    /// its receipt. An unreadable identity receipt leaves the effect unknown.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::{Error, NewSession};
    /// use std::path::Path;
    ///
    /// let receipt = NewSession::new("work")
    ///     .decode(Path::new("/example/socket"), b"123 456 %7 $8 @9\n")?;
    /// let error = Error::CleanupPending(Box::new(Error::LateCreation {
    ///     receipt: Some(Box::new(receipt.clone())),
    /// }));
    /// assert_eq!(error.creation_receipt(), Some(&receipt));
    /// assert!(Error::AfterCreate.creation_receipt().is_none());
    /// # Ok::<(), libtmux_core::Error>(())
    /// ```
    #[must_use]
    pub fn creation_receipt(&self) -> Option<&CreatedSession> {
        match self {
            Self::CreationInterrupted { receipt, .. } => Some(receipt),
            Self::LateCreation { receipt } | Self::CreationFollowup { receipt, .. } => {
                receipt.as_deref()
            }
            Self::Cleanup { primary, .. } | Self::CleanupPending(primary) => {
                primary.creation_receipt()
            }
            Self::AdmissionFailed { source } | Self::ObservationFailed { source, .. } => {
                source.creation_receipt()
            }
            _ => None,
        }
    }

    /// Classify a read failure while preserving pre-dispatch rejections.
    #[doc(hidden)]
    #[must_use]
    pub fn during_read(self, operation: CommandKind) -> Self {
        if self.effect() == Effect::Unknown {
            Self::ObservationFailed {
                operation,
                source: Box::new(self),
            }
        } else {
            self
        }
    }

    /// The evidence available about the requested operation's effect.
    #[must_use]
    pub fn effect(&self) -> Effect {
        match self {
            Self::Cleanup { primary, .. } | Self::CleanupPending(primary) => primary.effect(),
            Self::Busy { .. }
            | Self::Closed
            | Self::BeforeSpawn
            | Self::CancelledBeforeSpawn
            | Self::UnsupportedVersion
            | Self::Version(_)
            | Self::VersionProbeFailed { .. }
            | Self::AdmissionFailed { .. }
            | Self::Spawn(_)
            | Self::WrongEndpoint => Effect::NotSubmitted,
            Self::CreationInterrupted { .. }
            | Self::CreationFollowup {
                receipt: Some(_), ..
            }
            | Self::LateCreation { receipt: Some(_) } => Effect::Created,
            Self::SnapshotRow(_)
            | Self::LateObservation { .. }
            | Self::ObservationFailed { .. }
            | Self::FramedRead
            | Self::InvalidKey
            | Self::PaneNotFound(_)
            | Self::StalePane
            | Self::Command {
                operation:
                    CommandKind::ListPanes
                    | CommandKind::CapturePane
                    | CommandKind::ListSessions
                    | CommandKind::ListWindows
                    | CommandKind::ListClients,
                ..
            } => Effect::Observation,
            Self::Command { .. }
            | Self::AfterCreate
            | Self::CreationFollowup { receipt: None, .. }
            | Self::LateCreation { receipt: None }
            | Self::UncertainStart(_)
            | Self::UnconfirmedCreation { .. }
            | Self::Shutdown(_)
            | Self::Deadline
            | Self::OutputLimit { .. }
            | Self::Cancelled
            | Self::LostReply(_) => Effect::Unknown,
        }
    }
}

impl From<crate::VersionError> for Error {
    fn from(error: crate::VersionError) -> Self {
        Self::Version(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn missing_receipts_never_describe_creation_as_acknowledged() {
        for status in [None, Some(0), Some(1)] {
            for error in [
                Error::UnconfirmedCreation {
                    status,
                    late: false,
                },
                Error::CreationFollowup {
                    receipt: None,
                    status,
                    late: false,
                },
                Error::LateCreation { receipt: None },
                Error::AfterCreate,
            ] {
                assert_eq!(error.effect(), Effect::Unknown);
                let message = error.to_string();
                assert!(message.contains("may have run"), "{message}");
                assert!(!message.contains("session created"), "{message}");
                assert!(
                    !message.contains("acknowledged session creation"),
                    "{message}"
                );
                assert!(!message.contains("Some("), "{message}");
            }
        }
    }

    #[test]
    fn verified_creation_receipts_survive_compound_errors() {
        let receipt = crate::NewSession::new("work")
            .decode(Path::new("/example/socket"), b"123 456 %7 $8 @9\n")
            .unwrap();
        for error in [
            Error::LateCreation {
                receipt: Some(Box::new(receipt.clone())),
            },
            Error::CreationFollowup {
                receipt: Some(Box::new(receipt.clone())),
                status: Some(1),
                late: false,
            },
            Error::CreationInterrupted {
                receipt: Box::new(receipt.clone()),
                source: Box::new(Error::Cancelled),
            },
        ] {
            assert_eq!(error.creation_receipt(), Some(&receipt));
            let error = Error::Cleanup {
                primary: Box::new(Error::CleanupPending(Box::new(Error::ObservationFailed {
                    operation: CommandKind::CapturePane,
                    source: Box::new(Error::AdmissionFailed {
                        source: Box::new(error),
                    }),
                }))),
                errors: crate::supervision::CleanupErrors {
                    kill: Some(std::io::Error::other("signal failed")),
                    reap: Some(std::io::Error::other("wait failed")),
                },
            };
            assert_eq!(error.creation_receipt(), Some(&receipt));
        }
    }

    #[test]
    fn an_unidentified_creation_never_supplies_a_cleanup_receipt() {
        for error in [
            Error::AfterCreate,
            Error::LateCreation { receipt: None },
            Error::CreationFollowup {
                receipt: None,
                status: Some(1),
                late: false,
            },
            Error::UnconfirmedCreation {
                status: Some(1),
                late: false,
            },
        ] {
            assert!(error.creation_receipt().is_none());
            assert!(
                Error::CleanupPending(Box::new(error))
                    .creation_receipt()
                    .is_none()
            );
        }
    }
}
