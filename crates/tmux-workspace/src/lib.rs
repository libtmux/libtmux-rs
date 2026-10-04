#![doc = include_str!("../docs/library.md")]
#![forbid(unsafe_code)]

mod config;
mod freeze;

pub use config::{ConfigError, PaneConfig, ShellCommand, WindowConfig, Workspace};
pub use freeze::freeze;

use std::path::Path;

use libtmux::plan::{
    KillWindow, NewSession, NewWindow, PaneSlot, Pause, Plan, Planner, SelectLayout, SelectPane,
    SelectWindow, SendKeys, SessionSlot, SetEnvironment, SetOption, Slot, SplitWindow, WindowSlot,
};
use libtmux::{Server, Session, SessionId};

/// A failure while building a workspace.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// The workspace configuration could not be read.
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// tmux refused an operation, or could not be reached.
    #[error(transparent)]
    Tmux(#[from] libtmux::Error),

    /// tmux created a session without the window it always creates.
    ///
    /// This cannot be reported as a libtmux error: its variants are
    /// `#[non_exhaustive]`, so only that crate constructs them. A consumer
    /// describes its own failures in its own vocabulary.
    #[error("session {name} was created without its initial window")]
    MissingInitialWindow {
        /// The session that was created.
        name: String,
    },

    /// tmux refused a step of the build.
    #[error("building session {name} was refused: {detail}")]
    Refused {
        /// The session being built.
        name: String,
        /// What tmux said, or that it said nothing.
        detail: String,
    },

    /// A session with the requested name already exists.
    ///
    /// Building into an existing session would interleave windows with
    /// whatever is already there, so it is refused rather than guessed at.
    #[error("a session named {name} already exists")]
    SessionExists {
        /// The name that was already taken.
        name: String,
    },
}

/// Creates tmux sessions from workspace configurations.
#[derive(Debug)]
pub struct WorkspaceBuilder<'server> {
    server: &'server Server,
}

impl<'server> WorkspaceBuilder<'server> {
    /// Build workspaces on one server.
    #[must_use]
    pub const fn new(server: &'server Server) -> Self {
        Self { server }
    }

