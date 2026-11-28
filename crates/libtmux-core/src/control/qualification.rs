use std::ffi::OsString;

use crate::{
    Command, CreationNonce, Error, ServerGeneration, SessionId, TmuxVersion, parse_number,
};

/// The version, generation and exact session reported by one pinned endpoint.
///
/// This is metadata, not a protocol-compatibility witness. A driver must also
/// complete a nonce challenge on the attached connection before exposing it.
///
/// # Examples
///
/// ```
/// use libtmux_core::{CreationNonce, ServerGeneration, control::ControlProbe};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let probe = ControlProbe::new("$0".parse()?, CreationNonce::from_bytes([0x10; 16]));
/// let report = concat!(
///     "unknown command: libtmux-control-challenge:",
///     "10101010101010101010101010101010:3.8,123,456,$0\n",
/// );
/// let identity = probe.decode_ordinary(Some(1), b"", report.as_bytes(), false)?;
/// assert_eq!(identity.session().to_string(), "$0");
/// assert_eq!(identity.generation(), ServerGeneration::from_raw(123, 456));
/// assert_eq!(identity.version().raw(), "3.8");
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct ControlIdentity {
    version: TmuxVersion,
    generation: ServerGeneration,
    session: SessionId,
}

impl PartialEq for ControlIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.version.raw() == other.version.raw()
            && self.generation == other.generation
            && self.session == other.session
    }
}

impl Eq for ControlIdentity {}

impl ControlIdentity {
    /// The exact checked daemon version report.
    #[must_use]
    pub const fn version(&self) -> &TmuxVersion {
        &self.version
    }

    /// The daemon's PID and whole-second start time.
    #[must_use]
    pub const fn generation(&self) -> ServerGeneration {
        self.generation
    }

    /// The session selected by the exact probe target.
    #[must_use]
    pub const fn session(&self) -> &SessionId {
        &self.session
    }
}

/// A fresh-nonce metadata challenge for ordinary and attached control clients.
///
/// `run-shell -C` parses a deliberately unknown formatted command. The
/// resulting diagnostic runs no shell, after-hook or command-error hook.
/// A native driver supplies fresh OS entropy for every challenge.
///
/// # Examples
///
/// ```
/// use libtmux_core::{CreationNonce, control::ControlProbe};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let probe = ControlProbe::new("$0".parse()?, CreationNonce::from_bytes([0x10; 16]));
/// assert_eq!(probe.arguments()[0], "-N");
/// let report = concat!(
///     "unknown command: libtmux-control-challenge:",
///     "10101010101010101010101010101010:3.8,123,456,$0\n",
/// );
/// assert!(probe.decode_control_report(report.as_bytes()).is_ok());
/// assert!(probe.decode_control_report(b"unknown command: another-nonce\n").is_err());
/// # Ok(())
/// # }
/// ```
pub struct ControlProbe {
    session: SessionId,
    marker: String,
}

impl std::fmt::Debug for ControlProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlProbe")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl ControlProbe {
    /// Bind an exact session to fresh driver-supplied OS entropy.
    #[must_use]
    pub fn new(session: SessionId, nonce: CreationNonce) -> Self {
        Self {
            session,
            marker: format!("libtmux-control-challenge:{}:", nonce.encoded()),
        }
    }

    /// Encode the logical challenge for an already attached connection.
    pub fn command(&self) -> Command {
        Command::new("run-shell")
            .arg("-C")
            .arg("-t")
            .arg(format!("{}:", self.session))
            .arg(format!(
                "{}#{{version}},#{{pid}},#{{start_time}},#{{session_id}}",
                self.marker
            ))
    }

    /// Encode a non-starting ordinary client for the same metadata challenge.
    #[must_use]
    pub fn arguments(&self) -> Vec<OsString> {
        let mut arguments = vec![OsString::from("-N")];
        self.command().extend_argv(&mut arguments);
        arguments
    }

    /// Verify an ordinary probe without treating a failed probe as cold.
    ///
    /// # Errors
    ///
    /// Rejects unexpected status, output, deadline or nonce diagnostic. No
    /// report below the managed-control version floor qualifies attachment.
    pub fn decode_ordinary(
        &self,
        status: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
        late: bool,
    ) -> Result<ControlIdentity, Error> {
        if status != Some(1) || !stdout.is_empty() || late {
            return Err(Error::VersionProbeFailed {
                status,
                diagnostic_len: stderr.len(),
                late,
            });
        }
        self.decode_control_report(stderr)
    }

    /// Decode exactly this nonce's report from the attached connection.
    ///
    /// # Errors
    ///
    /// Rejects extra records, malformed fields, another session and versions
    /// lacking managed acquisition. The diagnostic bytes are never retained.
    pub fn decode_control_report(&self, bytes: &[u8]) -> Result<ControlIdentity, Error> {
        let invalid = || Error::VersionProbeFailed {
            status: Some(1),
            diagnostic_len: bytes.len(),
            late: false,
        };
        let report = bytes
            .strip_suffix(b"\n")
            .and_then(|bytes| bytes.strip_prefix(b"unknown command: "))
            .and_then(|bytes| bytes.strip_prefix(self.marker.as_bytes()))
            .ok_or_else(invalid)?;
        let report = std::str::from_utf8(report).map_err(|_| invalid())?;
        let fields: Vec<_> = report.split(',').collect();
        let [version, pid, started, session] = fields.as_slice() else {
            return Err(invalid());
        };
        let version = TmuxVersion::parse_output(format!("tmux {version}\n").as_bytes())?;
        version.ensure_supported()?;
        version.require(
            "managed control acquisition",
            crate::since::CONTROL_ACQUISITION,
        )?;
        let pid = parse_number::<u32>(pid)
            .filter(|pid| *pid > 0)
            .ok_or_else(invalid)?;
        let started = parse_number::<i64>(started).ok_or_else(invalid)?;
        let session: SessionId = session.parse().map_err(|_| invalid())?;
        if session != self.session {
            return Err(invalid());
        }
        Ok(ControlIdentity {
            version,
            generation: ServerGeneration::from_raw(pid, started),
            session,
        })
    }

    pub(super) fn matches_report_prefix(&self, bytes: &[u8]) -> bool {
        bytes
            .strip_prefix(b"unknown command: ")
            .is_some_and(|bytes| bytes.starts_with(self.marker.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_complete_nonce_bound_managed_identity_qualifies() {
        let probe = ControlProbe::new("$2".parse().unwrap(), CreationNonce::from_bytes([0xab; 16]));
        let report = b"unknown command: libtmux-control-challenge:abababababababababababababababab:3.8,123,456,$2\n";
        let identity = probe.decode_ordinary(Some(1), b"", report, false).unwrap();
        assert_eq!(identity.generation(), ServerGeneration::from_raw(123, 456));
        assert_eq!(identity.session().to_string(), "$2");
        for report in [
            b"unknown command: libtmux-control-challenge:cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd:3.8,123,456,$2\n".as_slice(),
            b"unknown command: libtmux-control-challenge:abababababababababababababababab:3.8,123,456,$3\n",
            b"unknown command: libtmux-control-challenge:abababababababababababababababab:3.8,123,456,$2\nextra\n",
            b"unknown command: libtmux-control-challenge:abababababababababababababababab:3.4,123,456,$2\n",
        ] { assert!(probe.decode_ordinary(Some(1), b"", report, false).is_err()); }
        assert!(probe.decode_ordinary(Some(0), b"", report, false).is_err());
        assert!(probe.decode_ordinary(Some(1), b"", report, true).is_err());
    }
}
