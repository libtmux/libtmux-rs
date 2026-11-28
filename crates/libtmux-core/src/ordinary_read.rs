//! Ordinary subprocess observations without acquiring a control client.

use std::ffi::OsString;
use std::path::Path;

use crate::{
    CommandKind, CreationNonce, Error, ObservePane, PaneId, PaneKey, TmuxVersion, parse_number,
};

/// Reads the PID and whole-second start time of a running daemon.
///
/// The ordinary client uses a nonce-bound diagnostic from `run-shell -C`.
/// This avoids running the user's `after-display-message` hook while
/// discovering the generation of a handle.
///
/// # Examples
///
/// ```
/// use libtmux_core::{CreationNonce, OrdinaryGenerationProbe};
///
/// let probe = OrdinaryGenerationProbe::new(CreationNonce::from_bytes([0x42; 16]));
/// assert_eq!(probe.arguments()[0], "-N");
/// ```
pub struct OrdinaryGenerationProbe {
    marker: String,
}

impl std::fmt::Debug for OrdinaryGenerationProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OrdinaryGenerationProbe(<redacted>)")
    }
}

impl OrdinaryGenerationProbe {
    /// Prepare a request with fresh driver-supplied entropy.
    #[must_use]
    pub fn new(nonce: CreationNonce) -> Self {
        Self {
            marker: format!("libtmux-generation:{}:", nonce.encoded()),
        }
    }

    /// Encode a non-starting client for an already-running daemon.
    #[must_use]
    pub fn arguments(&self) -> Vec<OsString> {
        [
            "-N",
            "run-shell",
            "-C",
            &format!("{}#{{pid}},#{{start_time}}", self.marker),
        ]
        .map(OsString::from)
        .into()
    }

    /// Read exactly one matching diagnostic with two decimal fields.
    ///
    /// # Errors
    ///
    /// Returns an observation error on a late, interrupted, unrelated or malformed reply.
    pub fn decode(
        &self,
        status: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
        late: bool,
    ) -> Result<crate::ServerGeneration, Error> {
        if late {
            return Err(Error::LateObservation {
                operation: CommandKind::ServerGeneration,
            });
        }
        if status != Some(1) || !stdout.is_empty() {
            return Err(Error::FramedRead);
        }
        let (pid, started) = probe_report(stderr, &self.marker)
            .and_then(|report| report.split_once(','))
            .ok_or(Error::InvalidKey)?;
        let pid = parse_number::<u32>(pid)
            .filter(|pid| *pid > 0)
            .ok_or(Error::InvalidKey)?;
        let started = parse_number::<i64>(started)
            .filter(|seconds| crate::snapshot::unix_time(*seconds).is_some())
            .ok_or(Error::InvalidKey)?;
        Ok(crate::ServerGeneration::from_raw(pid, started))
    }
}

