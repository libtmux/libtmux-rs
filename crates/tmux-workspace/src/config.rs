//! Parsing tmuxp-style workspace YAML.

use std::fmt::Write as _;
use std::path::PathBuf;

use yaml_rust2::{Yaml, YamlLoader};

/// A workspace configuration failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The document was not valid YAML.
    #[error("workspace configuration is not valid YAML")]
    Yaml(#[from] yaml_rust2::ScanError),

    /// The document was empty, or held more than one workspace.
    #[error("expected exactly one workspace document, found {found}")]
    DocumentCount {
        /// How many documents the file held.
        found: usize,
    },

    /// A required key was absent or the wrong shape.
    #[error("workspace configuration is invalid: {reason}")]
    Invalid {
        /// What was wrong, in terms of the configuration's own vocabulary.
        reason: String,
    },
}

impl ConfigError {
    fn invalid(reason: impl Into<String>) -> Self {
        Self::Invalid {
            reason: reason.into(),
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
    pub shell_command_before: Vec<String>,
    /// Whether to keep pane commands out of the shell's history.
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
    /// A command to run instead of the window's default shell.
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
    pub shell_command_before: Vec<String>,
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
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PaneConfig {
    /// Commands to run in the pane once it exists.
    pub shell_commands: Vec<String>,
    /// Environment variables set for the process this pane starts.
    pub environment: Vec<(String, String)>,
    /// The pane's working directory.
    pub start_directory: Option<PathBuf>,
    /// Whether this pane should end up selected.
    pub focus: bool,
    /// Whether to press Enter after each command.
    ///
    /// tmuxp's `enter: false` types a command without running it, which is
    /// how a file leaves something ready for the user to review.
    pub enter: bool,
    /// Whether this pane's commands stay out of the shell's history.
    ///
    /// `None` inherits the window, then the workspace.
    pub suppress_history: Option<bool>,
    /// Keys this parser recognized on the pane but does not act on.
    pub unsupported_keys: Vec<String>,
}

impl Workspace {
    /// Parse one workspace from tmuxp-style YAML.
    ///
    /// This accepts the shape tmuxp uses for the parts a builder needs. It is
    /// deliberately not a full tmuxp implementation: unknown keys are ignored
    /// rather than rejected, so a richer tmuxp file still loads.
    ///
    /// # Errors
    ///
    /// Returns an error when the document is not valid YAML, does not hold
    /// exactly one workspace, or is missing `session_name`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tmux_workspace::Workspace;
    ///
    /// let workspace = Workspace::from_yaml(
    ///     "
    /// session_name: demo
    /// windows:
    ///   - window_name: editor
    ///     panes:
    ///       - echo one
    ///       - shell_command: echo two
    /// ",
    /// )?;
    ///
    /// assert_eq!(workspace.session_name, "demo");
    /// assert_eq!(workspace.windows.len(), 1);
    /// assert_eq!(workspace.windows[0].panes.len(), 2);
    /// # Ok::<(), tmux_workspace::ConfigError>(())
    /// ```
    pub fn from_yaml(source: &str) -> Result<Self, ConfigError> {
        let documents = YamlLoader::load_from_str(source)?;
        let [document] = documents.as_slice() else {
            return Err(ConfigError::DocumentCount {
                found: documents.len(),
            });
        };

        let session_name = document["session_name"]
            .as_str()
            .ok_or_else(|| ConfigError::invalid("session_name must be a string"))?
            .to_owned();

        let windows = match &document["windows"] {
            Yaml::BadValue | Yaml::Null => Vec::new(),
            Yaml::Array(entries) => entries
                .iter()
                .enumerate()
                .map(|(index, window)| WindowConfig::from_yaml(window, index))
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err(ConfigError::invalid("windows must be a list")),
        };

        Ok(Self {
            session_name,
            start_directory: optional_path(&document["start_directory"], "start_directory")?,
            environment: pairs(&document["environment"])?,
            options: pairs(&document["options"])?,
            global_options: pairs(&document["global_options"])?,
            shell_command_before: commands(&document["shell_command_before"])?,
            suppress_history: is_true(&document["suppress_history"], "suppress_history")?,
            windows,
            unsupported_keys: unsupported(document, SESSION_KEYS),
        })
    }
}

impl WindowConfig {
    fn from_yaml(value: &Yaml, index: usize) -> Result<Self, ConfigError> {
        let at = format!("windows[{index}]");
        if !matches!(value, Yaml::Hash(_)) {
            return Err(ConfigError::invalid(format!("{at} must be a mapping")));
        }
        let panes = match &value["panes"] {
            // A window with no panes still has the one tmux creates with it.
            Yaml::BadValue | Yaml::Null => vec![PaneConfig::default()],
            Yaml::Array(entries) => entries
                .iter()
                .enumerate()
                .map(|(pane, entry)| PaneConfig::from_yaml(entry, &at, pane))
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err(ConfigError::invalid(format!("{at}.panes must be a list"))),
        };

        Ok(Self {
            window_name: value["window_name"].as_str().map(ToOwned::to_owned),
            window_index: optional_index(&value["window_index"])?,
            window_shell: value["window_shell"].as_str().map(ToOwned::to_owned),
            environment: pairs(&value["environment"])?,
            layout: optional_text(&value["layout"], &format!("{at}.layout"))?,
            start_directory: optional_path(
                &value["start_directory"],
                &format!("{at}.start_directory"),
            )?,
            focus: is_true(&value["focus"], &format!("{at}.focus"))?,
            options: pairs(&value["options"])?,
            shell_command_before: commands(&value["shell_command_before"])?,
            suppress_history: optional_bool(
                &value["suppress_history"],
                &format!("{at}.suppress_history"),
            )?,
            unsupported_keys: unsupported(value, WINDOW_KEYS),
            panes: if panes.is_empty() {
                vec![PaneConfig::default()]
            } else {
                panes
            },
        })
    }
}

impl PaneConfig {
    fn from_yaml(value: &Yaml, window: &str, index: usize) -> Result<Self, ConfigError> {
        let at = format!("{window}.panes[{index}]");
        // tmuxp lets a pane be a bare command string.
        if let Some(command) = value.as_str() {
            return Ok(Self {
                shell_commands: vec![command.to_owned()],
                ..Self::default()
            });
        }

        if !matches!(value, Yaml::Hash(_)) {
            return Err(ConfigError::invalid(format!(
                "{at} must be a command string or a mapping"
            )));
        }

        Ok(Self {
            shell_commands: commands(&value["shell_command"])?,
            environment: pairs(&value["environment"])?,
            start_directory: optional_path(
                &value["start_directory"],
                &format!("{at}.start_directory"),
            )?,
            focus: is_true(&value["focus"], &format!("{at}.focus"))?,
            // tmuxp presses Enter unless a file says otherwise.
            enter: optional_bool(&value["enter"], &format!("{at}.enter"))?.unwrap_or(true),
            suppress_history: optional_bool(
                &value["suppress_history"],
                &format!("{at}.suppress_history"),
            )?,
            unsupported_keys: unsupported(value, PANE_KEYS),
        })
    }
}

/// Keys this parser understands on a window.
const WINDOW_KEYS: &[&str] = &[
    "window_name",
    "window_index",
    "window_shell",
    "environment",
    "layout",
    "start_directory",
    "focus",
    "options",
    "shell_command_before",
    "suppress_history",
    "panes",
];

/// Keys this parser understands on a pane.
const PANE_KEYS: &[&str] = &[
    "shell_command",
    "environment",
    "start_directory",
    "focus",
    "enter",
    "suppress_history",
];

/// Keys this parser understands at the workspace level.
const SESSION_KEYS: &[&str] = &[
    "session_name",
    "start_directory",
    "environment",
    "options",
    "global_options",
    "shell_command_before",
    "suppress_history",
    "windows",
];

/// Collect the keys present in a mapping that this parser does not act on.
fn unsupported(document: &Yaml, known: &[&str]) -> Vec<String> {
    let Yaml::Hash(entries) = document else {
        return Vec::new();
    };

    entries
        .keys()
        .filter_map(|key| key.as_str())
        .filter(|key| !known.contains(key))
        .map(ToOwned::to_owned)
        .collect()
}

/// Read a mapping of names to values, as `environment` and `options` use.
fn pairs(value: &Yaml) -> Result<Vec<(String, String)>, ConfigError> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(Vec::new()),
        Yaml::Hash(entries) => entries
            .iter()
            .map(|(key, value)| {
                let key = key
                    .as_str()
                    .ok_or_else(|| ConfigError::invalid("names must be strings"))?;
                // tmuxp writes option values as strings, numbers, or bools.
                let value = value.as_str().map(ToOwned::to_owned).or_else(|| {
                    value.as_i64().map(|number| number.to_string()).or_else(|| {
                        value.as_bool().map(|flag| {
                            if flag {
                                "on".to_owned()
                            } else {
                                "off".to_owned()
                            }
                        })
                    })
                });

                value
                    .map(|value| (key.to_owned(), value))
                    .ok_or_else(|| ConfigError::invalid("values must be scalars"))
            })
            .collect(),
        _ => Err(ConfigError::invalid(
            "expected a mapping of names to values",
        )),
    }
}

/// Read a value tmuxp allows as a string or a list of strings.
fn commands(value: &Yaml) -> Result<Vec<String>, ConfigError> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(Vec::new()),
        Yaml::String(command) => Ok(vec![command.clone()]),
        Yaml::Array(entries) => entries
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| ConfigError::invalid("shell_command entries must be strings"))
            })
            .collect(),
        _ => Err(ConfigError::invalid(
            "shell_command must be a string or a list of strings",
        )),
    }
}