    /// Describe what building this workspace would do, without doing it.
    ///
    /// The returned plan is inert, so a caller can render it, count what it
    /// costs, or explain it before anything reaches tmux. Every object a later
    /// step addresses is a forward reference to the step that makes it, so the
    /// whole file lowers without a single round trip to look an id up.
    ///
    /// # Examples
    ///
    /// ```
    /// use tmux_workspace::{Workspace, WorkspaceBuilder};
    ///
    /// let workspace = Workspace::from_yaml(
    ///     "
    /// session_name: dev
    /// windows:
    ///   - window_name: editor
    ///     panes:
    ///       - vim
    /// ",
    /// )?;
    ///
    /// let server = libtmux::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/plan-example.sock")
    ///     .build()?;
    /// let plan = WorkspaceBuilder::new(&server).plan(&workspace);
    ///
    /// // Nothing has run, but the first command is already known.
    /// assert!(plan.preview()[0].as_ref().is_some_and(|command| {
    ///     command.summary().to_string().contains("new-session")
    /// }));
    /// # Ok::<(), tmux_workspace::BuildError>(())
    /// ```
    #[must_use]
    pub fn plan(&self, workspace: &Workspace) -> Plan {
        let mut plan = Plan::new();
        let session = plan.add(Self::session_op(workspace));

        // Environment and options land before any pane runs a command, so a
        // command sees the environment the file describes rather than the one
        // it happened to start in.
        for (name, value) in &workspace.environment {
            plan.add(SetEnvironment::new(session, name.as_str(), value.as_str()));
        }
        for (name, value) in &workspace.options {
            plan.add(SetOption::session(session, name.as_str(), value.as_str()));
        }
        for (name, value) in &workspace.global_options {
            plan.add(SetOption::global(name.as_str(), value.as_str()));
        }

        let mut focus_window = None;
        for config in &workspace.windows {
            let directory = config
                .start_directory
                .as_deref()
                .or(workspace.start_directory.as_deref());
            let first_pane = config.panes.first();
            let first_directory = first_pane
                .and_then(|pane| pane.start_directory.as_deref())
                .or(directory);
            let pane_environments = Self::pane_environments(workspace, config);
            let window = plan.add(Self::window_op(
                session,
                config,
                pane_environments.first().map_or(&[], Vec::as_slice),
                first_directory,
                first_pane.and_then(|pane| pane.shell.as_deref()),
            ));
            for (name, value) in &config.options {
                plan.add(SetOption::window(window, name.as_str(), value.as_str()));
            }

            // A new window arrives holding exactly one pane, so the count is
            // known rather than looked up: the first configured pane is that
            // one, and the rest are splits.
            //
            // Each split targets the pane the previous one made
            // (`SplitWindow::from_pane`): `-t <window>` always divides
            // the active pane, which a detached split never changes.
            let mut source = window.pane();
            let mut panes = vec![source];
            for (pane, environment) in config.panes.iter().zip(&pane_environments).skip(1) {
                source = Self::split_op(
                    &mut plan,
                    window,
                    source,
                    config,
                    directory,
                    pane,
                    environment,
                );
                panes.push(source);
            }

            // Layout is applied once the pane count is final, or tmux would
            // rebalance it away on the next split.
            if let Some(layout) = config.layout.as_deref().filter(|layout| !layout.is_empty()) {
                plan.add(SelectLayout::new(window, layout));
            }

            let mut focus_pane = None;
            for (pane, pane_config) in panes.iter().zip(&config.panes) {
                // Narrowest wins: pane, then window, then workspace.
                let suppress = pane_config
                    .suppress_history
                    .or(config.suppress_history)
                    .unwrap_or(workspace.suppress_history);

                // As in tmuxp, the `shell_command_before` commands lead the
                // pane's own list, and a command's `enter` and sleeps hold
                // for the rest.
                let mut enter = pane_config.enter;
                let mut sleep_before = pane_config.sleep_before;
                let mut sleep_after = pane_config.sleep_after;
                let commands = workspace
                    .shell_command_before
                    .iter()
                    .chain(&config.shell_command_before)
                    .chain(&pane_config.shell_command_before)
                    .chain(&pane_config.shell_commands);
                for command in commands {
                    enter = command.enter.unwrap_or(enter);
                    sleep_before = command.sleep_before.or(sleep_before);
                    sleep_after = command.sleep_after.or(sleep_after);
                    if let Some(duration) = sleep_before {
                        plan.add(Pause::new(duration));
                    }
                    plan.add(Self::typing(*pane, &command.cmd, suppress, enter));
                    if let Some(duration) = sleep_after {
                        plan.add(Pause::new(duration));
                    }
                }
                if pane_config.focus {
                    focus_pane = Some(*pane);
                }
            }

            if let Some(pane) = focus_pane {
                plan.add(SelectPane::new(pane));
            }
            if config.focus {
                focus_window = Some(window);
            }
        }

        // Killed last: a session with no windows is a session tmux destroys,
        // so this only happens once the configured windows exist. A workspace
        // that names no windows keeps the one tmux made.
        if !workspace.windows.is_empty() {
            plan.add(KillWindow::new(session.window()));
        }
        if let Some(window) = focus_window {
            plan.add(SelectWindow::new(window));
        }

        plan
    }

