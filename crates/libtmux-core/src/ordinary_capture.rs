//! Ordinary capture when joined rows have no known line count.

use std::ffi::OsString;

use crate::{CommandKind, CreationNonce, Error, PaneId};

/// Collect hook settings and joined pane bytes from one guarded command queue.
#[doc(hidden)]
pub struct OrdinaryCapturePrefix {
    marker: String,
}

impl std::fmt::Debug for OrdinaryCapturePrefix {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OrdinaryCapturePrefix(<redacted>)")
    }
}

impl OrdinaryCapturePrefix {
    /// Bind the hook inventory to this capture.
    #[must_use]
    pub fn new(nonce: CreationNonce) -> Self {
        Self {
            marker: format!("libtmux-capture:{}", nonce.encoded()),
        }
    }

    /// Insert the global and target-session hook reads immediately before capture.
    ///
    /// # Errors
    ///
    /// Requires the guarded `capture-pane` argv produced by the ordinary observation.
    pub fn insert(&self, arguments: &mut Vec<OsString>, pane: &PaneId) -> Result<(), Error> {
        let index = arguments
            .windows(2)
            .position(|pair| pair[0] == ";" && pair[1] == "capture-pane")
            .ok_or(Error::FramedRead)?
            + 1;
        arguments.splice(
            index..index,
            [
                "show-hooks",
                "-g",
                "after-capture-pane",
                ";",
                "show-hooks",
                "-t",
                pane.as_ref(),
                "after-capture-pane",
                ";",
                "list-commands",
                "-F",
                &self.marker,
                "display-message",
                ";",
            ]
            .map(OsString::from),
        );
        Ok(())
    }

    /// Return the native bytes when both inventories prove the hook is empty.
    ///
    /// # Errors
    ///
    /// Rejects a configured hook, unexpected inventory, late reply or client failure.
    pub fn decode(
        &self,
        status: Option<i32>,
        mut stdout: Vec<u8>,
        stderr: &[u8],
        late: bool,
    ) -> Result<Vec<u8>, Error> {
        check_capture_status(status, &stdout, stderr, late)?;
        // An empty global array prints its bare name. The session prints
        // nothing when inherited and the same bare name for a local empty array.
        let empty = b"after-capture-pane\n";
        let mut prefix = stdout.as_slice();
        if let Some(local) = prefix.strip_prefix(empty) {
            prefix = local.strip_prefix(empty).unwrap_or(local);
        } else if !prefix.starts_with(b"after-capture-pane[") {
            return Err(Error::FramedRead);
        }
        if prefix.starts_with(b"after-capture-pane[") {
            return Err(Error::JoinedCaptureHook);
        }
        let body = prefix
            .strip_prefix(self.marker.as_bytes())
            .and_then(|remaining| remaining.strip_prefix(b"\n"))
            .filter(|bytes| bytes.ends_with(b"\n"))
            .ok_or(Error::FramedRead)?;
        let offset = stdout.len() - body.len();
        stdout.drain(..offset);
        Ok(stdout)
    }
}

pub(crate) fn check_capture_status(
    status: Option<i32>,
    _stdout: &[u8],
    stderr: &[u8],
    late: bool,
) -> Result<(), Error> {
    // Queue guards include changing read state as well as object identity.
    // Their refusal alone does not establish which check failed.
    if status != Some(0) {
        return Err(Error::Command {
            operation: CommandKind::CapturePane,
            status,
            diagnostic_len: stderr.len(),
            late,
        });
    }
    if late {
        return Err(Error::LateObservation {
            operation: CommandKind::CapturePane,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joined_capture_accepts_only_an_empty_hook_inventory_at_the_start() {
        let prefix = OrdinaryCapturePrefix::new(CreationNonce::from_bytes([0x12; 16]));
        let body = [
            b"after-capture-pane[12]\n" as &[u8],
            prefix.marker.as_bytes(),
            b"\n\xff\n",
        ]
        .concat();
        for local in [b"" as &[u8], b"after-capture-pane\n"] {
            let stdout = [
                b"after-capture-pane\n",
                local,
                prefix.marker.as_bytes(),
                b"\n",
                &body,
            ]
            .concat();
            assert_eq!(prefix.decode(Some(0), stdout, b"", false).unwrap(), body);
        }
        for prelude in [
            b"after-capture-pane[17] display-message -p TEXT\n" as &[u8],
            b"after-capture-pane\nafter-capture-pane[17] display-message -p TEXT\n",
            b"unrelated\nafter-capture-pane\n",
            b"",
        ] {
            let stdout = [prelude, prefix.marker.as_bytes(), b"\n", &body].concat();
            let error = prefix.decode(Some(0), stdout, b"", false).unwrap_err();
            assert!(!error.to_string().contains("TEXT"));
            assert_eq!(
                matches!(error, Error::JoinedCaptureHook),
                prelude.starts_with(b"after-capture-pane")
            );
        }
    }
}
