//! Ordinary subprocess observations without acquiring a control client.

use std::ffi::OsString;
use std::path::Path;

use crate::{
    CommandKind, CreationNonce, Error, ObservePane, PaneId, PaneKey, TmuxVersion, parse_number,
};

/// A hook-free metadata request bound to a nonce chosen by the native driver.
///
/// The driver supplies fresh OS entropy for each request. The intentionally
/// unknown command is expanded and rejected by `run-shell -C`; it never
/// acquires a control client or invokes an observation's `after-` hook.
///
/// # Examples
///
/// ```
/// use libtmux_core::{CreationNonce, OrdinaryPaneProbe};
///
/// # fn example(nonce: CreationNonce) -> Result<(), libtmux_core::IdParseError> {
/// let probe = OrdinaryPaneProbe::new("%7".parse()?, nonce);
/// let arguments = probe.arguments();
/// assert_eq!(arguments[1], "run-shell");
/// # Ok(())
/// # }
/// ```
pub struct OrdinaryPaneProbe {
    pane: PaneId,
    marker: String,
}

impl std::fmt::Debug for OrdinaryPaneProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OrdinaryPaneProbe")
            .field("pane", &self.pane)
            .finish_non_exhaustive()
    }
}

impl OrdinaryPaneProbe {
    /// Prepare a probe using a fresh nonce supplied by the native driver.
    #[must_use]
    pub fn new(pane: PaneId, nonce: CreationNonce) -> Self {
        Self {
            pane,
            marker: format!("libtmux-pane:{}:", nonce.encoded()),
        }
    }

    /// Encode an ordinary, non-starting client with an exact pane target.
    #[must_use]
    pub fn arguments(&self) -> Vec<OsString> {
        [
            "-N",
            "run-shell",
            "-C",
            "-t",
            self.pane.as_ref(),
            &format!(
                "{}#{{pid}},#{{start_time}},#{{pane_id}},#{{pane_height}},#{{version}}",
                self.marker
            ),
        ]
        .map(OsString::from)
        .into()
    }

    /// Validate the exact diagnostic and qualify its pane generation and extent.
    ///
    /// # Errors
    ///
    /// Rejects late, malformed, unrelated or unsupported reports. Payload
    /// bytes and the nonce are never included in an error.
    pub fn decode(
        &self,
        socket: &Path,
        status: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
        late: bool,
    ) -> Result<OrdinaryPaneObservation, Error> {
        if late {
            return Err(Error::LateObservation {
                operation: CommandKind::ListPanes,
            });
        }
        if status != Some(1) || !stdout.is_empty() {
            return Err(Error::FramedRead);
        }
        let report = stderr
            .strip_suffix(b"\n")
            .and_then(|bytes| bytes.strip_prefix(b"unknown command: "))
            .and_then(|bytes| bytes.strip_prefix(self.marker.as_bytes()))
            .ok_or(Error::InvalidKey)?;
        let report = std::str::from_utf8(report).map_err(|_| Error::InvalidKey)?;
        let fields: Vec<_> = report.split(',').collect();
        let [pid, started, pane, height, version] = fields.as_slice() else {
            return Err(Error::InvalidKey);
        };
        if parse_number::<u32>(pid).is_none_or(|pid| pid == 0)
            || parse_number::<u64>(started).is_none()
            || pane.parse::<PaneId>().ok().as_ref() != Some(&self.pane)
        {
            return Err(Error::InvalidKey);
        }
        let height = parse_number::<u32>(height)
            .filter(|height| *height > 0)
            .ok_or(Error::InvalidKey)?;
        let qualified = TmuxVersion::parse_output(format!("tmux {version}\n").as_bytes())?;
        qualified.ensure_supported()?;
        let key = ObservePane::decode_rows(
            socket,
            &self.pane,
            format!("{pid} {started} {pane}\n").as_bytes(),
        )?;
        Ok(OrdinaryPaneObservation {
            key,
            height,
            version: (*version).to_owned(),
        })
    }
}