fn probe_report<'a>(stderr: &'a [u8], marker: &str) -> Option<&'a str> {
    let bytes = stderr
        .strip_suffix(b"\n")?
        .strip_prefix(b"unknown command: ")?
        .strip_prefix(marker.as_bytes())?;
    std::str::from_utf8(bytes).ok()
}

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
/// let nonce = CreationNonce::from_bytes([0x42; 16]);
/// let probe = OrdinaryPaneProbe::new("%7".parse()?, nonce);
/// let arguments = probe.arguments();
/// assert_eq!(arguments[1], "run-shell");
/// # Ok::<(), libtmux_core::IdParseError>(())
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
                "{}#{{pid}},#{{start_time}},#{{pane_id}},#{{pane_height}},#{{version}},#{{history_size}},#{{pane_dead}}",
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
        let report = probe_report(stderr, &self.marker).ok_or(Error::InvalidKey)?;
        let fields: Vec<_> = report.split(',').collect();
        let [pid, started, pane, height, version, history, dead] = fields.as_slice() else {
            return Err(Error::InvalidKey);
        };
        if parse_number::<u32>(pid).is_none_or(|pid| pid == 0)
            || parse_number::<u64>(started).is_none()
        {
            return Err(Error::InvalidKey);
        }
        let qualified = TmuxVersion::parse_output(format!("tmux {version}\n").as_bytes())?;
        qualified.ensure_supported()?;
        if [pane, height, history, dead]
            .iter()
            .all(|field| field.is_empty())
        {
            return Err(Error::PaneNotFound(self.pane.clone()));
        }
        if pane.parse::<PaneId>().ok().as_ref() != Some(&self.pane) {
            return Err(Error::InvalidKey);
        }
        let height = parse_number::<u32>(height)
            .filter(|height| *height > 0)
            .ok_or(Error::InvalidKey)?;
        let history = parse_number::<u32>(history).ok_or(Error::InvalidKey)?;
        let dead = match *dead {
            "0" => false,
            "1" => true,
            _ => return Err(Error::InvalidKey),
        };
        let key = ObservePane::decode_rows(
            socket,
            &self.pane,
            format!("{pid} {started} {pane}\n").as_bytes(),
        )?;
        Ok(OrdinaryPaneObservation {
            key,
            height,
            history,
            dead,
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
/// use libtmux_core::{CreationNonce, OrdinaryPaneProbe};
/// use std::path::Path;
///
/// let probe = OrdinaryPaneProbe::new("%7".parse()?, CreationNonce::from_bytes([0; 16]));
/// let report = b"unknown command: libtmux-pane:00000000000000000000000000000000:123,456,%7,24,3.4,0,0\n";
/// let observed = probe.decode(Path::new("/tmp/recorded.sock"), Some(1), b"", report, false)?;
/// assert_eq!(observed.key().id().as_ref(), "%7");
/// assert_eq!(observed.key().generation().pid(), 123);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
pub struct OrdinaryPaneObservation {
    key: PaneKey,
    height: u32,
    history: u32,
    dead: bool,
    version: String,
}

impl OrdinaryPaneObservation {
    /// The exact pane ID and generation reported by the probe.
    #[must_use]
    pub const fn key(&self) -> &PaneKey {
        &self.key
    }

    /// Whether the process was dead in the observed pane metadata.
    #[doc(hidden)]
    #[must_use]
    pub const fn pane_dead(&self) -> bool {
        self.dead
    }

    /// Encode a same-queue generation and height check followed by capture.
    ///
    /// # Errors
    ///
    /// Rejects another endpoint or a key that differs from this observation.
    pub fn capture_arguments(&self, socket: &Path, key: &PaneKey) -> Result<Vec<OsString>, Error> {
        self.capture_arguments_checked(socket, key, false, false)
    }

    /// Encode a capture guard that also checks the observed history extent.
    ///
    /// # Errors
    ///
    /// Rejects another endpoint or a key that differs from this observation.
    #[doc(hidden)]
    pub fn capture_range_arguments(
        &self,
        socket: &Path,
        key: &PaneKey,
    ) -> Result<Vec<OsString>, Error> {
        self.capture_arguments_checked(socket, key, true, false)
    }

    /// Encode a capture guard checking the observed process state.
    ///
    /// # Errors
    ///
    /// Rejects another endpoint or a key that differs from this observation.
    #[doc(hidden)]
    pub fn capture_look_arguments(
        &self,
        socket: &Path,
        key: &PaneKey,
    ) -> Result<Vec<OsString>, Error> {
        self.capture_arguments_checked(socket, key, false, true)
    }

    fn capture_arguments_checked(
        &self,
        socket: &Path,
        key: &PaneKey,
        check_history: bool,
        check_dead: bool,
    ) -> Result<Vec<OsString>, Error> {
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
        let extent = if check_dead {
            format!("#{{==:#{{pane_dead}},{}}}", u8::from(self.dead))
        } else {
            format!("#{{==:#{{pane_height}},{height}}}")
        };
        let predicate = format!(
            "#{{&&:#{{==:#{{pane_id}},{pane}}},#{{&&:#{{==:#{{pid}},{pid}}},#{{&&:#{{==:#{{start_time}},{started}}},{extent}}}}}}}"
        );
        let mut predicate = format!("#{{&&:{predicate},#{{==:#{{version}},{}}}}}", self.version);
        if check_history {
            predicate = format!(
                "#{{&&:{predicate},#{{==:#{{history_size}},{}}}}}",
                self.history
            );
        }
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
        stdout: Vec<u8>,
        stderr: &[u8],
        late: bool,
    ) -> Result<Vec<u8>, Error> {
        Self::decode_capture_rows(status, stdout, stderr, late, u64::from(self.height))
    }

    /// Isolate an unjoined capture using the same numeric bounds as tmux.
    ///
    /// The submitted range must match these bounds and its guard must check
    /// the history extent through `capture_range_arguments`.
    ///
    /// # Errors
    ///
    /// Rejects stale, failed, late or incomplete output without screen bytes.
    #[doc(hidden)]
    pub fn decode_capture_range(
        &self,
        status: Option<i32>,
        stdout: Vec<u8>,
        stderr: &[u8],
        late: bool,
        range: (Option<&str>, Option<&str>),
    ) -> Result<Vec<u8>, Error> {
        let top = self.capture_bound(range.0, true);
        let bottom = self.capture_bound(range.1, false);
        let rows = top.abs_diff(bottom) + 1;
        Self::decode_capture_rows(status, stdout, stderr, late, rows)
    }

    /// Decode a one-row `capture-pane -pPC` without its hook output.
    ///
    /// # Errors
    ///
    /// Rejects failed, late, truncated or invalid octal output without bytes.
    #[doc(hidden)]
    pub fn decode_pending_capture(
        &self,
        status: Option<i32>,
        stdout: Vec<u8>,
        stderr: &[u8],
        late: bool,
    ) -> Result<Vec<u8>, Error> {
        let wire = Self::decode_capture_rows(status, stdout, stderr, late, 1)?;
        let mut decoded = Vec::with_capacity(wire.len());
        let mut input = wire[..wire.len() - 1].iter().copied();
        while let Some(byte) = input.next() {
            if byte == b'\\' {
                let high = input.next().filter(|byte| (b'0'..=b'3').contains(byte));
                let middle = input.next().filter(|byte| (b'0'..=b'7').contains(byte));
                let low = input.next().filter(|byte| (b'0'..=b'7').contains(byte));
                let (Some(high), Some(middle), Some(low)) = (high, middle, low) else {
                    return Err(Error::FramedRead);
                };
                decoded.push((high - b'0') * 64 + (middle - b'0') * 8 + (low - b'0'));
            } else {
                decoded.push(byte);
            }
        }
        if !decoded.ends_with(b"\n") {
            decoded.push(b'\n');
        }
        Ok(decoded)
    }

    fn capture_bound(&self, bound: Option<&str>, start: bool) -> u64 {
        let history = u64::from(self.history);
        let last = history + u64::from(self.height) - 1;
        if bound == Some("-") {
            return if start { 0 } else { last };
        }
        // tmux accepts positive numeric bounds only through SHRT_MAX.
        let line = bound
            .and_then(|text| text.parse::<i32>().ok())
            .filter(|line| *line <= i32::from(i16::MAX));
        let Some(line) = line else {
            return if start { history } else { last };
        };
        (i64::from(self.history) + i64::from(line))
            .clamp(0, i64::from(self.history) + i64::from(self.height) - 1)
            .unsigned_abs()
    }

    fn decode_capture_rows(
        status: Option<i32>,
        mut stdout: Vec<u8>,
        stderr: &[u8],
        late: bool,
        rows: u64,
    ) -> Result<Vec<u8>, Error> {
        crate::ordinary_capture::check_capture_status(status, &stdout, stderr, late)?;
        let index = usize::try_from(rows - 1).map_err(|_| Error::FramedRead)?;
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
            "unknown command: libtmux-pane:12121212121212121212121212121212:123,456,%7,{height},3.3a,0,0\n"
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
    fn generation_requires_one_complete_nonce_bound_report() {
        let probe = OrdinaryGenerationProbe::new(CreationNonce::from_bytes([0x42; 16]));
        let report = |body: &str| format!("unknown command: {}{body}\n", probe.marker);
        let clean = report("123,456");
        assert_eq!(
            probe.decode(Some(1), b"", clean.as_bytes(), false).unwrap(),
            crate::ServerGeneration::from_raw(123, 456)
        );
        for bad in [
            "0,456",
            "1,+42",
            "9223372036854775807,1",
            "7,1 ",
            "7,1\n2,8",
            "7,1,2",
        ] {
            assert!(
                probe
                    .decode(Some(1), b"", report(bad).as_bytes(), false)
                    .is_err(),
                "{bad:?}"
            );
        }
        for (status, output, diagnostic, late) in [
            (Some(0), &b""[..], clean.as_bytes(), false),
            (Some(1), &b"hook"[..], clean.as_bytes(), false),
            (
                Some(1),
                &b""[..],
                b"unknown command: other:123,456\n",
                false,
            ),
            (Some(1), &b""[..], clean.as_bytes(), true),
        ] {
            assert!(probe.decode(status, output, diagnostic, late).is_err());
        }
    }

    #[test]
    fn metadata_is_accepted_only_from_the_exact_hook_free_probe() {
        let expected = "unknown command: libtmux-pane:12121212121212121212121212121212:123,456,%7,3,3.3a,0,0\n";
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
            "unknown command: libtmux-pane:99999999999999999999999999999999:123,456,%7,3,3.3a,0,0\n",
            "unknown command: libtmux-pane:12121212121212121212121212121212:123,456,%8,3,3.3a,0,0\n",
            "unknown command: libtmux-pane:12121212121212121212121212121212:123,456,%7,0,3.3a,0,0\n",
            "unknown command: libtmux-pane:12121212121212121212121212121212:123,456,%7,3,3.3a,0,0\nextra\n",
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
            b"unknown command: libtmux-pane:12121212121212121212121212121212:123,456,%7,3,3.3,0,0\n";
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
    fn an_exact_empty_pane_report_preserves_the_absent_target() {
        let report =
            b"unknown command: libtmux-pane:12121212121212121212121212121212:123,456,,,3.3a,,\n";
        assert!(matches!(
            probe().decode(Path::new("/example/socket"), Some(1), b"", report, false),
            Err(Error::PaneNotFound(pane)) if pane.to_string() == "%7"
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
        let refused = observation(3)
            .decode_capture(Some(1), Vec::new(), b"syntax error\n", false)
            .unwrap_err();
        assert!(matches!(
            refused,
            Error::Command {
                operation: CommandKind::CapturePane,
                status: Some(1),
                ..
            }
        ));
        assert_eq!(refused.effect(), crate::Effect::Observation);
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

    #[test]
    fn ordinary_ranges_follow_native_clamping_and_exclude_hooks() {
        let mut observed = observation(3);
        observed.history = 4;
        for (range, rows) in [
            ((None, None), 3),
            ((Some("-"), None), 7),
            ((Some("-2"), Some("1")), 4),
            ((Some("3"), Some("-3")), 6),
            ((Some("2147483647"), Some("99")), 3),
            ((Some("-2147483648"), Some("0")), 5),
            ((Some("0"), Some("0")), 1),
        ] {
            let expected = b"row\n".repeat(rows);
            let wire = [expected.as_slice(), b"private-hook\n"].concat();
            assert_eq!(
                observed
                    .decode_capture_range(Some(0), wire, b"", false, range)
                    .unwrap(),
                expected,
                "{range:?}"
            );
        }
        let guarded = observed
            .capture_range_arguments(Path::new("/example/socket"), observed.key())
            .unwrap();
        assert!(guarded[5].to_str().unwrap().contains("#{history_size},4"));
        let viewport = observed
            .capture_arguments(Path::new("/example/socket"), observed.key())
            .unwrap();
        assert!(!viewport[5].to_str().unwrap().contains("history_size"));
    }

    #[test]
    fn pending_octal_bytes_are_lossless_and_keep_native_final_newlines() {
        for (wire, expected) in [
            (
                b"\\033]part\\000after\\134slash\nprivate-hook\n".as_slice(),
                b"\x1b]part\0after\\slash\n".as_slice(),
            ),
            (b"\\377\\012\nprivate-hook\n", b"\xff\n"),
            (b"\nprivate-hook\n", b"\n"),
        ] {
            assert_eq!(
                observation(3)
                    .decode_pending_capture(Some(0), wire.to_vec(), b"", false)
                    .unwrap(),
                expected
            );
        }
        for wire in [
            b"\\12\n".as_slice(),
            b"\\400\n",
            b"\\0g0\n",
            b"missing newline",
        ] {
            assert!(
                observation(3)
                    .decode_pending_capture(Some(0), wire.to_vec(), b"", false)
                    .is_err()
            );
        }
    }
}
