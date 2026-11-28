use std::ffi::{OsStr, OsString};
use std::fmt;

/// The role of one logical command argument before a transport encodes it.
///
/// # Examples
///
/// ```
/// use libtmux_core::{NewSession, OperandKind};
///
/// let operands = NewSession::new("work").command("private-command").operands("format");
/// assert_eq!(operands.last().map(|operand| operand.kind()), Some(OperandKind::Sensitive));
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OperandKind {
    /// A switch authored by the operation, including the `--` terminator.
    Flag,
    /// An argument that may appear in a command diagnostic.
    Plain,
    /// A payload that command diagnostics must redact.
    Sensitive,
}

/// A lossless logical argument with its diagnostic sensitivity.
///
/// Values have not been quoted for a command line or lowered for process argv.
/// A transport must encode each value once and honour [`OperandKind::Sensitive`].
/// Debug output redacts sensitive values.
///
/// # Examples
///
/// ```
/// use libtmux_core::NewSession;
///
/// let operands = NewSession::new("work").command("secret").operands("format");
/// assert_eq!(operands.last().map(|operand| operand.as_os_str()), Some(std::ffi::OsStr::new("secret")));
/// assert!(!format!("{operands:?}").contains("secret"));
/// ```
#[derive(Clone, Eq, PartialEq)]
pub struct Operand {
    kind: OperandKind,
    value: OsString,
}

impl Operand {
    pub(crate) fn flag(value: &'static str) -> Self {
        Self {
            kind: OperandKind::Flag,
            value: value.into(),
        }
    }

    pub(crate) fn plain(value: impl Into<OsString>) -> Self {
        Self {
            kind: OperandKind::Plain,
            value: value.into(),
        }
    }

    pub(crate) fn sensitive(value: impl Into<OsString>) -> Self {
        Self {
            kind: OperandKind::Sensitive,
            value: value.into(),
        }
    }

    /// The role and diagnostic sensitivity of this argument.
    #[must_use]
    pub const fn kind(&self) -> OperandKind {
        self.kind
    }

    /// Borrow the exact logical value before transport quoting.
    #[must_use]
    pub fn as_os_str(&self) -> &OsStr {
        &self.value
    }

    /// Take the exact logical value before transport quoting.
    #[must_use]
    pub fn into_os_string(self) -> OsString {
        self.value
    }

    /// Take the classification and value together for a transport renderer.
    #[must_use]
    pub fn into_parts(self) -> (OperandKind, OsString) {
        (self.kind, self.value)
    }
}

impl fmt::Debug for Operand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut output = formatter.debug_struct("Operand");
        output.field("kind", &self.kind);
        if self.kind == OperandKind::Sensitive {
            output.field("value", &"<redacted>");
        } else {
            output.field("value", &self.value);
        }
        output.finish()
    }
}

/// Lower one logical argument so a trailing semicolon cannot split commands.
#[cfg(any(unix, windows))]
#[must_use]
pub fn argv_operand(value: &OsStr) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let bytes = value.as_bytes();
        if let Some(prefix) = bytes.strip_suffix(b";") {
            let mut lowered = Vec::with_capacity(bytes.len() + 1);
            lowered.extend_from_slice(prefix);
            lowered.extend_from_slice(b"\\;");
            return OsString::from_vec(lowered);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        let mut units: Vec<_> = value.encode_wide().collect();
        if units.last() == Some(&u16::from(b';')) {
            units.pop();
            units.extend([u16::from(b'\\'), u16::from(b';')]);
            return OsString::from_wide(&units);
        }
    }
    value.to_owned()
}
