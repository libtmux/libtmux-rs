use std::fmt;
use std::sync::Arc;

use libtmux_core::{
    Effect, Error,
    control::{ControlCommandRefusal, ControlFault},
    supervision::CleanupErrors,
};

/// A persistent-client failure retaining submission and cleanup evidence.
///
/// Reply bytes are never included in this error's diagnostics.
///
/// # Examples
///
/// ```
/// use libtmux_blocking::control::ControlError;
/// use libtmux_core::{Effect, Error};
///
/// let error = ControlError::from(Error::BeforeSpawn);
/// assert_eq!(error.effect(), Effect::NotSubmitted);
/// assert!(error.fault().is_none());
/// assert!(!error.is_cleanup_pending());
/// ```
#[derive(Clone, Debug)]
pub struct ControlError(Arc<Failure>);

#[derive(Debug)]
enum Failure {
    Admission(ControlError),
    Core(Error),
    Policy(ControlCommandRefusal),
    Native {
        source: std::io::Error,
        effect: Effect,
    },
    Protocol {
        fault: ControlFault,
        effect: Effect,
    },
    Cleanup {
        primary: ControlError,
        errors: CleanupErrors,
    },
    CleanupPending(ControlError),
}

impl ControlError {
    /// Whether a caller's command may have reached tmux.
    #[must_use]
    pub fn effect(&self) -> Effect {
        match self.0.as_ref() {
            Failure::Admission(_) | Failure::Policy(_) => Effect::NotSubmitted,
            Failure::Core(error) => error.effect(),
            Failure::Native { effect, .. } | Failure::Protocol { effect, .. } => *effect,
            Failure::Cleanup { primary, .. } | Failure::CleanupPending(primary) => primary.effect(),
        }
    }

    /// The protocol or admission cause, if this failure has one.
    #[must_use]
    pub fn fault(&self) -> Option<ControlFault> {
        match self.0.as_ref() {
            Failure::Protocol { fault, .. } => Some(*fault),
            Failure::Admission(primary)
            | Failure::Cleanup { primary, .. }
            | Failure::CleanupPending(primary) => primary.fault(),
            _ => None,
        }
    }

    /// Why the public command was refused before occupying a request slot.
    #[must_use]
    pub fn policy_refusal(&self) -> Option<ControlCommandRefusal> {
        match self.0.as_ref() {
            Failure::Policy(refusal) => Some(*refusal),
            Failure::Admission(primary)
            | Failure::Cleanup { primary, .. }
            | Failure::CleanupPending(primary) => primary.policy_refusal(),
            _ => None,
        }
    }

    /// Failed signal and reap operations, when cleanup reported them.
    #[must_use]
    pub fn cleanup_errors(&self) -> Option<&CleanupErrors> {
        match self.0.as_ref() {
            Failure::Cleanup { errors, .. } => Some(errors),
            Failure::Admission(primary) | Failure::CleanupPending(primary) => {
                primary.cleanup_errors()
            }
            _ => None,
        }
    }

    /// Whether bounded reporting ended while the child was still being reclaimed.
    #[must_use]
    pub fn is_cleanup_pending(&self) -> bool {
        match self.0.as_ref() {
            Failure::CleanupPending(_) => true,
            Failure::Admission(primary) | Failure::Cleanup { primary, .. } => {
                primary.is_cleanup_pending()
            }
            _ => false,
        }
    }

    pub(super) fn protocol(fault: ControlFault, submitted: bool) -> Self {
        Self(Arc::new(Failure::Protocol {
            fault,
            effect: stage(submitted),
        }))
    }

    pub(super) fn native(source: std::io::Error, submitted: bool) -> Self {
        Self(Arc::new(Failure::Native {
            source,
            effect: stage(submitted),
        }))
    }

    pub(super) fn cleanup(self, errors: CleanupErrors) -> Self {
        Self(Arc::new(Failure::Cleanup {
            primary: self,
            errors,
        }))
    }

    pub(super) fn pending(self) -> Self {
        Self(Arc::new(Failure::CleanupPending(self)))
    }

    pub(super) fn before_submission(self) -> Self {
        Self(Arc::new(Failure::Admission(self)))
    }
}

const fn stage(submitted: bool) -> Effect {
    if submitted {
        Effect::Unknown
    } else {
        Effect::NotSubmitted
    }
}

impl From<Error> for ControlError {
    fn from(error: Error) -> Self {
        Self(Arc::new(Failure::Core(error)))
    }
}

impl From<ControlCommandRefusal> for ControlError {
    fn from(refusal: ControlCommandRefusal) -> Self {
        Self(Arc::new(Failure::Policy(refusal)))
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.as_ref() {
            Failure::Admission(error) => error.fmt(formatter),
            Failure::Core(error) => error.fmt(formatter),
            Failure::Policy(refusal) => refusal.fmt(formatter),
            Failure::Native { .. } => formatter.write_str("native control transport failed"),
            Failure::Protocol { fault, .. } => fault.fmt(formatter),
            Failure::Cleanup { primary, .. } => {
                write!(formatter, "{primary}; control cleanup failed")
            }
            Failure::CleanupPending(primary) => {
                write!(formatter, "{primary}; control cleanup is still pending")
            }
        }
    }
}

impl std::error::Error for ControlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self.0.as_ref() {
            Failure::Admission(error) => error,
            Failure::Core(error) => error,
            Failure::Policy(refusal) => refusal,
            Failure::Native { source, .. } => source,
            Failure::Protocol { fault, .. } => fault,
            Failure::Cleanup { primary, .. } | Failure::CleanupPending(primary) => primary,
        })
    }
}
