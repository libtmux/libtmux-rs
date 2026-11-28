use crate::{NewSession, Operand};
use std::fmt;

/// A fresh per-dispatch marker for an ordinary creation receipt.
///
/// The driver supplies 128 bits from an OS random source for each submission.
/// Reusing a nonce allows old or independently printed rows to imitate this
/// request. The marker distinguishes output provenance; it does not authenticate
/// a tmux daemon, client executable or configuration that can inspect it.
///
/// # Examples
///
/// ```
/// use libtmux_core::{CreationNonce, NewSession};
///
/// # let fresh_os_random_bytes = [0x5a; 16];
/// let nonce = CreationNonce::from_bytes(fresh_os_random_bytes);
/// let operands = NewSession::new("work").ordinary_operands(nonce);
/// assert_eq!(operands[0].as_os_str(), "-d");
/// ```
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct CreationNonce([u8; 16]);

impl CreationNonce {
    /// Bind driver-supplied fresh OS entropy to one creation dispatch.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub(crate) fn encoded(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(32);
        for byte in self.0 {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        encoded
    }

    fn projection(self) -> String {
        format!(
            "libtmux-create:{} {} :end",
            self.encoded(),
            NewSession::RECEIPT_FORMAT
        )
    }

    pub(crate) fn receipt_row(self, stdout: &[u8]) -> Option<Vec<u8>> {
        let encoded = self.encoded();
        let mut occurrences = stdout
            .windows(encoded.len())
            .filter(|candidate| *candidate == encoded.as_bytes());
        if occurrences.next().is_none() || occurrences.next().is_some() {
            return None;
        }
        let end = stdout.iter().position(|byte| *byte == b'\n')?;
        let prefix = format!("libtmux-create:{encoded} ");
        let row = stdout[..end]
            .strip_prefix(prefix.as_bytes())?
            .strip_suffix(b" :end")?;
        let mut row = row.to_vec();
        row.push(b'\n');
        Some(row)
    }
}

impl fmt::Debug for CreationNonce {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CreationNonce(<redacted>)")
    }
}

impl NewSession {
    /// Encode ordinary creation operands with a request-specific print marker.
    ///
    /// A native driver must generate a fresh [`CreationNonce`] for every
    /// dispatch and use that same nonce when decoding. No control client is
    /// attached. The ordinary decoder accepts only the first complete record;
    /// the same nonce appearing again makes the reply ambiguous.
    #[must_use]
    pub fn ordinary_operands(&self, nonce: CreationNonce) -> Vec<Operand> {
        self.operands(&nonce.projection())
    }

