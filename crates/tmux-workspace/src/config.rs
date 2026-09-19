//! Parsing tmuxp-style workspace YAML.

mod locate;

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use yaml_rust2::{Yaml, YamlLoader};

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
    /// Commands to run in the pane once it exists.
    pub shell_commands: Vec<ShellCommand>,
    /// Environment variables set for the process this pane starts.
    pub environment: Vec<(String, String)>,
    /// The pane's working directory.
    pub start_directory: Option<PathBuf>,
    /// Whether this pane should end up selected.
    pub focus: bool,
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
            shell_commands: Vec::new(),
            environment: Vec::new(),
            start_directory: None,
            focus: false,
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

impl Workspace {
    /// Parse one workspace from tmuxp-style YAML.
    ///
    /// This accepts the shape tmuxp uses for the parts a builder needs. It is
    /// deliberately not a full tmuxp implementation: unknown keys are ignored
    /// rather than rejected, so a richer tmuxp file still loads.
    ///
    /// As tmuxp does, it expands `~` and `$NAME` or `${NAME}` from this
    /// process's environment in names, start directories, and `environment`
    /// and option values, leaving an unset variable as written; commands are
    /// typed as written, for the pane's shell to expand. A start directory
    /// that begins with `.` is relative to the one it inherits, or to the
    /// current directory at the top: [`Self::from_file`] uses the file's
    /// directory instead.
    ///
    /// # Errors
    ///
    /// Returns an error when the document is not valid YAML, does not hold
    /// exactly one workspace, or is missing `session_name`. Every error
    /// except the document count names the line and column to look at.
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
        Self::parse(source, None)
    }

    /// Read and parse one workspace file, as `tmuxp load` does.
    ///
    /// Everything [`Self::from_yaml`] says holds, except that a start
    /// directory beginning with `.` and inheriting none is relative to the
    /// file's directory. JSON is read too, as the YAML it is a subset of.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Read`] when the file cannot be read, and
    /// otherwise what [`Self::from_yaml`] returns.
    ///
    /// # Examples
    ///
    /// ```
    /// use tmux_workspace::Workspace;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let project = tempfile::tempdir()?;
    /// let file = project.path().join(".tmuxp.yaml");
    /// std::fs::write(&file, "session_name: project\nstart_directory: ./\n")?;
    ///
    /// let workspace = Workspace::from_file(&file)?;
    /// assert_eq!(workspace.start_directory.as_deref(), Some(project.path()));
    /// # Ok(())
    /// # }
    /// ```
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let read = |source| ConfigError::Read {
            path: path.to_owned(),
            source,
        };
        let source = std::fs::read_to_string(path).map_err(read)?;
        let directory = std::path::absolute(path)
            .map_err(read)?
            .parent()
            .map(Path::to_path_buf);
        Self::parse(&source, directory.as_deref())
    }

    fn parse(source: &str, base: Option<&Path>) -> Result<Self, ConfigError> {
        let documents = YamlLoader::load_from_str(source)?;
        let [document] = documents.as_slice() else {
            return Err(ConfigError::DocumentCount {
                found: documents.len(),
            });
        };
        Self::from_document(document, &Directories { base })
            .map_err(|problem| problem.locate(source))
    }

    fn from_document(document: &Yaml, directories: &Directories<'_>) -> Result<Self, Problem> {
        let session_name = document["session_name"]
            .as_str()
            .ok_or_else(|| Problem::new("session_name", "must be a string"))?;
        let session_name = expand(session_name, "session_name")?;
        let start_directory =
            directories.resolve(&document["start_directory"], "start_directory", None, false)?;

        let windows = match &document["windows"] {
            Yaml::BadValue | Yaml::Null => Vec::new(),
            Yaml::Array(entries) => entries
                .iter()
                .enumerate()
                .map(|(index, window)| {
                    WindowConfig::from_yaml(window, index, directories, start_directory.as_deref())
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err(Problem::new("windows", "must be a list")),
        };

        Ok(Self {
            session_name,
            start_directory,
            environment: pairs(&document["environment"], "environment")?,
            options: pairs(&document["options"], "options")?,
            global_options: pairs(&document["global_options"], "global_options")?,
            shell_command_before: commands(
                &document["shell_command_before"],
                "shell_command_before",
            )?,
            // tmuxp suppresses unless a file says otherwise.
            suppress_history: optional_bool(&document["suppress_history"], "suppress_history")?
                .unwrap_or(true),
            windows,
            unsupported_keys: unsupported(document, SESSION_KEYS),
        })
    }
}

impl WindowConfig {
    fn from_yaml(
        value: &Yaml,
        index: usize,
        directories: &Directories<'_>,
        session: Option<&Path>,
    ) -> Result<Self, Problem> {
        let at = format!("windows[{index}]");
        if !matches!(value, Yaml::Hash(_)) {
            return Err(Problem::new(at, "must be a mapping"));
        }
        // tmuxp joins a window's relative directory onto the session's.
        let start_directory = directories.resolve(
            &value["start_directory"],
            &format!("{at}.start_directory"),
            session,
            true,
        )?;
        let inherited = start_directory.as_deref().or(session);
        let panes = match &value["panes"] {
            // A window with no panes still has the one tmux creates with it.
            Yaml::BadValue | Yaml::Null => vec![PaneConfig::default()],
            Yaml::Array(entries) => entries
                .iter()
                .enumerate()
                .map(|(pane, entry)| {
                    PaneConfig::from_yaml(entry, &at, pane, directories, inherited)
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err(Problem::new(format!("{at}.panes"), "must be a list")),
        };
        let window_name = value["window_name"]
            .as_str()
            .map(|name| expand(name, &format!("{at}.window_name")))
            .transpose()?;

        Ok(Self {
            window_name,
            window_index: optional_index(&value["window_index"], &format!("{at}.window_index"))?,
            window_shell: value["window_shell"].as_str().map(ToOwned::to_owned),
            environment: pairs(&value["environment"], &format!("{at}.environment"))?,
            layout: optional_text(&value["layout"], &format!("{at}.layout"))?,
            start_directory,
            focus: is_true(&value["focus"], &format!("{at}.focus"))?,
            options: pairs(&value["options"], &format!("{at}.options"))?,
            shell_command_before: commands(
                &value["shell_command_before"],
                &format!("{at}.shell_command_before"),
            )?,
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
    fn from_yaml(
        value: &Yaml,
        window: &str,
        index: usize,
        directories: &Directories<'_>,
        inherited: Option<&Path>,
    ) -> Result<Self, Problem> {
        let at = format!("{window}.panes[{index}]");
        match value {
            // tmuxp lets a pane be its commands alone, or nothing at all.
            Yaml::Null | Yaml::String(_) | Yaml::Array(_) => {
                return Ok(Self {
                    shell_commands: commands(value, &at)?,
                    ..Self::default()
                });
            }
            Yaml::Hash(_) => {}
            _ => {
                return Err(Problem::new(
                    at,
                    "must be a command, a list of commands, or a mapping; \
                     quote a command YAML would read as a number or a boolean",
                ));
            }
        }

        Ok(Self {
            shell_commands: commands(&value["shell_command"], &format!("{at}.shell_command"))?,
            environment: pairs(&value["environment"], &format!("{at}.environment"))?,
            // tmuxp does not join a pane's relative directory onto the
            // window's; only a `.` path starts from it.
            start_directory: directories.resolve(
                &value["start_directory"],
                &format!("{at}.start_directory"),
                inherited,
                false,
            )?,
            focus: is_true(&value["focus"], &format!("{at}.focus"))?,
            // tmuxp presses Enter unless a file says otherwise.
            enter: optional_bool(&value["enter"], &format!("{at}.enter"))?.unwrap_or(true),
            sleep_before: optional_seconds(&value["sleep_before"], &format!("{at}.sleep_before"))?,
            sleep_after: optional_seconds(&value["sleep_after"], &format!("{at}.sleep_after"))?,
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
    "sleep_before",
    "sleep_after",
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
fn pairs(value: &Yaml, path: &str) -> Result<Vec<(String, String)>, Problem> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(Vec::new()),
        Yaml::Hash(entries) => entries
            .iter()
            .map(|(key, value)| {
                let key = key
                    .as_str()
                    .ok_or_else(|| Problem::new(path, "names must be strings"))?;
                let at = format!("{path}.{key}");
                // tmuxp writes option values as strings, numbers, or bools,
                // and expands only the strings.
                let value = match value {
                    Yaml::String(text) => expand(text, &at)?,
                    Yaml::Integer(number) => number.to_string(),
                    Yaml::Boolean(true) => "on".to_owned(),
                    Yaml::Boolean(false) => "off".to_owned(),
                    _ => {
                        return Err(Problem::new(at, "must be a string, a number, or a boolean"));
                    }
                };
                Ok((key.to_owned(), value))
            })
            .collect(),
        _ => Err(Problem::new(path, "must be a mapping of names to values")),
    }
}

/// Read `shell_command` or `shell_command_before`: one command, a list of
/// them, or nothing.
fn commands(value: &Yaml, path: &str) -> Result<Vec<ShellCommand>, Problem> {
    let (entries, single) = match value {
        Yaml::BadValue | Yaml::Null => return Ok(Vec::new()),
        Yaml::Array(entries) => (entries.as_slice(), false),
        _ => (std::slice::from_ref(value), true),
    };
    // tmuxp reads a lone null, `pane` or `blank` as a pane with no command.
    // Among other commands the words are typed, and a null is refused.
    if let [only] = entries {
        if matches!(only, Yaml::Null) || matches!(only.as_str(), Some("pane" | "blank")) {
            return Ok(Vec::new());
        }
    }
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let at = if single {
                path.to_owned()
            } else {
                format!("{path}[{index}]")
            };
            command(entry, &at)
        })
        .collect()
}

fn command(value: &Yaml, path: &str) -> Result<ShellCommand, Problem> {
    match value {
        Yaml::String(text) => Ok(ShellCommand::new(text.as_str())),
        Yaml::Hash(_) => Ok(ShellCommand {
            cmd: value["cmd"]
                .as_str()
                .ok_or_else(|| Problem::new(format!("{path}.cmd"), "must be a string"))?
                .to_owned(),
            enter: optional_bool(&value["enter"], &format!("{path}.enter"))?,
            sleep_before: optional_seconds(
                &value["sleep_before"],
                &format!("{path}.sleep_before"),
            )?,
            sleep_after: optional_seconds(&value["sleep_after"], &format!("{path}.sleep_after"))?,
        }),
        Yaml::Null => Err(Problem::new(
            path,
            "is empty among other commands; remove it, or write \"\" to press Enter",
        )),
        _ => Err(Problem::new(
            path,
            "must be a command or a mapping with `cmd`; \
             quote a command YAML would read as a number or a boolean",
        )),
    }
}

/// Read a number of seconds, which tmuxp passes to `time.sleep`.
fn optional_seconds(value: &Yaml, path: &str) -> Result<Option<Duration>, Problem> {
    let refused = || Problem::new(path, "must be a number of seconds, zero or more");
    match value {
        Yaml::BadValue | Yaml::Null => Ok(None),
        Yaml::Integer(seconds) => u64::try_from(*seconds)
            .map(|seconds| Some(Duration::from_secs(seconds)))
            .map_err(|_| refused()),
        Yaml::Real(_) => value
            .as_f64()
            .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
            .map(Some)
            .ok_or_else(refused),
        _ => Err(refused()),
    }
}

/// Read a window index, which tmuxp writes as an integer or a string.
fn optional_index(value: &Yaml, path: &str) -> Result<Option<i32>, Problem> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(None),
        Yaml::Integer(index) => i32::try_from(*index)
            .map(Some)
            .map_err(|_| Problem::new(path, "is out of range")),
        Yaml::String(index) => index
            .parse()
            .map(Some)
            .map_err(|_| Problem::new(path, "must be a number")),
        _ => Err(Problem::new(path, "must be a number")),
    }
}

/// Where a workspace's relative start directories are resolved from.
struct Directories<'a> {
    /// The workspace file's directory, or `None` for the current directory.
    base: Option<&'a Path>,
}

impl Directories<'_> {
    /// Read a `start_directory` and resolve it the way tmuxp's loader does.
    ///
    /// `~` and variables expand first. An absolute result stands. A result
    /// starting with `.` is relative to `parent`, else to the base. Any other
    /// relative result joins `parent` when `join` is set, which tmuxp does for
    /// a window under its session, and is otherwise relative to the current
    /// directory, where tmux would resolve it.
    ///
    /// Absence defaults; a wrong shape does not. `start_directory: 123` used
    /// to read as "no start directory", which builds a workspace that is valid
    /// and not the one the file describes.
    fn resolve(
        &self,
        value: &Yaml,
        path: &str,
        parent: Option<&Path>,
        join: bool,
    ) -> Result<Option<PathBuf>, Problem> {
        let text = match value {
            Yaml::BadValue | Yaml::Null => return Ok(None),
            Yaml::String(text) => text,
            _ => return Err(Problem::new(path, "must be a string")),
        };
        if text.starts_with('~') && !(text == "~" || text.starts_with("~/")) {
            return Err(Problem::new(
                path,
                "starts with `~name`, which is not expanded here; write the directory out",
            ));
        }
        let expanded = PathBuf::from(expand(text, path)?);
        if expanded.is_absolute() {
            return Ok(Some(tidy(&expanded)));
        }
        let anchor = if text.starts_with('.') {
            parent.or(self.base)
        } else if join {
            parent
        } else {
            None
        };
        let anchor = match anchor {
            Some(anchor) => anchor.to_owned(),
            None => std::env::current_dir().map_err(|error| {
                Problem::new(
                    path,
                    format!("is relative, and the current directory cannot be read: {error}"),
                )
            })?,
        };
        Ok(Some(tidy(&anchor.join(expanded))))
    }
}

/// Drop `.` components and doubled separators. `..` is kept for the kernel
/// to resolve, since a lexical `..` is wrong across a symbolic link.
fn tidy(path: &Path) -> PathBuf {
    path.components().collect()
}

/// Expand `text` against this process's environment, as tmuxp's
/// `expandshell` does.
fn expand(text: &str, path: &str) -> Result<String, Problem> {
    expand_with(text, |name| std::env::var_os(name)).map_err(|reason| Problem::new(path, reason))
}

/// Python's `os.path.expanduser` then `os.path.expandvars`, which is what
/// tmuxp applies.
///
/// A leading `~` or `~/` becomes `$HOME`. `$NAME` (ASCII letters, digits and
/// `_`) and `${NAME}` become the variable's value; an unset variable, `~name`
/// and a lone `$` stay as written. There is no escape, in tmuxp or here.
fn expand_with(text: &str, variable: impl Fn(&str) -> Option<OsString>) -> Result<String, String> {
    let text_of = |name: &str, value: OsString| {
        value
            .into_string()
            .map_err(|_| format!("names ${name}, whose value is not UTF-8"))
    };
    let mut expanded = String::with_capacity(text.len());
    let mut rest = text;
    if let Some(tail) = text.strip_prefix('~') {
        if tail.is_empty() || tail.starts_with('/') {
            let home = variable("HOME").ok_or("starts with `~`, and HOME is not set")?;
            expanded.push_str(text_of("HOME", home)?.trim_end_matches('/'));
            if expanded.is_empty() && tail.is_empty() {
                expanded.push('/');
            }
            rest = tail;
        }
    }
    while let Some(at) = rest.find('$') {
        expanded.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let (name, length) = if let Some(braced) = after.strip_prefix('{') {
            braced
                .find('}')
                .map_or(("", 0), |end| (&braced[..end], end + 2))
        } else {
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            (&after[..end], end)
        };
        // A name no environment variable can have is never looked up.
        let value = if name.is_empty() || name.contains(['=', '\0']) {
            None
        } else {
            variable(name)
        };
        match value {
            Some(value) => expanded.push_str(&text_of(name, value)?),
            None => expanded.push_str(&rest[at..=at + length]),
        }
        rest = &after[length..];
    }
    expanded.push_str(rest);
    Ok(expanded)
}

/// Read an optional string, refusing a value that is present and not one.
fn optional_text(value: &Yaml, path: &str) -> Result<Option<String>, Problem> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(None),
        Yaml::String(text) => Ok(Some(text.clone())),
        _ => Err(Problem::new(path, "must be a string")),
    }
}

