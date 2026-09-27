//! Parsing tmuxp-style workspace YAML.

mod emit;
mod expand;
mod keys;
mod locate;
mod merge;
mod parse;

use std::path::PathBuf;
use std::time::Duration;

/// A workspace configuration failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The workspace file could not be read.
    #[error("cannot read workspace file {}", path.display())]
    Read {
        /// The file that was asked for.
        path: PathBuf,
        /// Why it could not be read.
        source: std::io::Error,
    },

    /// The document was not valid YAML.
    #[error("workspace configuration is not valid YAML at line {line}, column {column}: {reason}")]
    Yaml {
        /// The line the parser stopped on, counting from 1.
        line: usize,
        /// The column the parser stopped on, counting from 1.
        column: usize,
        /// What the parser expected and did not find.
        reason: String,
    },

    /// The document was empty, or held more than one workspace.
    #[error("expected exactly one workspace document, found {found}")]
    DocumentCount {
        /// How many documents the file held.
        found: usize,
    },

    /// A required key was absent or the wrong shape.
    #[error("workspace configuration is invalid at line {line}, column {column}: {path} {reason}")]
    Invalid {
        /// Where, as a key path such as `windows[0].panes[1]`.
        path: String,
        /// The line of the offending value, counting from 1. A missing key
        /// is placed at the mapping that should have held it.
        line: usize,
        /// The column of the offending value, counting from 1.
        column: usize,
        /// What was wrong, in terms of the configuration's own vocabulary.
        reason: String,
    },
}

impl From<yaml_rust2::ScanError> for ConfigError {
    fn from(error: yaml_rust2::ScanError) -> Self {
        let mark = error.marker();
        Self::Yaml {
            line: mark.line(),
            // `Marker::col` counts from zero; `ScanError`'s own `Display` adds one.
            column: mark.col() + 1,
            reason: error.info().to_owned(),
        }
    }
}

/// A key path and what is wrong there, before the source is scanned for
/// where that path sits.
#[derive(Debug)]
struct Problem {
    path: String,
    reason: String,
}

impl Problem {
    fn new(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            reason: reason.into(),
        }
    }

    fn locate(self, source: &str) -> ConfigError {
        let (line, column) = locate::locate(source, &self.path);
        ConfigError::Invalid {
            path: self.path,
            line,
            column,
            reason: self.reason,
        }
    }
}

/// One workspace: a session and the windows it should contain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    /// The session name to create.
    pub session_name: String,
    /// A working directory inherited by windows that do not set their own.
    pub start_directory: Option<PathBuf>,
    /// Environment variables to set on the session.
    pub environment: Vec<(String, String)>,
    /// Session options to apply once the session exists.
    pub options: Vec<(String, String)>,
    /// Global options to apply once the session exists.
    pub global_options: Vec<(String, String)>,
    /// Commands run in every pane before its own, in order.
    pub shell_command_before: Vec<ShellCommand>,
    /// Whether to keep pane commands out of the shell's history.
    ///
    /// A file that does not say reads as `true`, as in tmuxp.
    pub suppress_history: bool,
    /// The windows to create, in order.
    pub windows: Vec<WindowConfig>,
    /// Keys this parser recognized but does not act on.
    ///
    /// Reported rather than dropped, so a caller can say what part of a file
    /// was ignored instead of leaving the difference to be discovered later.
    pub unsupported_keys: Vec<String>,
}

/// One window and the panes it should contain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowConfig {
    /// The window name, or `None` to let tmux choose.
    pub window_name: Option<String>,
    /// The window index to create at, or `None` for the first free one.
    pub window_index: Option<i32>,
    /// A command that replaces the shell in every pane of this window that
    /// does not set its own [`PaneConfig::shell`].
    pub window_shell: Option<String>,
    /// Environment variables set for the processes this window starts.
    pub environment: Vec<(String, String)>,
    /// A tmux layout name or specification applied after the panes exist.
    pub layout: Option<String>,
    /// A working directory inherited by panes that do not set their own.
    pub start_directory: Option<PathBuf>,
    /// Whether this window should end up selected.
    pub focus: bool,
    /// Window options to apply once the window exists.
    pub options: Vec<(String, String)>,
    /// Commands run in this window's panes before their own, in order.
    pub shell_command_before: Vec<ShellCommand>,
    /// Whether this window's commands stay out of the shell's history.
    ///
    /// `None` inherits the workspace setting.
    pub suppress_history: Option<bool>,
    /// The panes to create, in order. The first is the window's own pane.
    pub panes: Vec<PaneConfig>,
    /// Keys this parser recognized on the window but does not act on.
    pub unsupported_keys: Vec<String>,
}