/// Read a window index, which tmuxp writes as an integer or a string.
fn optional_index(value: &Yaml) -> Result<Option<i32>, ConfigError> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(None),
        Yaml::Integer(index) => i32::try_from(*index)
            .map(Some)
            .map_err(|_| ConfigError::invalid("window_index is out of range")),
        Yaml::String(index) => index
            .parse()
            .map(Some)
            .map_err(|_| ConfigError::invalid("window_index must be a number")),
        _ => Err(ConfigError::invalid("window_index must be a number")),
    }
}

/// Read an optional path, refusing a value that is present and not one.
///
/// Absence defaults; a wrong shape does not. `start_directory: 123` used to
/// read as "no start directory", which builds a workspace that is valid and
/// not the one the file describes.
fn optional_path(value: &Yaml, path: &str) -> Result<Option<PathBuf>, ConfigError> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(None),
        Yaml::String(text) => Ok(Some(PathBuf::from(text))),
        _ => Err(ConfigError::invalid(format!("{path} must be a string"))),
    }
}

/// Read an optional string, refusing a value that is present and not one.
fn optional_text(value: &Yaml, path: &str) -> Result<Option<String>, ConfigError> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(None),
        Yaml::String(text) => Ok(Some(text.clone())),
        _ => Err(ConfigError::invalid(format!("{path} must be a string"))),
    }
}

