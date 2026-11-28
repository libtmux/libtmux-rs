use std::path::Path;

use crate::{
    Capture, CommandKind, CreatedSession, CreationNonce, Error, NewSession, ObservePane, PaneId,
    PaneKey, framed,
};

/// A native driver's collected reply before operation-specific verification.
///
/// Only typed, validated results cross the driver's public boundary. The
/// diagnostic stream contributes a byte count without retaining its contents.
#[doc(hidden)]
pub struct Reply {
    status: Option<i32>,
    stdout: Vec<u8>,
    diagnostic_len: usize,
    late: bool,
}

impl std::fmt::Debug for Reply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Reply")
            .field("status", &self.status)
            .field("stdout_len", &self.stdout.len())
            .field("diagnostic_len", &self.diagnostic_len)
            .field("late", &self.late)
            .finish()
    }
}

impl Reply {
    /// Capture process status, output and the driver's deadline observation.
    #[must_use]
    pub fn new(status: Option<i32>, stdout: Vec<u8>, diagnostic_len: usize, late: bool) -> Self {
        Self {
            status,
            stdout,
            diagnostic_len,
            late,
        }
    }

    /// Retain a complete, unique creation receipt from a failed collection.
    ///
    /// An incomplete read never yields screen or listing data. A creation can
    /// keep its verified initial action receipt while preserving the original
    /// interruption and cleanup cause.
    #[must_use]
    pub fn interrupted(creation_socket: Option<&Path>, stdout: &[u8], cause: Error) -> Error {
        let Some(socket) = creation_socket else {
            return cause;
        };
        let Ok((action, _)) = framed::collected_prefix(stdout)
            .skip_creation_notices()
            .take(None)
        else {
            return cause;
        };
        if !action.success {
            return cause;
        }
        match NewSession::new("").decode(socket, action.bytes) {
            Ok(receipt) => Error::CreationInterrupted {
                receipt,
                source: Box::new(cause),
            },
            Err(_) => cause,
        }
    }

    fn command_error(&self, operation: CommandKind) -> Error {
        Error::Command {
            operation,
            status: self.status,
            diagnostic_len: self.diagnostic_len,
            late: self.late,
        }
    }

    /// Verify the initial creation action independently of later hook frames.
    ///
    /// # Errors
    ///
    /// Returns an unconfirmed effect for ambiguous action framing, and a known
    /// creation with any readable receipt when hooks, exit or deadline fail
    /// after an acknowledged action. Malformed receipt payloads are discarded.
    pub fn creation(self, request: NewSession, socket: &Path) -> Result<CreatedSession, Error> {
        let unconfirmed = || Error::UnconfirmedCreation {
            status: self.status,
            late: self.late,
        };
        let (frames, saw_exit) = match framed::first(&self.stdout) {
            Ok(frames) => (frames, true),
            Err(_) => (framed::collected_prefix(&self.stdout), false),
        };
        let (action, rest) = frames
            .skip_creation_notices()
            .take(None)
            .map_err(|_| unconfirmed())?;
        if !action.success {
            return Err(self.command_error(CommandKind::NewSession));
        }
        let receipt = request.decode(socket, action.bytes);
        if !saw_exit && receipt.is_err() {
            return Err(unconfirmed());
        }
        if !saw_exit || self.status != Some(0) || !rest.followups_succeeded(action.number) {
            return Err(Error::CreationFollowup {
                receipt: receipt.ok(),
                status: self.status,
                late: self.late,
            });
        }
        if self.late {
            return Err(Error::LateCreation {
                receipt: receipt.ok(),
            });
        }
        receipt
    }

    /// Verify an ordinary creation's unique request-specific first record.
    ///
    /// The driver must use the nonce encoded in this request's print projection.
    /// In the supported built-in command, printing precedes client hook output.
    /// A later row is never searched for a receipt. A repeated nonce makes the
    /// reply ambiguous, even when the first record was well formed.
    ///
    /// # Errors
    ///
    /// Returns an unconfirmed effect for absent, incomplete or ambiguous
    /// markers. A verified acknowledgement retains any typed receipt across
    /// process failure, a failed follow-up or a late reply.
    pub fn ordinary_creation(
        self,
        request: NewSession,
        socket: &Path,
        nonce: CreationNonce,
    ) -> Result<CreatedSession, Error> {
        let row = nonce
            .receipt_row(&self.stdout)
            .ok_or(Error::UnconfirmedCreation {
                status: self.status,
                late: self.late,
            })?;
        let receipt = request.decode(socket, &row);
        if self.status != Some(0) {
            return Err(Error::CreationFollowup {
                receipt: receipt.ok(),
                status: self.status,
                late: self.late,
            });
        }
        if self.late {
            return Err(Error::LateCreation {
                receipt: receipt.ok(),
            });
        }
        receipt
    }

