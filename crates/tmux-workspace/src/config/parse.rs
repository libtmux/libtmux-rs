//! Turning a YAML document into a [`Workspace`], enforcing the shape tmuxp
//! files have and this crate's own strictness policy.

use std::path::Path;

use yaml_rust2::{Yaml, YamlLoader};

use super::expand::{Directories, expand};
use super::keys::{
    PANE_KEYS, SESSION_KEYS, SESSION_RECOGNIZED, Strictness, WINDOW_KEYS, WINDOW_RECOGNIZED,
    check_known, has_key, unsupported,
};
use super::merge::resolve_merges;
use super::values::{
    commands, is_true, optional_bool, optional_index, optional_seconds, optional_text, pairs,
};
use super::{ConfigError, PaneConfig, Problem, WindowConfig, Workspace};

impl Workspace {
    /// Parse one workspace from tmuxp-style YAML.
    ///
    /// This accepts the shape tmuxp uses for the parts a builder needs. A key
    /// this parser does not act on is recorded rather than rejected, so a
    /// richer tmuxp file still loads; [`Self::from_yaml_strict`] refuses one
    /// instead.
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
        Self::parse(source, None, Strictness::Lenient)
    }

    /// Parse as [`Self::from_yaml`] does, refusing a key that is neither this
    /// parser's own vocabulary nor tmuxp's, unless it starts with `x-`.
    ///
    /// This is the policy the `tmux-workspace` command applies to every
    /// document it loads.
    ///
    /// # Errors
    ///
    /// What [`Self::from_yaml`] returns, and also an unrecognized key.
    ///
    /// # Examples
    ///
    /// ```
    /// use tmux_workspace::Workspace;
    ///
    /// let error = Workspace::from_yaml_strict(
    ///     "session_name: demo\nwindows: []\nfrobnicate: 1\n",
    /// )
    /// .expect_err("an unrecognized key is refused");
    /// assert!(error.to_string().contains("frobnicate"));
    /// ```
    pub fn from_yaml_strict(source: &str) -> Result<Self, ConfigError> {
        Self::parse(source, None, Strictness::Strict)
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
    /// let yaml = "session_name: project\nstart_directory: ./\nwindows: []\n";
    /// std::fs::write(&file, yaml)?;
    ///
    /// let workspace = Workspace::from_file(&file)?;
    /// assert_eq!(workspace.start_directory.as_deref(), Some(project.path()));
    /// # Ok(())
    /// # }
    /// ```
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        Self::read_file(path, Strictness::Lenient)
    }

    /// Read as [`Self::from_file`] does, under [`Self::from_yaml_strict`]'s
    /// policy.
    ///
    /// # Errors
    ///
    /// What [`Self::from_file`] returns, and also an unrecognized key.
    pub fn from_file_strict(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        Self::read_file(path, Strictness::Strict)
    }

    fn read_file(path: impl AsRef<Path>, strictness: Strictness) -> Result<Self, ConfigError> {
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
        Self::parse(&source, directory.as_deref(), strictness)
    }

    fn parse(
        source: &str,
        base: Option<&Path>,
        strictness: Strictness,
    ) -> Result<Self, ConfigError> {
        let documents = YamlLoader::load_from_str(source)?;
        let [document] = documents.as_slice() else {
            return Err(ConfigError::DocumentCount {
                found: documents.len(),
            });
        };
        let document = resolve_merges(document);
        Self::from_document(&document, &Directories { base }, strictness)
            .map_err(|problem| problem.locate(source))
    }

    fn from_document(
        document: &Yaml,
        directories: &Directories<'_>,
        strictness: Strictness,
    ) -> Result<Self, Problem> {
        check_known(document, SESSION_KEYS, SESSION_RECOGNIZED, strictness, "")?;
        let session_name = document["session_name"]
            .as_str()
            .ok_or_else(|| Problem::new("session_name", "must be a string"))?;
        let session_name = expand(session_name, "session_name")?;
        let start_directory =
            directories.resolve(&document["start_directory"], "start_directory", None, false)?;
        if let Some(separator) = session_name.chars().find(|c| matches!(c, ':' | '.')) {
            return Err(Problem::new(
                "session_name",
                format!("must not contain {separator:?}; tmux reads it as a target separator"),
            ));
        }

        // tmuxp refuses a document missing `windows`; a null one would crash
        // its builder the same way, so both are refused here too. An empty
        // list is a list, so it keeps the window tmux made, as tmuxp does.
        let windows = match &document["windows"] {
            Yaml::Array(entries) => entries
                .iter()
                .enumerate()
                .map(|(index, window)| {
                    WindowConfig::from_yaml(
                        window,
                        index,
                        directories,
                        start_directory.as_deref(),
                        strictness,
                    )
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
        strictness: Strictness,
    ) -> Result<Self, Problem> {
        let at = format!("windows[{index}]");
        if !matches!(value, Yaml::Hash(_)) {
            return Err(Problem::new(at, "must be a mapping"));
        }
        check_known(value, WINDOW_KEYS, WINDOW_RECOGNIZED, strictness, &at)?;
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
                    PaneConfig::from_yaml(entry, &at, pane, directories, inherited, strictness)
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
        strictness: Strictness,
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
        check_known(value, PANE_KEYS, &[], strictness, &at)?;

        Ok(Self {
            shell_command_before: commands(
                &value["shell_command_before"],
                &format!("{at}.shell_command_before"),
            )?,
            shell_commands: commands(&value["shell_command"], &format!("{at}.shell_command"))?,
            // tmuxp's builder replaces the window's environment with the
            // pane's own when the pane sets the key at all, even to an empty
            // mapping, rather than adding to it; presence is what decides it.
            environment: has_key(value, "environment")
                .then(|| pairs(&value["environment"], &format!("{at}.environment")))
                .transpose()?,
            // tmuxp does not join a pane's relative directory onto the
            // window's; only a `.` path starts from it.
            start_directory: directories.resolve(
                &value["start_directory"],
                &format!("{at}.start_directory"),
                inherited,
                false,
            )?,
            focus: is_true(&value["focus"], &format!("{at}.focus"))?,
            // tmuxp's builder resolves a pane's own shell before falling
            // back to the window's, the same way it resolves environment.
            shell: value["shell"].as_str().map(ToOwned::to_owned),
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

#[cfg(test)]
mod tests {
    use super::{Directories, PaneConfig, Strictness};
    use yaml_rust2::YamlLoader;

    /// The value a single-document YAML fragment parses to, for feeding
    /// these functions the same shapes tmuxp's parser hands them.
    fn value(source: &str) -> yaml_rust2::Yaml {
        YamlLoader::load_from_str(source).unwrap().remove(0)
    }

    /// tmuxp's builder replaces the window's environment with the pane's own
    /// when the pane sets the key at all, even to an empty mapping, rather
    /// than adding to it: presence, not emptiness, is what decides it.
    #[test]
    fn pane_environment_distinguishes_absent_from_explicitly_set() {
        let directories = Directories { base: None };
        let pane = |source| {
            PaneConfig::from_yaml(
                &value(source),
                "windows[0]",
                0,
                &directories,
                None,
                Strictness::Lenient,
            )
        };

        assert_eq!(pane("{}").unwrap().environment, None);
        assert_eq!(
            pane("{environment: {}}").unwrap().environment,
            Some(Vec::new())
        );
        assert_eq!(
            pane("{environment: {FOO: bar}}").unwrap().environment,
            Some(vec![("FOO".into(), "bar".into())]),
        );
    }
}