/// A pane generation and viewport extent verified by an ordinary metadata probe.
///
/// Capture checks the same endpoint, generation, version and height in its
/// daemon queue. Exactly the viewport's rows belong to the capture; later
/// hook output cannot become screen content.
///
/// # Examples
///
/// ```
/// use libtmux_core::OrdinaryPaneObservation;
///
/// # fn example(observed: &OrdinaryPaneObservation) {
/// let pane = observed.key().id();
/// # let _ = pane;
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct OrdinaryPaneObservation {
    key: PaneKey,
    height: u32,
    version: String,
}

impl OrdinaryPaneObservation {
    /// The exact pane ID and generation reported by the probe.
    #[must_use]
    pub const fn key(&self) -> &PaneKey {
        &self.key
    }

    /// Encode a same-queue generation and height check followed by capture.
    ///
    /// # Errors
    ///
    /// Rejects another endpoint or a key that differs from this observation.
    pub fn capture_arguments(&self, socket: &Path, key: &PaneKey) -> Result<Vec<OsString>, Error> {
        if socket != key.server_identity().socket_path() {
            return Err(Error::WrongEndpoint);
        }
        if key != &self.key {
            return Err(Error::StalePane);
        }
        let pane = key.id().to_string();
        let pid = key.generation().pid();
        let started = key.generation().unix_start_time();
        let height = self.height;
        let predicate = format!(
            "#{{&&:#{{==:#{{pane_id}},{pane}}},#{{&&:#{{==:#{{pid}},{pid}}},#{{&&:#{{==:#{{start_time}},{started}}},#{{==:#{{pane_height}},{height}}}}}}}}}"
        );
        let predicate = format!("#{{&&:{predicate},#{{==:#{{version}},{}}}}}", self.version);
        Ok([
            "-N",
            "if-shell",
            "-F",
            "-t",
            &pane,
            &predicate,
            "",
            "}",
            ";",
            "capture-pane",
            "-p",
            "-t",
            &pane,
        ]
        .map(OsString::from)
        .into())
    }

    /// Encode the same checked viewport read, optionally retaining ANSI escapes.
    ///
    /// # Errors
    ///
    /// Refuses the endpoint and generation mismatches checked by `capture_arguments`.
    #[doc(hidden)]
    pub fn capture_arguments_with_escapes(
        &self,
        socket: &Path,
        key: &PaneKey,
        escapes: bool,
    ) -> Result<Vec<OsString>, Error> {
        let mut arguments = self.capture_arguments(socket, key)?;
        if escapes {
            arguments.push(OsString::from("-e"));
        }
        Ok(arguments)
    }

    /// Return the viewport bytes only after a successful, timely collection.
    ///
    /// # Errors
    ///
    /// Rejects the stale branch, a failed hook, missing rows, client failure or
    /// deadline. A failed observation never returns a partial screen.
    pub fn decode_capture(
        &self,
        status: Option<i32>,
        mut stdout: Vec<u8>,
        stderr: &[u8],
        late: bool,
    ) -> Result<Vec<u8>, Error> {
        if status == Some(1) && stdout.is_empty() && stderr == b"syntax error\n" {
            return Err(Error::StalePane);
        }
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
        let index = usize::try_from(self.height - 1).map_err(|_| Error::FramedRead)?;
        let end = stdout
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'\n')
            .nth(index)
            .map(|(offset, _)| offset + 1)
            .ok_or(Error::FramedRead)?;
        stdout.truncate(end);
        Ok(stdout)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::NewSession;

    fn probe() -> OrdinaryPaneProbe {
        OrdinaryPaneProbe::new("%7".parse().unwrap(), CreationNonce::from_bytes([0x12; 16]))
    }

    fn observation(height: u32) -> OrdinaryPaneObservation {
        let diagnostic = format!(
            "unknown command: libtmux-pane:12121212121212121212121212121212:123,456,%7,{height},3.3a\n"
        );
        probe()
            .decode(
                Path::new("/example/socket"),
                Some(1),
                b"",
                diagnostic.as_bytes(),
                false,
            )
            .unwrap()
    }