/// tmuxp writes booleans as bools in some files and strings in others.
///
/// Both spellings are accepted; a third thing is refused. `focus: "tru"` used
/// to read as `false`, which is a different workspace rather than an error.
fn optional_bool(value: &Yaml, path: &str) -> Result<Option<bool>, Problem> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(None),
        Yaml::Boolean(flag) => Ok(Some(*flag)),
        Yaml::String(text) => match text.as_str() {
            "true" | "yes" | "on" => Ok(Some(true)),
            "false" | "no" | "off" => Ok(Some(false)),
            _ => Err(Problem::new(
                path,
                format!("must be a boolean, found {text:?}"),
            )),
        },
        _ => Err(Problem::new(path, "must be a boolean")),
    }
}

/// Read a boolean that defaults to false when absent, and fails when wrong.
fn is_true(value: &Yaml, path: &str) -> Result<bool, Problem> {
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
        if !self.suppress_history {
            out.push_str("suppress_history: false\n");
        }
        write_pairs(&mut out, Some("environment"), &self.environment, 2);
        write_pairs(&mut out, Some("options"), &self.options, 2);
        write_pairs(&mut out, Some("global_options"), &self.global_options, 2);
        write_commands(
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
            write_commands(out, None, &self.shell_command_before, 6);
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
            entry.key(out, &format!("shell_command: {}", command_yaml(only)));
        } else if !self.shell_commands.is_empty() {
            entry.key(out, "shell_command:");
            write_commands(out, None, &self.shell_commands, 10);
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
        if let Some(sleep) = self.sleep_before {
            entry.key(out, &format!("sleep_before: {}", sleep.as_secs_f64()));
        }
        if let Some(sleep) = self.sleep_after {
            entry.key(out, &format!("sleep_after: {}", sleep.as_secs_f64()));
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

/// Write a sequence of commands, indented.
fn write_commands(out: &mut String, name: Option<&str>, commands: &[ShellCommand], indent: usize) {
    if commands.is_empty() {
        return;
    }
    if let Some(name) = name {
        let _ = writeln!(out, "{name}:");
    }
    for command in commands {
        let _ = writeln!(out, "{:indent$}- {}", "", command_yaml(command));
    }
}

/// A command as a string, or as a flow mapping when it has settings of its own.
fn command_yaml(command: &ShellCommand) -> String {
    if command.is_plain() {
        return quoted(&command.cmd);
    }
    let mut fields = vec![format!("cmd: {}", quoted(&command.cmd))];
    if let Some(enter) = command.enter {
        fields.push(format!("enter: {enter}"));
    }
    if let Some(sleep) = command.sleep_before {
        fields.push(format!("sleep_before: {}", sleep.as_secs_f64()));
    }
    if let Some(sleep) = command.sleep_after {
        fields.push(format!("sleep_after: {}", sleep.as_secs_f64()));
    }
    format!("{{{}}}", fields.join(", "))
}

/// Quote a path the way a scalar is quoted.
fn path(value: &Path) -> String {
    quoted(&value.display().to_string())
}

/// Quote a scalar so YAML reads it back as the string it started as.
///
/// Always quoted rather than only when necessary: a command is arbitrary
/// shell, and deciding which of YAML's bare-scalar rules it trips is a larger
/// job than quoting everything.
fn quoted(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str(r#"\""#),
            '\\' => escaped.push_str(r"\\"),
            character if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') => {
                let code = u32::from(character);
                let _ = write!(escaped, r"\u{code:04x}");
            }
            _ => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
mod tests {
    use super::expand_with;

    /// Each expected value is what Python's `os.path.expandvars(
    /// os.path.expanduser(text))` returns with the same two variables set.
    #[test]
    fn expansion_is_pythons_expanduser_then_expandvars() {
        let expand = |text| {
            expand_with(text, |name| match name {
                "HOME" => Some("/home/me/".into()),
                "PROJECT" => Some("tmux".into()),
                _ => None,
            })
        };
        for (text, expected) in [
            ("~", "/home/me"),
            ("~/src", "/home/me/src"),
            ("~nosuchuser/src", "~nosuchuser/src"),
            ("a~", "a~"),
            ("$PROJECT/x", "tmux/x"),
            ("${PROJECT}x", "tmuxx"),
            ("$PROJECTx", "$PROJECTx"),
            ("$UNSET and ${UNSET}", "$UNSET and ${UNSET}"),
            ("$ ${ ${} $-", "$ ${ ${} $-"),
            ("~/$PROJECT", "/home/me/tmux"),
            ("price: $5", "price: $5"),
        ] {
            assert_eq!(expand(text).as_deref(), Ok(expected), "{text}");
        }

        let root = |text| expand_with(text, |_| Some("/".into()));
        assert_eq!(root("~").as_deref(), Ok("/"));
        assert_eq!(root("~/x").as_deref(), Ok("/x"));
        assert!(expand_with("~", |_| None).is_err(), "no HOME, no `~`");
    }
}
