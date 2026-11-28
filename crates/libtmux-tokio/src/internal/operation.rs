use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use libtmux_core::{CreatedSession, CreationNonce, Effect, Reply};
use tokio::time::Instant;

use crate::Error;

pub(crate) type CollectedPrefix = Arc<Mutex<Vec<u8>>>;

pub(crate) async fn finish_shutdown(
    cleanup: impl Future<Output = Result<(), Error>>,
) -> Result<(), Error> {
    tokio::time::timeout(libtmux_core::supervision::CLEANUP_ALLOWANCE, cleanup)
        .await
        .unwrap_or(Err(Error::ShutdownPending))
}

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

pub(crate) fn server_gone_for_observation(
    socket: &Path,
    command: &'static str,
    result: &crate::CommandResult,
    late: bool,
) -> Option<Error> {
    if late || result.exit_code() != Some(1) || !result.stdout().is_empty() {
        return None;
    }
    let diagnostic = result.stderr().strip_suffix(b"\n")?;
    let socket = socket.as_os_str().as_encoded_bytes();
    let kind = if diagnostic.strip_prefix(b"no server running on ") == Some(socket) {
        crate::ServerGoneKind::NotRunning
    } else {
        let path = diagnostic.strip_prefix(b"error connecting to ")?;
        if ![
            b" (No such file or directory)".as_slice(),
            b" (Connection refused)",
        ]
        .iter()
        .any(|suffix| path.strip_suffix(*suffix) == Some(socket))
        {
            return None;
        }
        crate::ServerGoneKind::Unreachable
    };
    Some(Error::ServerGone { command, kind })
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::*;
    use crate::{Command, ErrorKind};

    #[test]
    fn only_timely_empty_endpoint_diagnostics_settle_server_absence() {
        use std::os::unix::process::ExitStatusExt as _;
        use std::process::ExitStatus;

        use crate::command::{CommandResult, ProcessStatus, RequestId};

        let result = |status, stdout: &[u8], stderr: &[u8]| {
            CommandResult::new(
                RequestId::new(1),
                Command::new("capture-pane").summary(),
                ProcessStatus::from_exit_status(ExitStatus::from_raw(status)),
                stdout.to_vec(),
                stderr.to_vec(),
            )
        };
        let socket = Path::new("/private/capture.sock");
        for diagnostic in [
            b"no server running on /private/capture.sock\n".as_slice(),
            b"error connecting to /private/capture.sock (No such file or directory)\n",
            b"error connecting to /private/capture.sock (Connection refused)\n",
        ] {
            assert!(
                server_gone_for_observation(
                    socket,
                    "capture-pane",
                    &result(256, b"", diagnostic),
                    false
                )
                .is_some()
            );
        }
        for (status, stdout, diagnostic, late) in [
            (
                0,
                b"".as_slice(),
                b"no server running on /private/capture.sock\n".as_slice(),
                false,
            ),
            (
                256,
                b"",
                b"no server running on /private/capture.sock\n",
                true,
            ),
            (
                256,
                b"hook bytes",
                b"no server running on /private/capture.sock\n",
                false,
            ),
            (256, b"", b"no server running on /another/socket\n", false),
            (
                256,
                b"",
                b"no server running on /private/capture.sock\nmore\n",
                false,
            ),
            (256, b"", b"server exited unexpectedly\n", false),
            (
                256,
                b"",
                b"error connecting to /private/capture.sock (Permission denied)\n",
                false,
            ),
            (256, b"", b"protocol version mismatch\n", false),
        ] {
            assert!(
                server_gone_for_observation(
                    socket,
                    "capture-pane",
                    &result(status, stdout, diagnostic),
                    late
                )
                .is_none()
            );
        }
    }

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