    /// Retain an ordinary receipt collected before cancellation or reply loss.
    ///
    /// Only a complete first record with a unique nonce and valid typed receipt
    /// changes the effect evidence. Malformed or incomplete output preserves
    /// the original interruption and its uncertainty.
    #[must_use]
    pub fn interrupted_ordinary_creation(
        socket: &Path,
        nonce: CreationNonce,
        stdout: &[u8],
        cause: Error,
    ) -> Error {
        let Some(row) = nonce.receipt_row(stdout) else {
            return cause;
        };
        match NewSession::new("").decode(socket, &row) {
            Ok(receipt) => Error::CreationInterrupted {
                receipt,
                source: Box::new(cause),
            },
            Err(_) => cause,
        }
    }

    /// Verify an exact pane observation and discard any late result.
    ///
    /// # Errors
    ///
    /// Returns an observation error for failed framing, an absent or different
    /// pane, inconsistent duplicate rows, process failure or a late reply.
    pub fn observe(self, socket: &Path, pane: PaneId) -> Result<PaneKey, Error> {
        let result = ObservePane::decode_frames(socket, pane, self.status, &self.stdout);
        self.observation(CommandKind::ListPanes, result)
    }

    /// Return screen bytes only after both guard and capture frames validate.
    ///
    /// # Errors
    ///
    /// Returns [`Error::StalePane`] for an authenticated generation refusal,
    /// or an observation error for ambiguous framing, failure or a late reply.
    pub fn capture(self) -> Result<Vec<u8>, Error> {
        let result = Capture::decode_frames(self.status, &self.stdout);
        self.observation(CommandKind::CapturePane, result)
    }

