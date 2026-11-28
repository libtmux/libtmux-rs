use libtmux_core::{CreatedSession, Effect};

use super::{Error, ErrorKind};

impl Error {
    /// Combine an operation result with its completed cleanup result.
    ///
    /// # Errors
    ///
    /// Returns the failed operation or cleanup. If both failed,
    /// [`Self::OperationAndCleanup`] retains both typed causes.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_tokio::Error;
    ///
    /// let result = Error::after_cleanup::<()>(
    ///     Err(Error::ShutdownPending),
    ///     Err(Error::ShutdownPending),
    /// );
    /// assert!(matches!(result, Err(Error::OperationAndCleanup { .. })));
    /// ```
    pub fn after_cleanup<T>(
        operation: Result<T, Self>,
        cleanup: Result<(), Self>,
    ) -> Result<T, Self> {
        match (operation, cleanup) {
            (result, Ok(())) => result,
            (Ok(_), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(Self::OperationAndCleanup {
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            }),
        }
    }

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
            Self::CoreOperation { source } => source.creation_receipt(),
            Self::OperationFailure {
                receipt, source, ..
            } => receipt.as_deref().or_else(|| source.creation_receipt()),
            #[cfg(feature = "plan")]
            Self::PlanInterrupted { source, .. } => source.creation_receipt(),
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
            #[cfg(feature = "plan")]
            Self::PlanInterrupted { source, .. } => source.operation_effect(),
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

pub(super) fn core_version(error: &libtmux_core::Error) -> Option<&libtmux_core::VersionError> {
    use libtmux_core::Error as CoreError;
    match error {
        CoreError::Version(version) => Some(version),
        CoreError::Cleanup { primary, .. } | CoreError::CleanupPending(primary) => {
            core_version(primary)
        }
        CoreError::AdmissionFailed { source }
        | CoreError::ObservationFailed { source, .. }
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
        CoreError::Busy { .. } | CoreError::Command { .. } | CoreError::JoinedCaptureHook => {
            ErrorKind::Refused
        }
        CoreError::BeforeSpawn
        | CoreError::Deadline
        | CoreError::LateObservation { .. }
        | CoreError::LateCreation { receipt: None } => ErrorKind::Timeout,
        CoreError::UnsupportedVersion => ErrorKind::UnsupportedVersion,
        CoreError::Spawn(_) => ErrorKind::Unreachable,
        CoreError::SnapshotRow(_)
        | CoreError::AfterCreate
        | CoreError::CreationFollowup { receipt: None, .. }
        | CoreError::InvalidKey
        | CoreError::FramedRead
        | CoreError::UnconfirmedCreation { .. } => ErrorKind::Decode,
        CoreError::WrongEndpoint => ErrorKind::InvalidInput,
        CoreError::PaneNotFound(_) | CoreError::StalePane => ErrorKind::ObjectGone,
        CoreError::ObservationFailed { source, .. } | CoreError::AdmissionFailed { source } => {
            core_kind(source)
        }
        CoreError::Cleanup { primary, .. } | CoreError::CleanupPending(primary) => {
            core_kind(primary)
        }
        _ => ErrorKind::Transport,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn unidentified_creation_never_becomes_a_partial_effect_or_transient_error() {
        use libtmux_core::Error as CoreError;

        for (source, kind) in [
            (CoreError::AfterCreate, ErrorKind::Decode),
            (
                CoreError::CreationFollowup {
                    receipt: None,
                    status: Some(1),
                    late: false,
                },
                ErrorKind::Decode,
            ),
            (
                CoreError::LateCreation { receipt: None },
                ErrorKind::Timeout,
            ),
        ] {
            let error = Error::from(source);
            assert_eq!(error.kind(), kind);
            assert_eq!(error.operation_effect(), Some(Effect::Unknown));
            assert!(error.creation_receipt().is_none());
            assert!(!error.is_transient());
        }
    }

    #[test]
    fn completed_cleanup_retains_each_typed_failure_and_creation_receipt() {
        assert_eq!(Error::after_cleanup(Ok(7), Ok(())).unwrap(), 7);
        assert!(matches!(
            Error::after_cleanup::<()>(Ok(()), Err(Error::ShutdownPending)),
            Err(Error::ShutdownPending)
        ));
        let receipt = libtmux_core::NewSession::new("work")
            .decode(Path::new("/example/socket"), b"123 456 %7 $8 @9\n")
            .unwrap();
        let primary = || {
            Error::from(libtmux_core::Error::CreationFollowup {
                receipt: Some(Box::new(receipt.clone())),
                status: Some(1),
                late: false,
            })
        };
        let original = Error::after_cleanup::<()>(Err(primary()), Ok(())).unwrap_err();
        assert_eq!(original.creation_receipt(), Some(&receipt));
        assert!(matches!(original, Error::CoreOperation { .. }));
        let combined =
            Error::after_cleanup::<()>(Err(primary()), Err(Error::ShutdownPending)).unwrap_err();
        assert_eq!(combined.creation_receipt(), Some(&receipt));
        let Error::OperationAndCleanup { primary, cleanup } = combined else {
            panic!("both failures must survive");
        };
        assert!(matches!(*primary, Error::CoreOperation { .. }));
        assert!(matches!(*cleanup, Error::ShutdownPending));
    }
}