/// tmuxp writes booleans as bools in some files and strings in others.
///
/// Both spellings are accepted; a third thing is refused. `focus: "tru"` used
/// to read as `false`, which is a different workspace rather than an error.
fn optional_bool(value: &Yaml, path: &str) -> Result<Option<bool>, ConfigError> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(None),
        Yaml::Boolean(flag) => Ok(Some(*flag)),
        Yaml::String(text) => match text.as_str() {
            "true" | "yes" | "on" => Ok(Some(true)),
            "false" | "no" | "off" => Ok(Some(false)),
            _ => Err(ConfigError::invalid(format!(
                "{path} must be a boolean, found {text:?}"
            ))),
        },
        _ => Err(ConfigError::invalid(format!("{path} must be a boolean"))),
    }
}

/// Read a boolean that defaults to false when absent, and fails when wrong.
fn is_true(value: &Yaml, path: &str) -> Result<bool, ConfigError> {
    Ok(optional_bool(value, path)?.unwrap_or(false))
}

impl Workspace {
    /// Render this workspace as tmuxp-style YAML.
    ///
    /// Emits the keys this crate acts on and nothing else, so a document that
    /// came from [`Self::from_yaml`] and back may be shorter than it started:
    /// what is dropped is what `unsupported_keys` already named.
    ///
    /// # Examples
    ///
    /// ```
    /// use tmux_workspace::Workspace;
    ///
    /// let workspace = Workspace::from_yaml(
    ///     "
    /// session_name: demo
    /// windows:
    ///   - window_name: editor
    ///     panes: [htop]
    /// ",
    /// )?;
    ///
    /// // What it writes, it can read.
    /// assert_eq!(Workspace::from_yaml(&workspace.to_yaml())?, workspace);
    /// # Ok::<(), tmux_workspace::ConfigError>(())
    /// ```
    #[must_use]
    pub fn to_yaml(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "session_name: {}", quoted(&self.session_name));
        if let Some(directory) = &self.start_directory {
            let _ = writeln!(out, "start_directory: {}", path(directory));
        }
        if self.suppress_history {
            out.push_str("suppress_history: true\n");
        }
        write_pairs(&mut out, Some("environment"), &self.environment, 2);
        write_pairs(&mut out, Some("options"), &self.options, 2);
        write_pairs(&mut out, Some("global_options"), &self.global_options, 2);
        write_list(
            &mut out,
            Some("shell_command_before"),
            &self.shell_command_before,
            2,
        );

        out.push_str("windows:\n");
        for window in &self.windows {
            window.write_yaml(&mut out);
        }

        out
    }
}