    /// Create the session a workspace describes, and return it.
    ///
    /// Windows and panes are created in the order the configuration lists
    /// them. Nothing is attached: the caller decides whether to take over a
    /// terminal.
    ///
    /// # Errors
    ///
    /// Returns an error when a session of the same name exists, or when tmux
    /// refuses any step.
    pub async fn build(&self, workspace: &Workspace) -> Result<Session, BuildError> {
        self.server
            .validate_layouts(workspace.windows.iter().filter_map(|window| {
                window
                    .layout
                    .as_deref()
                    .filter(|layout| !layout.is_empty())
                    .map(|layout| (std::ffi::OsStr::new(layout), window.panes.len().max(1)))
            }))
            .await?;
        let plan = self.plan(workspace);
        // Marked, because a workspace is mostly a creation followed by the
        // typing that decorates it, which is the shape the fold is for.
        let result = plan.run(self.server, Planner::Marked).await?;
        if !result.is_complete() {
            // Asked after the fact rather than checked before: a name can be
            // taken between a check and a create, so tmux refusing is the only
            // answer that cannot be stale.
            let refusal = result
                .steps()
                .iter()
                .find_map(libtmux::plan::StepOutcome::refusal);
            if matches!(refusal.as_ref(), Some(libtmux::Error::SessionExists { .. })) {
                return Err(BuildError::SessionExists {
                    name: workspace.session_name.clone(),
                });
            }
            return match refusal {
                Some(error) if result.created(0).is_some() => {
                    Err(error.after_effect("workspace-build").into())
                }
                Some(error) => Err(BuildError::Refused {
                    name: workspace.session_name.clone(),
                    detail: error.to_string(),
                }),
                None => Err(BuildError::Refused {
                    name: workspace.session_name.clone(),
                    detail: String::from("tmux refused a step without saying why"),
                }),
            };
        }

        let created = result
            .created(0)
            .and_then(|id| id.to_str())
            .and_then(|id| id.parse::<SessionId>().ok())
            .ok_or_else(|| BuildError::MissingInitialWindow {
                name: workspace.session_name.clone(),
            })?;
        self.server
            .session_by_id(&created)
            .await
            .map_err(|error| error.after_effect("workspace-build"))?
            .ok_or_else(|| BuildError::MissingInitialWindow {
                name: workspace.session_name.clone(),
            })
    }

    fn session_op(workspace: &Workspace) -> NewSession {
        // A workspace file is not this program's own text, and every sink
        // below escapes what it is given, so a `#(command)` in one arrives as
        // the characters someone typed rather than as a shell command.
        let mut session = NewSession::new(workspace.session_name.as_str());
        if let Some(directory) = workspace.start_directory.as_deref() {
            session = session.start_directory(directory);
        }
        session
    }

    fn window_op(
        session: Slot<SessionSlot>,
        config: &WindowConfig,
        environment: &[(String, String)],
        directory: Option<&Path>,
        pane_shell: Option<&str>,
    ) -> NewWindow {
        let mut window = NewWindow::new(session);
        if let Some(name) = config.window_name.as_deref() {
            window = window.name(name);
        }
        if let Some(directory) = directory {
            window = window.start_directory(directory);
        }
        if let Ok(index) = u32::try_from(config.window_index.unwrap_or(-1)) {
            window = window.index(index);
        }
        // tmuxp's window_shell replaces the window's shell rather than being
        // typed into it, so the window closes when the command ends; the
        // first pane's own `shell` overrides it, the same as for a split.
        if let Some(shell) = pane_shell.or(config.window_shell.as_deref()) {
            window = window.command(shell);
        }
        for (name, value) in environment {
            window = window.environment(name.as_str(), value.as_str());
        }
        window
    }

    /// Split `source` into a new pane of `window`, and rebalance the layout.
    ///
    /// Halving each pane in turn runs out of rows before the fifth at a
    /// default terminal size; rebalancing after every split reclaims them.
    /// The window's own layout, applied once the pane count is final, still
    /// has the last say. `window_shell` is the default shell for every pane
    /// in the window, not only the one that comes with it, unless the pane
    /// sets its own [`PaneConfig::shell`].
    fn split_op(
        plan: &mut Plan,
        window: Slot<WindowSlot>,
        source: Slot<PaneSlot>,
        config: &WindowConfig,
        directory: Option<&Path>,
        pane: &PaneConfig,
        environment: &[(String, String)],
    ) -> Slot<PaneSlot> {
        let directory = pane.start_directory.as_deref().or(directory);
        let mut split = SplitWindow::from_pane(source);
        if let Some(directory) = directory {
            split = split.start_directory(directory);
        }
        for (name, value) in environment {
            split = split.environment(name.as_str(), value.as_str());
        }
        if let Some(shell) = pane.shell.as_deref().or(config.window_shell.as_deref()) {
            split = split.command(shell);
        }
        let pane = plan.add(split);
        plan.add(SelectLayout::new(window, "tiled"));
        pane
    }

