use std::ffi::OsString;
use std::fmt;

use crate::{Operand, TmuxArg};

/// A request to create a detached session and its first window and pane.
///
/// Names and start directories are literal unless supplied through
/// [`TmuxArg::format`]. Environment values and the shell command are passed
/// unchanged and marked sensitive in the encoded operands. Construction and
/// encoding perform no I/O.
///
/// # Examples
///
/// ```
/// use libtmux_core::{NewSession, TmuxArg};
///
/// let request = NewSession::new("work#1")
///     .window_name("editor")
///     .start_directory(TmuxArg::format("#{pane_current_path}"))
///     .size(120, 40);
/// let operands = request.operands(NewSession::RECEIPT_FORMAT);
/// assert_eq!(operands[5].as_os_str(), "work##1");
/// ```
#[must_use = "a request has no effect until a transport submits it"]
#[derive(Clone, Eq, PartialEq)]
pub struct NewSession {
    pub(crate) name: TmuxArg,
    pub(crate) start_directory: Option<TmuxArg>,
    pub(crate) window_name: Option<TmuxArg>,
    pub(crate) command: Option<OsString>,
    pub(crate) environment: Vec<(OsString, OsString)>,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
}

impl NewSession {
    /// The creation projection containing daemon generation and all three IDs.
    pub const RECEIPT_FORMAT: &'static str =
        "#{pid} #{start_time} #{pane_id} #{session_id} #{window_id}";

    /// Describe a detached session with the given name.
    ///
    /// Ordinary text reaches tmux literally. [`TmuxArg::format`] opts into
    /// format expansion.
    pub fn new(name: impl Into<TmuxArg>) -> Self {
        Self {
            name: name.into(),
            start_directory: None,
            window_name: None,
            command: None,
            environment: Vec::new(),
            width: None,
            height: None,
        }
    }

    /// Set the first window's working directory.
    ///
    /// Ordinary text is literal; [`TmuxArg::format`] requests expansion.
    pub fn start_directory(mut self, directory: impl Into<TmuxArg>) -> Self {
        self.start_directory = Some(directory.into());
        self
    }

    /// Name the first window, with expansion only through [`TmuxArg::format`].
    pub fn window_name(mut self, name: impl Into<TmuxArg>) -> Self {
        self.window_name = Some(name.into());
        self
    }

    /// Run a sensitive shell command instead of the default shell.
    pub fn command(mut self, command: impl Into<OsString>) -> Self {
        self.command = Some(command.into());
        self
    }

    /// Set a sensitive environment assignment for the first pane's process.
    ///
    /// Repeated calls preserve order and duplicate names. tmux receives the
    /// value unchanged, without format expansion.
    pub fn environment(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((name.into(), value.into()));
        self
    }