    #[test]
    fn metadata_is_accepted_only_from_the_exact_hook_free_probe() {
        let expected =
            "unknown command: libtmux-pane:12121212121212121212121212121212:123,456,%7,3,3.3a\n";
        let observed = probe()
            .decode(
                Path::new("/example/socket"),
                Some(1),
                b"",
                expected.as_bytes(),
                false,
            )
            .unwrap();
        assert_eq!(observed.key().id().to_string(), "%7");
        assert_eq!(observed.key().generation().pid(), 123);
        for diagnostic in [
            "unknown command: libtmux-pane:99999999999999999999999999999999:123,456,%7,3,3.3a\n",
            "unknown command: libtmux-pane:12121212121212121212121212121212:123,456,%8,3,3.3a\n",
            "unknown command: libtmux-pane:12121212121212121212121212121212:123,456,%7,0,3.3a\n",
            "unknown command: libtmux-pane:12121212121212121212121212121212:123,456,%7,3,3.3a\nextra\n",
        ] {
            assert!(
                probe()
                    .decode(
                        Path::new("/example/socket"),
                        Some(1),
                        b"",
                        diagnostic.as_bytes(),
                        false
                    )
                    .is_err()
            );
        }
        assert!(
            probe()
                .decode(
                    Path::new("/example/socket"),
                    Some(1),
                    b"unexpected",
                    expected.as_bytes(),
                    false
                )
                .is_err()
        );
        assert!(
            probe()
                .decode(
                    Path::new("/example/socket"),
                    Some(0),
                    b"",
                    expected.as_bytes(),
                    false
                )
                .is_err()
        );
    }

    #[test]
    fn unsupported_versions_are_not_qualified_by_valid_metadata() {
        let diagnostic =
            b"unknown command: libtmux-pane:12121212121212121212121212121212:123,456,%7,3,3.3\n";
        assert!(matches!(
            probe().decode(
                Path::new("/example/socket"),
                Some(1),
                b"",
                diagnostic,
                false
            ),
            Err(Error::Version(error)) if matches!(*error, crate::VersionError::UnsupportedTmuxVersion { .. })
        ));
    }

    #[test]
    fn capture_keeps_exact_screen_rows_and_excludes_successful_hook_output() {
        let observation = observation(3);
        let screen = b"wrap one\ninvalid-\xff\n\n";
        let mut reply = screen.to_vec();
        reply.extend_from_slice(b"hook output\n%begin 1 2 0\n");
        assert_eq!(
            observation
                .decode_capture(Some(0), reply, b"", false)
                .unwrap(),
            screen
        );
    }

    #[test]
    fn capture_rejects_incomplete_rows_failed_hooks_and_late_data() {
        for (status, stdout, stderr, late) in [
            (Some(0), b"one\ntwo\n".as_slice(), b"".as_slice(), false),
            (
                Some(0),
                b"one\ntwo\nthree".as_slice(),
                b"".as_slice(),
                false,
            ),
            (
                Some(1),
                b"one\ntwo\nthree\n".as_slice(),
                b"failed hook\n".as_slice(),
                false,
            ),
            (
                Some(0),
                b"one\ntwo\nthree\n".as_slice(),
                b"".as_slice(),
                true,
            ),
        ] {
            assert!(
                observation(3)
                    .decode_capture(status, stdout.to_vec(), stderr, late)
                    .is_err()
            );
        }
        assert!(matches!(
            observation(3).decode_capture(Some(1), Vec::new(), b"syntax error\n", false),
            Err(Error::StalePane)
        ));
    }

    #[test]
    fn capture_refuses_a_key_from_another_endpoint_or_generation() {
        let observation = observation(3);
        let different_generation = NewSession::new("work")
            .decode(Path::new("/example/socket"), b"124 456 %7 $8 @9\n")
            .unwrap();
        assert!(matches!(
            observation
                .capture_arguments(Path::new("/example/socket"), different_generation.pane()),
            Err(Error::StalePane)
        ));
        assert!(matches!(
            observation.capture_arguments(Path::new("/other/socket"), observation.key()),
            Err(Error::WrongEndpoint)
        ));
    }
}