/// One pane.
///
/// The default is a pane that runs nothing and would press Enter after
/// anything it were given, which is what tmuxp's `- pane`, `- blank` and an
/// empty `-` all mean.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneConfig {
    /// Commands run in this pane before [`Self::shell_commands`], in order,
    /// after the workspace's and the window's own
    /// [`WindowConfig::shell_command_before`].
    pub shell_command_before: Vec<ShellCommand>,
    /// Commands to run in the pane once it exists.
    pub shell_commands: Vec<ShellCommand>,
    /// Environment variables set for the process this pane starts.
    ///
    /// `None` inherits the window's [`WindowConfig::environment`] wholesale;
    /// `Some`, even an empty one, replaces it rather than adding to it, as
    /// tmuxp does.
    pub environment: Option<Vec<(String, String)>>,
    /// The pane's working directory.
    pub start_directory: Option<PathBuf>,
    /// Whether this pane should end up selected.
    pub focus: bool,
    /// A command that replaces this pane's shell, overriding
    /// [`WindowConfig::window_shell`] when both are given.
    pub shell: Option<String>,
    /// Whether to press Enter after each command, until a command sets its
    /// own [`ShellCommand::enter`].
    ///
    /// tmuxp's `enter: false` types a command without running it, which is
    /// how a file leaves something ready for the user to review. It covers
    /// the `shell_command_before` commands typed into this pane too.
    pub enter: bool,
    /// How long to wait before each command, until a command sets its own.
    pub sleep_before: Option<Duration>,
    /// How long to wait after each command, until a command sets its own.
    pub sleep_after: Option<Duration>,
    /// Whether this pane's commands stay out of the shell's history.
    ///
    /// `None` inherits the window, then the workspace.
    pub suppress_history: Option<bool>,
    /// Keys this parser recognized on the pane but does not act on.
    pub unsupported_keys: Vec<String>,
}

impl Default for PaneConfig {
    fn default() -> Self {
        Self {
            shell_command_before: Vec::new(),
            shell_commands: Vec::new(),
            environment: None,
            start_directory: None,
            focus: false,
            shell: None,
            enter: true,
            sleep_before: None,
            sleep_after: None,
            suppress_history: None,
            unsupported_keys: Vec::new(),
        }
    }
}

/// One command typed into a pane, with tmuxp's per-command settings.
///
/// A file writes it as a string, or as a mapping with `cmd` and any of
/// `enter`, `sleep_before` and `sleep_after`. A setting given here holds for
/// the commands after it in the same pane until one of them sets its own,
/// because that is what tmuxp does: `enter: false` on one command leaves the
/// next one unentered too.
///
/// # Examples
///
/// ```
/// use tmux_workspace::{ShellCommand, Workspace};
///
/// let workspace = Workspace::from_yaml(
///     "
/// session_name: demo
/// windows:
///   - panes:
///       - shell_command:
///           - cd src
///           - cmd: cargo test
///             enter: false
/// ",
/// )?;
///
/// let commands = &workspace.windows[0].panes[0].shell_commands;
/// assert_eq!(commands[0], ShellCommand::new("cd src"));
/// assert_eq!(commands[1].cmd, "cargo test");
/// assert_eq!(commands[1].enter, Some(false));
/// # Ok::<(), tmux_workspace::ConfigError>(())
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShellCommand {
    /// The text typed into the pane.
    pub cmd: String,
    /// Whether to press Enter after it. `None` keeps whatever is in force.
    pub enter: Option<bool>,
    /// How long to wait before typing it. `None` keeps whatever is in force.
    ///
    /// The wait is a [`libtmux::plan::Pause`], so it happens in tmux and
    /// holds the steps after it. tmux keeps typed input until the pane reads
    /// it, so a sleep that only waited for a shell to start is not needed.
    pub sleep_before: Option<Duration>,
    /// How long to wait after typing it. `None` keeps whatever is in force.
    pub sleep_after: Option<Duration>,
}

impl ShellCommand {
    /// A command with no settings of its own.
    #[must_use]
    pub fn new(cmd: impl Into<String>) -> Self {
        Self {
            cmd: cmd.into(),
            ..Self::default()
        }
    }

    /// Whether this is the bare string form, with no settings of its own.
    const fn is_plain(&self) -> bool {
        self.enter.is_none() && self.sleep_before.is_none() && self.sleep_after.is_none()
    }
}
