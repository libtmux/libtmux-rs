use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use libtmux_core::{CreatedSession, CreationNonce, Effect, Reply};
use tokio::time::Instant;

use crate::Error;

pub(crate) type CollectedPrefix = Arc<Mutex<Vec<u8>>>;

#[derive(Clone)]
pub(crate) struct OperationContext {
    deadline: Option<Instant>,
    creation: Option<CreationCollection>,
}

#[derive(Clone)]
struct CreationCollection {
    socket: PathBuf,
    nonce: CreationNonce,
    stdout: CollectedPrefix,
}

impl OperationContext {
    pub(crate) fn creation(socket: &Path, nonce: CreationNonce, deadline: Option<Instant>) -> Self {
        Self {
            deadline,
            creation: Some(CreationCollection {
                socket: socket.to_owned(),
                nonce,
                stdout: Arc::new(Mutex::new(Vec::new())),
            }),
        }
    }

    pub(crate) const fn observation(deadline: Option<Instant>) -> Self {
        Self {
            deadline,
            creation: None,
        }
    }

    pub(crate) const fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(crate) fn is_late(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    pub(crate) fn stdout_prefix(&self) -> Option<CollectedPrefix> {
        self.creation
            .as_ref()
            .map(|creation| Arc::clone(&creation.stdout))
    }

    fn receipt(&self) -> Option<CreatedSession> {
        let creation = self.creation.as_ref()?;
        let bytes = creation
            .stdout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match Reply::interrupted_ordinary_creation(
            &creation.socket,
            creation.nonce,
            &bytes,
            libtmux_core::Error::Cancelled,
        ) {
            libtmux_core::Error::CreationInterrupted { receipt, .. } => Some(*receipt),
            _ => None,
        }
    }

    pub(crate) fn interrupted(&self, source: Error) -> Error {
        let receipt = self.receipt();
        let effect = if receipt.is_some() {
            Effect::Created
        } else if self.creation.is_some() {
            Effect::Unknown
        } else {
            Effect::Observation
        };
        Error::OperationFailure {
            effect,
            receipt: receipt.map(Box::new),
            source: Box::new(source),
        }
    }

    pub(crate) fn before_dispatch(source: Error) -> Error {
        if source.operation_effect().is_some() {
            source
        } else {
            Error::OperationFailure {
                effect: Effect::NotSubmitted,
                receipt: None,
                source: Box::new(source),
            }
        }
    }
}

pub(crate) fn nonce() -> Result<CreationNonce, Error> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|source| {
        Error::from(libtmux_core::Error::Spawn(io::Error::other(
            source.to_string(),
        )))
    })?;
    Ok(CreationNonce::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::*;
    use crate::{Command, ErrorKind};

    fn timeout() -> Error {
        Error::timeout(
            1,
            Command::new("new-session").summary(),
            Duration::from_secs(1),
        )
    }

    #[test]
    fn an_interrupted_creation_retains_the_verified_receipt_and_original_cause() {
        let context = OperationContext::creation(
            Path::new("/private/socket"),
            CreationNonce::from_bytes([0xab; 16]),
            None,
        );
        context
            .stdout_prefix()
            .unwrap()
            .lock()
            .unwrap()
            .extend_from_slice(
            b"libtmux-create:abababababababababababababababab 123 456 %1 $2 @3 :end\nprivate-hook",
        );
        let error = context.interrupted(timeout());
        assert_eq!(error.operation_effect(), Some(Effect::Created));
        assert_eq!(
            error.creation_receipt().unwrap().session().to_string(),
            "$2"
        );
        assert_eq!(error.kind(), ErrorKind::PartialEffect);
        assert!(!error.is_transient());
        assert!(!format!("{error:?} {error}").contains("private-hook"));
        assert!(
            matches!(error, Error::OperationFailure { source, .. } if matches!(*source, Error::Timeout { .. }))
        );
    }

    #[test]
    fn ambiguous_prefixes_remain_uncertain_and_cannot_suggest_replay() {
        let context = OperationContext::creation(
            Path::new("/private/socket"),
            CreationNonce::from_bytes([0xab; 16]),
            None,
        );
        context.stdout_prefix().unwrap().lock().unwrap().extend_from_slice(
            b"libtmux-create:abababababababababababababababab 123 456 %1 $2 @3 :end\nabababababababababababababababab",
        );
        let error = context.interrupted(timeout());
        assert_eq!(error.operation_effect(), Some(Effect::Unknown));
        assert!(error.creation_receipt().is_none());
        assert!(!error.is_transient());
    }
}