/// Writes the keys of one sequence entry, indenting all but the first.
///
/// A YAML sequence entry marks only its first line with `-`, so which line
/// that is has to be tracked rather than decided per key.
struct Entry {
    marker: &'static str,
    indent: &'static str,
    first: bool,
}

impl Entry {
    const fn new(marker: &'static str, indent: &'static str) -> Self {
        Self {
            marker,
            indent,
            first: true,
        }
    }

    fn key(&mut self, out: &mut String, line: &str) {
        out.push_str(if self.first { self.marker } else { self.indent });
        self.first = false;
        out.push_str(line);
        out.push('\n');
    }
}

impl WindowConfig {
    /// Write this window as one entry of a `windows:` sequence.
    fn write_yaml(&self, out: &mut String) {
        let mut entry = Entry::new("  - ", "    ");

        if let Some(name) = &self.window_name {
            entry.key(out, &format!("window_name: {}", quoted(name)));
        }
        if let Some(index) = self.window_index {
            entry.key(out, &format!("window_index: {index}"));
        }
        if let Some(shell) = &self.window_shell {
            entry.key(out, &format!("window_shell: {}", quoted(shell)));
        }
        if let Some(layout) = &self.layout {
            entry.key(out, &format!("layout: {}", quoted(layout)));
        }
        if let Some(directory) = &self.start_directory {
            entry.key(out, &format!("start_directory: {}", path(directory)));
        }
        if self.focus {
            entry.key(out, "focus: true");
        }
        if let Some(suppress) = self.suppress_history {
            entry.key(out, &format!("suppress_history: {suppress}"));
        }
        if !self.environment.is_empty() {
            entry.key(out, "environment:");
            write_pairs(out, None, &self.environment, 6);
        }
        if !self.options.is_empty() {
            entry.key(out, "options:");
            write_pairs(out, None, &self.options, 6);
        }
        if !self.shell_command_before.is_empty() {
            entry.key(out, "shell_command_before:");
            write_list(out, None, &self.shell_command_before, 6);
        }

        // Always written, even when empty: an entry with no keys at all is
        // not a mapping, and `panes` is the one key every window has.
        entry.key(out, "panes:");
        for pane in &self.panes {
            pane.write_yaml(out);
        }
    }
}

impl PaneConfig {
    /// Write this pane as one entry of a `panes:` sequence.
    fn write_yaml(&self, out: &mut String) {
        let mut entry = Entry::new("      - ", "        ");

        if let [only] = self.shell_commands.as_slice() {
            entry.key(out, &format!("shell_command: {}", quoted(only)));
        } else if !self.shell_commands.is_empty() {
            entry.key(out, "shell_command:");
            write_list(out, None, &self.shell_commands, 10);
        }
        if let Some(directory) = &self.start_directory {
            entry.key(out, &format!("start_directory: {}", path(directory)));
        }
        if self.focus {
            entry.key(out, "focus: true");
        }
        if !self.enter {
            entry.key(out, "enter: false");
        }
        if let Some(suppress) = self.suppress_history {
            entry.key(out, &format!("suppress_history: {suppress}"));
        }
        if !self.environment.is_empty() {
            entry.key(out, "environment:");
            write_pairs(out, None, &self.environment, 10);
        }
        if entry.first {
            // Nothing distinguished this pane, so it is the empty mapping a
            // reader turns back into a default pane.
            out.push_str("      - {}\n");
        }
    }
}

/// Write a mapping of name to value, indented.
fn write_pairs(out: &mut String, name: Option<&str>, values: &[(String, String)], indent: usize) {
    if values.is_empty() {
        return;
    }
    if let Some(name) = name {
        let _ = writeln!(out, "{name}:");
    }
    for (key, value) in values {
        let _ = writeln!(out, "{:indent$}{}: {}", "", quoted(key), quoted(value));
    }
}

/// Write a sequence of strings, indented.
fn write_list(out: &mut String, name: Option<&str>, values: &[String], indent: usize) {
    if values.is_empty() {
        return;
    }
    if let Some(name) = name {
        let _ = writeln!(out, "{name}:");
    }
    for value in values {
        let _ = writeln!(out, "{:indent$}- {}", "", quoted(value));
    }
}

/// Quote a path the way a scalar is quoted.
fn path(value: &std::path::Path) -> String {
    quoted(&value.display().to_string())
}

/// Quote a scalar so YAML reads it back as the string it started as.
///
/// Always quoted rather than only when necessary: a command is arbitrary
/// shell, and deciding which of YAML's bare-scalar rules it trips is a larger
/// job than quoting everything.
fn quoted(value: &str) -> String {
    let escaped = value.replace('\\', r"\\").replace('"', r#"\""#);
    format!("\"{escaped}\"")
}