    fn observation<T>(
        &self,
        operation: CommandKind,
        decoded: Result<T, Error>,
    ) -> Result<T, Error> {
        match decoded {
            Err(Error::FramedRead) if self.status != Some(0) => Err(self.command_error(operation)),
            Ok(_) if self.late => Err(Error::LateObservation { operation }),
            result => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Effect, supervision};

    const ACTION: &[u8] = b"%begin 1 2 0\n123 456 %1 $2 @3\n%end 1 2 0\n";

    fn creation(status: Option<i32>, bytes: &[u8], late: bool) -> Result<CreatedSession, Error> {
        Reply::new(status, bytes.to_vec(), 0, late)
            .creation(NewSession::new("work"), Path::new("/test/socket"))
    }

    fn assert_secret_absent(error: &Error, secret: &str) {
        assert!(!format!("{error} {error:?} {error:#?}").contains(secret));
    }

    #[test]
    fn late_creation_retains_typed_acknowledgement_without_receipt_payloads() {
        let output = [ACTION, b"%exit\n"].concat();
        let receipt = creation(Some(0), &output, false).unwrap();
        assert_eq!(receipt.session().to_string(), "$2");
        let error = creation(Some(0), &output, true).unwrap_err();
        assert_eq!(error.effect(), Effect::Created);
        assert!(matches!(
            error,
            Error::LateCreation { receipt: Some(receipt) }
                if receipt.pane().id().to_string() == "%1"
        ));

        let malformed = b"%begin 1 2 0\nprivate-receipt\n%end 1 2 0\n%exit\n";
        let error = creation(Some(0), malformed, true).unwrap_err();
        assert!(matches!(error, Error::LateCreation { receipt: None }));
        assert_eq!(error.effect(), Effect::Created);
        assert_secret_absent(&error, "private-receipt");
    }

    #[test]
    fn initial_failure_and_failed_followup_carry_different_effect_evidence() {
        for late in [false, true] {
            let error = creation(
                Some(1),
                b"%begin 1 2 0\n123 456 %1 $2 @3\n%error 1 2 0\n%exit\n",
                late,
            )
            .unwrap_err();
            assert!(matches!(
                error,
                Error::Command {
                    operation: CommandKind::NewSession,
                    ..
                }
            ));
            assert_eq!(error.effect(), Effect::Unknown);

            for (row, has_ids) in [(&b"123 456 %1 $2 @3\n"[..], true), (b"bad\n", false)] {
                let output = [
                    &b"%config-error cold diagnostic\n%begin 1 2 0\n"[..],
                    row,
                    b"%end 1 2 0\n%sessions-changed\n%begin 1 3 0\nprivate-hook\n%error 1 3 0\n%exit\n",
                ]
                .concat();
                let error = creation(Some(1), &output, late).unwrap_err();
                assert_eq!(error.effect(), Effect::Created);
                assert_secret_absent(&error, "private-hook");
                assert!(matches!(
                    error,
                    Error::CreationFollowup { receipt, status: Some(1), late: observed }
                        if receipt.is_some() == has_ids && observed == late
                ));
            }
        }
    }

    #[test]
    fn missing_exit_keeps_a_complete_receipt_and_process_status() {
        for status in [None, Some(7), Some(0)] {
            for late in [false, true] {
                let output = [ACTION, b"%begin 1 3 0\nprivate-hook"].concat();
                let error = creation(status, &output, late).unwrap_err();
                assert_eq!(error.effect(), Effect::Created);
                assert_secret_absent(&error, "private-hook");
                assert!(matches!(
                    error,
                    Error::CreationFollowup { receipt: Some(receipt), status: observed, late: elapsed }
                        if observed == status && elapsed == late && receipt.session().to_string() == "$2"
                ));
                for ambiguous in [
                    &b"%begin 1 2 0\n123 456 %1 $2 @3\n%end 1 2"[..],
                    b"%begin 1 2 0\nunreadable\n%end 1 2 0\n",
                    &[ACTION, b"%end 1 2 0\n"].concat(),
                ] {
                    assert_eq!(
                        creation(status, ambiguous, late).unwrap_err().effect(),
                        Effect::Unknown
                    );
                }
            }
        }
    }

    #[test]
    fn duplicate_or_incomplete_initial_frames_do_not_certify_creation() {
        for bytes in [
            &b"123 456 %1 $2 @3\n%exit\n"[..],
            b"%begin 1 2 0\n123 456 %1 $2 @3\n%end 1 2 0\n%begin 1 3 0\n%end 1 2 0\n%end 1 3 0\n%exit\n",
            b"%begin 1 2 0\n123 456 %1 $2 @3\n%exit\n",
            b"%begin 1 2 1\n123 456 %1 $2 @3\n%end 1 2 1\n%exit\n",
            b"%begin +1 2 0\n123 456 %1 $2 @3\n%end +1 2 0\n%exit\n",
        ] {
            let error = creation(Some(0), bytes, false).unwrap_err();
            assert!(matches!(error, Error::UnconfirmedCreation { .. }));
            assert_eq!(error.effect(), Effect::Unknown);
        }
        let unknown_tail = [ACTION, b"%unrecognised-record\n%exit\n"].concat();
        assert!(matches!(
            creation(Some(0), &unknown_tail, false),
            Err(Error::CreationFollowup {
                receipt: Some(_),
                ..
            })
        ));
    }

    #[test]
    fn interrupted_creation_only_keeps_a_unique_complete_first_receipt() {
        let socket = Path::new("/private/socket");
        for fragment in [
            &b""[..],
            b"123 456 %1 $2 @3\n",
            b"%begin 1 2 0\n123 456 %1 $2 @3\n%end 1 2",
            b"%begin 1 2 0\n123 456 %1 $2 @3\n%error 1 2 0\n",
            b"%begin 1 2 0\nbad\n%end 1 2 0\n",
            b"%output %1 untrusted\n%begin 1 2 0\n123 456 %1 $2 @3\n%end 1 2 0\n",
            &[ACTION, b"%begin 1 3 0\n%end 1 2 0\n"].concat(),
        ] {
            assert!(matches!(
                Reply::interrupted(Some(socket), fragment, Error::Deadline),
                Error::Deadline
            ));
        }
        let fragment = [ACTION, b"%sessions-changed\n%begin 1 3 0\nprivate-hook"].concat();
        let interrupted = Reply::interrupted(Some(socket), &fragment, Error::Deadline);
        let error = supervision::after_cleanup::<()>(interrupted, None);
        assert_eq!(error.effect(), Effect::Created);
        assert_secret_absent(&error, "private-hook");
        let Error::CleanupPending(primary) = error else {
            panic!("missing cleanup evidence")
        };
        assert!(matches!(
            *primary,
            Error::CreationInterrupted { receipt, source }
                if receipt.session().to_string() == "$2" && matches!(*source, Error::Deadline)
        ));
        assert!(matches!(
            Reply::interrupted(None, &fragment, Error::Deadline),
            Error::Deadline
        ));
    }

    #[test]
    fn observations_never_expose_late_or_failed_capture_bytes() {
        let output = b"%begin 1 2 0\n%end 1 2 0\n%begin 1 3 0\nprivate-screen\n%end 1 3 0\n%exit\n";
        let error = Reply::new(Some(0), output.to_vec(), 0, true)
            .capture()
            .unwrap_err();
        assert!(matches!(
            error,
            Error::LateObservation {
                operation: CommandKind::CapturePane
            }
        ));
        assert_secret_absent(&error, "private-screen");
        for malformed in [
            &b"%begin 1 2 0\n%end 1 2 0\n%begin 1 3 0\nprivate-screen"[..],
            b"%begin 1 2 0\n%end 1 2 0\n%begin 1 3 0\nprivate-screen\n%end 1 3 0\n",
        ] {
            let error = Reply::new(Some(0), malformed.to_vec(), 0, false)
                .capture()
                .unwrap_err();
            assert!(matches!(error, Error::FramedRead));
            assert_eq!(error.effect(), Effect::Observation);
            assert_secret_absent(&error, "private-screen");
        }
    }

    #[test]
    fn read_transport_failure_keeps_original_cleanup_evidence() {
        let primary = Error::CleanupPending(Box::new(Error::Deadline));
        let error = primary.during_read(CommandKind::CapturePane);
        assert_eq!(error.effect(), Effect::Observation);
        let Error::ObservationFailed { source, .. } = error else {
            panic!("missing observation context")
        };
        assert!(matches!(
            *source,
            Error::CleanupPending(primary) if matches!(*primary, Error::Deadline)
        ));
        for error in [
            Error::CancelledBeforeSpawn,
            Error::BeforeSpawn,
            Error::UnsupportedVersion,
        ] {
            assert_eq!(
                error.during_read(CommandKind::ListPanes).effect(),
                Effect::NotSubmitted
            );
        }
    }
}
