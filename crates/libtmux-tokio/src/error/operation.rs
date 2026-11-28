use libtmux_core::{CreatedSession, Effect};

use super::{Error, ErrorKind};

impl Error {
    /// Preserve the acknowledged IDs when a later step fails.
    #[must_use]
    pub fn after_creation(self, receipt: CreatedSession) -> Self {
        Self::OperationFailure {
            effect: Effect::Created,
            receipt: Some(Box::new(receipt)),
            source: Box::new(self),
        }
    }

    /// Borrow a creation receipt verified before a later failure.
    #[must_use]
    pub fn creation_receipt(&self) -> Option<&CreatedSession> {
        match self {
            Self::CoreOperation { source } => core_receipt(source),
            Self::OperationFailure { receipt, .. } => receipt.as_deref(),
            Self::OperationAndCleanup { primary, .. } => primary.creation_receipt(),
            Self::OperationCleanupPending { source } | Self::AfterEffect { source, .. } => {
                source.creation_receipt()
            }
            _ => None,
        }
    }

    /// Return the effect evidence for a shared typed operation failure.
    ///
    /// `None` means the error comes from the legacy object API and carries no
    /// shared-operation receipt contract. Unknown or acknowledged mutations
    /// must not be replayed automatically.
    #[must_use]
    pub fn operation_effect(&self) -> Option<Effect> {
        match self {
            Self::CoreOperation { source } => Some(source.effect()),
            Self::OperationFailure { effect, .. } => Some(*effect),
            Self::OperationAndCleanup { primary, .. } => primary.operation_effect(),
            Self::OperationCleanupPending { source } | Self::AfterEffect { source, .. } => {
                source.operation_effect()
            }
            _ => None,
        }
    }
}

impl From<libtmux_core::Error> for Error {
    fn from(source: libtmux_core::Error) -> Self {
        Self::CoreOperation {
            source: Box::new(source),
        }
    }
}

fn core_receipt(error: &libtmux_core::Error) -> Option<&CreatedSession> {
    use libtmux_core::Error as CoreError;
    match error {
        CoreError::CreationInterrupted { receipt, .. } => Some(receipt.as_ref()),
        CoreError::LateCreation { receipt } | CoreError::CreationFollowup { receipt, .. } => {
            receipt.as_deref()
        }
        CoreError::Cleanup { primary, .. } | CoreError::CleanupPending(primary) => {
            core_receipt(primary)
        }
        _ => None,
    }
}

pub(super) fn core_version(error: &libtmux_core::Error) -> Option<&libtmux_core::VersionError> {
    use libtmux_core::Error as CoreError;
    match error {
        CoreError::Version(version) => Some(version),
        CoreError::Cleanup { primary, .. } | CoreError::CleanupPending(primary) => {
            core_version(primary)
        }
        CoreError::AdmissionFailed { source } => core_version(source),
        CoreError::ObservationFailed { source, .. }
        | CoreError::CreationInterrupted { source, .. } => core_version(source),
        _ => None,
    }
}

pub(super) fn core_kind(error: &libtmux_core::Error) -> ErrorKind {
    use libtmux_core::Error as CoreError;
    if error.effect() == Effect::Created {
        return ErrorKind::PartialEffect;
    }
    match error {
        CoreError::Version(version) => match version.as_ref() {
            libtmux_core::VersionError::InvalidVersionOutput { .. } => ErrorKind::Decode,
            _ => ErrorKind::UnsupportedVersion,
        },
        CoreError::Busy { .. } | CoreError::Command { .. } => ErrorKind::Refused,
        CoreError::BeforeSpawn | CoreError::Deadline | CoreError::LateObservation { .. } => {
            ErrorKind::Timeout
        }
        CoreError::UnsupportedVersion => ErrorKind::UnsupportedVersion,
        CoreError::Spawn(_) => ErrorKind::Unreachable,
        CoreError::InvalidKey | CoreError::FramedRead | CoreError::UnconfirmedCreation { .. } => {
            ErrorKind::Decode
        }
        CoreError::WrongEndpoint => ErrorKind::InvalidInput,
        CoreError::PaneNotFound(_) | CoreError::StalePane => ErrorKind::ObjectGone,
        CoreError::ObservationFailed { source, .. } => core_kind(source),
        CoreError::AdmissionFailed { source } => core_kind(source),
        CoreError::Cleanup { primary, .. } | CoreError::CleanupPending(primary) => {
            core_kind(primary)
        }
        _ => ErrorKind::Transport,
    }
}