    /// Each pane's [`Self::pane_environment`] for a window, in order.
    fn pane_environments(
        workspace: &Workspace,
        config: &WindowConfig,
    ) -> Vec<Vec<(String, String)>> {
        config
            .panes
            .iter()
            .map(|pane_config| Self::pane_environment(workspace, config, pane_config))
            .collect()
    }

    /// This pane's own environment if it set one, else the window's, plus
    /// the loader's value of every `$NAME` its commands -- and its shell,
    /// or the window's when it sets none of its own -- reference, when the
    /// document does not set that name and the loader has it.
    ///
    /// tmuxp pastes the value into the command instead, so a value holding
    /// `;` or `$(...)` would run as a command of its own; here it stays data
    /// in the environment for the pane's own shell to expand.
    fn pane_environment(
        workspace: &Workspace,
        config: &WindowConfig,
        pane_config: &PaneConfig,
    ) -> Vec<(String, String)> {
        let mut environment = pane_config
            .environment
            .clone()
            .unwrap_or_else(|| config.environment.clone());
        let commands = workspace
            .shell_command_before
            .iter()
            .chain(&config.shell_command_before)
            .chain(&pane_config.shell_command_before)
            .chain(&pane_config.shell_commands)
            .map(|command| command.cmd.as_str());
        let shell = pane_config
            .shell
            .as_deref()
            .or(config.window_shell.as_deref());
        for name in commands.chain(shell).flat_map(referenced) {
            if environment.iter().any(|(set, _)| set == name) {
                continue;
            }
            if let Ok(value) = std::env::var(name) {
                environment.push((name.to_owned(), value));
            }
        }
        environment
    }

    /// Type one command into a pane, optionally running it.
    ///
    /// A leading space keeps the command out of shell history, which is what
    /// tmuxp's `suppress_history` means. It only works for shells configured
    /// to ignore space-prefixed commands, which is the same caveat tmuxp has.
    ///
    /// `enter: false` types the command and leaves it, so a workspace can set
    /// something up for the user to read before running.
    fn typing(
        pane: Slot<PaneSlot>,
        command: &str,
        suppress_history: bool,
        enter: bool,
    ) -> SendKeys {
        let text = if suppress_history {
            format!(" {command}")
        } else {
            command.to_owned()
        };
        let keys = SendKeys::new(pane).text(text);
        if enter { keys.enter() } else { keys }
    }
}

/// The variables `text` names as `$NAME` or `${NAME...}`, for a shell to
/// expand. A braced form counts by its leading name, as in `${NAME:-default}`.
fn referenced(text: &str) -> impl Iterator<Item = &str> {
    text.match_indices('$').filter_map(|(index, _)| {
        let tail = &text[index + 1..];
        let tail = tail.strip_prefix('{').unwrap_or(tail);
        let end = tail
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(tail.len());
        let name = &tail[..end];
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            .then_some(name)
    })
}

/// Compiles the `libtmux-macros` README's examples, and nothing else.
///
/// It cannot be compiled from `libtmux-macros`, whose only dependency on
/// `libtmux` is deliberately renamed so the UI tests prove the derive resolves
/// the crate. Here `libtmux` is an ordinary dependency under its own name,
/// which is the case a reader of that README is in.
#[cfg(doctest)]
#[doc = include_str!("../libtmux-macros-README.md")]
pub struct MacrosReadme;

/// The `tmux-workspace` command's reference, rendered with the library's
/// documentation so it is readable on docs.rs and a link to it is checked
/// when the documentation builds. Documentation only; it holds no items.
#[cfg(doc)]
#[doc = include_str!("../docs/cli.md")]
pub mod command {}

/// Compiles the README's Rust blocks. The README is the command's page on
/// crates.io, not this crate's documentation, so it renders nowhere here.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct Readme;