    /// Set the initial width and height of the detached session.
    pub fn size(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    /// Encode logical operands for a caller-selected creation projection.
    ///
    /// The subcommand is `new-session`. All switches precede the optional
    /// shell command, which follows `--`. The values preserve operating-system
    /// strings; each transport must quote them once for its own protocol.
    #[must_use]
    pub fn operands(&self, print_format: &str) -> Vec<Operand> {
        let mut operands = vec![
            Operand::flag("-d"),
            Operand::flag("-P"),
            Operand::flag("-F"),
            Operand::plain(print_format),
            Operand::flag("-s"),
            Operand::plain(self.name.as_os_str()),
        ];
        if let Some(directory) = &self.start_directory {
            operands.push(Operand::flag("-c"));
            operands.push(Operand::plain(directory.as_os_str()));
        }
        if let Some(name) = &self.window_name {
            operands.push(Operand::flag("-n"));
            operands.push(Operand::plain(name.as_os_str()));
        }
        if let (Some(width), Some(height)) = (self.width, self.height) {
            operands.push(Operand::flag("-x"));
            operands.push(Operand::plain(width.to_string()));
            operands.push(Operand::flag("-y"));
            operands.push(Operand::plain(height.to_string()));
        }
        for (name, value) in &self.environment {
            let mut assignment = name.clone();
            assignment.push("=");
            assignment.push(value);
            operands.push(Operand::flag("-e"));
            operands.push(Operand::sensitive(assignment));
        }
        if let Some(command) = &self.command {
            operands.push(Operand::flag("--"));
            operands.push(Operand::sensitive(command));
        }
        operands
    }

    /// Encode argv for a one-shot control client using the receipt projection.
    ///
    /// Global endpoint and configuration arguments belong before this vector.
    /// Every operand is lowered so a literal trailing semicolon cannot create
    /// another tmux command.
    #[cfg(any(unix, windows))]
    #[must_use]
    pub fn arguments(&self) -> Vec<OsString> {
        let mut arguments = vec![OsString::from("-C"), OsString::from("new-session")];
        arguments.extend(
            self.operands(Self::RECEIPT_FORMAT)
                .iter()
                .map(|operand| crate::operand::argv_operand(operand.as_os_str())),
        );
        arguments
    }
}

impl fmt::Debug for NewSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewSession")
            .field("has_start_directory", &self.start_directory.is_some())
            .field("has_window_name", &self.window_name.is_some())
            .field("has_command", &self.command.is_some())
            .field("environment_count", &self.environment.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl<T: Into<OsString>> From<T> for NewSession {
    fn from(name: T) -> Self {
        Self::new(name.into())
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;
    use crate::{OperandKind, TmuxArg};

    #[test]
    fn session_operands_keep_options_before_the_sensitive_shell_command() {
        let request = NewSession::new("work#1")
            .start_directory("/home/#literal")
            .window_name("editor#1")
            .size(120, 40)
            .environment("TOKEN", "private#value")
            .environment("EMPTY", "")
            .command("printf '%s' secret;");
        let operands = request.operands("custom-format");
        let values: Vec<_> = operands.iter().map(Operand::as_os_str).collect();
        let expected: Vec<_> = [
            "-d",
            "-P",
            "-F",
            "custom-format",
            "-s",
            "work##1",
            "-c",
            "/home/##literal",
            "-n",
            "editor##1",
            "-x",
            "120",
            "-y",
            "40",
            "-e",
            "TOKEN=private#value",
            "-e",
            "EMPTY=",
            "--",
            "printf '%s' secret;",
        ]
        .map(OsStr::new)
        .into();
        assert_eq!(values, expected);
        for (index, operand) in operands.iter().enumerate() {
            assert_eq!(
                operand.kind(),
                match index {
                    15 | 17 | 19 => OperandKind::Sensitive,
                    3 | 5 | 7 | 9 | 11 | 13 => OperandKind::Plain,
                    _ => OperandKind::Flag,
                }
            );
        }
    }

    #[test]
    fn explicit_formats_survive_cloning_and_repeated_encoding() {
        let request = NewSession::new(TmuxArg::format("#{session_name}"))
            .start_directory(TmuxArg::format("#{pane_current_path}"))
            .window_name(TmuxArg::format("#(printf editor)"));
        let encoded = request.operands("#{session_id}");
        assert_eq!(encoded[5].as_os_str(), "#{session_name}");
        assert_eq!(encoded[7].as_os_str(), "#{pane_current_path}");
        assert_eq!(encoded[9].as_os_str(), "#(printf editor)");
        assert_eq!(request.clone().operands("#{session_id}"), encoded);
        assert_eq!(request.operands("#{session_id}"), encoded);
    }

    #[test]
    fn debug_hides_builder_values_and_sensitive_operands() {
        let request = NewSession::new("private-session")
            .start_directory("private-directory")
            .window_name("private-window")
            .environment("PRIVATE_TOKEN", "private-environment")
            .command("private-command");
        for diagnostic in [format!("{request:?}"), format!("{request:#?}")] {
            for secret in [
                "private-session",
                "private-directory",
                "private-window",
                "PRIVATE_TOKEN",
                "private-environment",
                "private-command",
            ] {
                assert!(!diagnostic.contains(secret));
            }
        }
        let operands = request.operands(NewSession::RECEIPT_FORMAT);
        let diagnostic = format!("{operands:?} {operands:#?}");
        for secret in ["PRIVATE_TOKEN", "private-environment", "private-command"] {
            assert!(!diagnostic.contains(secret));
        }
    }

    #[cfg(unix)]
    #[test]
    fn encoding_preserves_non_utf8_argument_and_environment_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let request = NewSession::new(OsString::from_vec(b"work\xff#".to_vec()))
            .environment(
                OsString::from_vec(b"N\xfe".to_vec()),
                OsString::from_vec(b"V\xfd#".to_vec()),
            )
            .command(OsString::from_vec(b"cmd\xfc;".to_vec()));
        let operands = request.operands("format");
        assert_eq!(operands[5].as_os_str().as_bytes(), b"work\xff##");
        assert_eq!(operands[7].as_os_str().as_bytes(), b"N\xfe=V\xfd#");
        assert_eq!(operands[9].as_os_str().as_bytes(), b"cmd\xfc;");
    }
}