    /// Encode ordinary process argv for a request-specific creation receipt.
    ///
    /// This has no `-C` flag. Global endpoint and configuration arguments belong
    /// before the returned vector. Literal trailing semicolons remain values.
    #[cfg(any(unix, windows))]
    #[must_use]
    pub fn ordinary_arguments(&self, nonce: CreationNonce) -> Vec<std::ffi::OsString> {
        let mut arguments = vec![std::ffi::OsString::from("new-session")];
        arguments.extend(
            self.ordinary_operands(nonce)
                .iter()
                .map(|operand| crate::operand::argv_operand(operand.as_os_str())),
        );
        arguments
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::{CreatedSession, Effect, Error, Reply, supervision};

    const NONCE: CreationNonce = CreationNonce::from_bytes([0xab; 16]);
    const RECEIPT: &[u8] =
        b"libtmux-create:abababababababababababababababab 123 456 %1 $2 @3 :end\n";

    fn creation(status: Option<i32>, bytes: &[u8], late: bool) -> Result<CreatedSession, Error> {
        Reply::new(status, bytes.to_vec(), 0, late).ordinary_creation(
            NewSession::new("work"),
            Path::new("/socket"),
            NONCE,
        )
    }

    #[test]
    fn ordinary_creation_binds_the_projection_to_this_request() {
        let request = NewSession::new("work;")
            .environment("TOKEN", "secret;")
            .command("command;");
        let arguments = request.ordinary_arguments(NONCE);
        assert_eq!(arguments[0], "new-session");
        assert!(!arguments.iter().any(|argument| argument == "-C"));
        assert_eq!(
            arguments[4],
            "libtmux-create:abababababababababababababababab #{pid} #{start_time} #{pane_id} #{session_id} #{window_id} :end"
        );
        assert_eq!(arguments[6], "work\\;");
        assert_eq!(arguments[8], "TOKEN=secret\\;");
        assert_eq!(arguments[10], "command\\;");
        let operands = request.ordinary_operands(NONCE);
        assert_eq!(operands[7].kind(), crate::OperandKind::Sensitive);
        assert_eq!(operands[9].kind(), crate::OperandKind::Sensitive);
        assert!(!format!("{operands:?} {NONCE:?}").contains("secret"));
        assert!(!format!("{NONCE:?}").contains("abababab"));
        let receipt = creation(Some(0), RECEIPT, false).unwrap();
        assert_eq!(receipt.session().to_string(), "$2");
        assert_eq!(receipt.pane().id().to_string(), "%1");
        assert_eq!(receipt.generation().pid(), 123);
    }

    #[test]
    fn misleading_id_rows_do_not_become_receipts() {
        for output in [
            &b"123 456 %1 $2 @3\n"[..],
            &[b"123 456 %77 $88 @99\n".as_slice(), RECEIPT].concat(),
            b"libtmux-create:cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd 123 456 %1 $2 @3 :end\n",
            b"libtmux-create:abababababababababababababababab 123 456 %1 $2 @3\n",
            b"libtmux-create:abababababababababababababababab 123 456 %1 $2 @3 :end",
        ] {
            let error = creation(Some(0), output, false).unwrap_err();
            assert!(matches!(error, Error::UnconfirmedCreation { .. }));
            assert_eq!(error.effect(), Effect::Unknown);
        }
    }

    #[test]
    fn duplicate_nonce_is_ambiguous_even_after_a_well_formed_receipt() {
        for output in [
            [RECEIPT, RECEIPT].concat(),
            [RECEIPT, b"hook echoed abababababababababababababababab\n"].concat(),
            [RECEIPT, b"libtmux-create:abababababababababababababababab"].concat(),
        ] {
            assert_eq!(
                creation(Some(0), &output, false).unwrap_err().effect(),
                Effect::Unknown
            );
            assert!(matches!(
                Reply::interrupted_ordinary_creation(
                    Path::new("/socket"),
                    NONCE,
                    &output,
                    Error::Deadline
                ),
                Error::Deadline
            ));
        }
    }

    #[test]
    fn hook_output_is_separate_from_the_initial_receipt() {
        let output = [RECEIPT, b"123 456 %77 $88 @99\nprivate-hook\n"].concat();
        let receipt = creation(Some(0), &output, false).unwrap();
        assert_eq!(receipt.session().to_string(), "$2");
        for (status, late) in [(Some(1), false), (None, false), (Some(1), true)] {
            let error = creation(status, &output, late).unwrap_err();
            assert_eq!(error.effect(), Effect::Created);
            assert!(!format!("{error} {error:?}").contains("private-hook"));
            assert!(matches!(
                error,
                Error::CreationFollowup { receipt: Some(receipt), status: found, late: observed }
                    if found == status && observed == late && receipt.session().to_string() == "$2"
            ));
        }
        assert!(matches!(
            creation(Some(0), &output, true),
            Err(Error::LateCreation { receipt: Some(_) })
        ));
    }

    #[test]
    fn interrupted_receipts_preserve_cause_and_cleanup_evidence() {
        let output = [RECEIPT, b"private partial hook"].concat();
        let interrupted = Reply::interrupted_ordinary_creation(
            Path::new("/socket"),
            NONCE,
            &output,
            Error::Deadline,
        );
        let error = supervision::after_cleanup::<()>(interrupted, None);
        assert_eq!(error.effect(), Effect::Created);
        assert!(!format!("{error} {error:?}").contains("private partial hook"));
        let Error::CleanupPending(primary) = error else {
            panic!("missing cleanup context")
        };
        assert!(matches!(
            *primary,
            Error::CreationInterrupted { receipt, source }
                if receipt.window().to_string() == "@3" && matches!(*source, Error::Deadline)
        ));
        for fragment in [
            &b""[..],
            b"123 456 %1 $2 @3\n",
            b"libtmux-create:abababababababababababababababab 123 456 %1 $2 @3 :end",
            b"libtmux-create:abababababababababababababababab unreadable :end\n",
        ] {
            assert!(matches!(
                Reply::interrupted_ordinary_creation(
                    Path::new("/socket"),
                    NONCE,
                    fragment,
                    Error::Cancelled
                ),
                Error::Cancelled
            ));
        }
    }

    #[test]
    fn malformed_stamped_receipts_keep_acknowledgement_without_payload_bytes() {
        let output = b"libtmux-create:abababababababababababababababab private-receipt :end\n";
        let error = creation(Some(0), output, false).unwrap_err();
        assert!(matches!(error, Error::AfterCreate));
        assert_eq!(error.effect(), Effect::Created);
        for (status, late) in [(Some(0), true), (Some(1), false)] {
            let error = creation(status, output, late).unwrap_err();
            assert_eq!(error.effect(), Effect::Created);
            assert!(!format!("{error} {error:?}").contains("private-receipt"));
        }
    }
}
