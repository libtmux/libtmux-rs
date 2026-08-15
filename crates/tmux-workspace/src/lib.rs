#![doc = include_str!("../README.md")]
//!
//! Build tmux workspaces from tmuxp-style YAML.
//!
//! This crate exists to exercise the `libtmux` public API from outside, the
//! way a real consumer would. It reads the parts of a tmuxp workspace file a
//! builder needs and reproduces them with tmux.
//!
//! ```no_run
//! use tmux_workspace::{Workspace, WorkspaceBuilder};
//!
//! # async fn build() -> Result<(), tmux_workspace::BuildError> {
//! let workspace = Workspace::from_yaml(
//!     "
//! session_name: dev
//! windows:
//!   - window_name: editor
//!     panes:
//!       - vim
//!       - htop
//! ",
//! )?;
//!
//! let server = libtmux::Server::new()?;
//! let session = WorkspaceBuilder::new(&server).build(&workspace).await?;
//! assert_eq!(session.window_count(), 1);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

mod config;

pub use config::{ConfigError, PaneConfig, WindowConfig, Workspace};

use std::path::Path;

use libtmux::plan::{
    KillWindow, NewSession, NewWindow, PaneSlot, Plan, Planner, SelectLayout, SelectPane,
    SelectWindow, SendKeys, SessionSlot, SetEnvironment, SetOption, Slot, SplitWindow,
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
    /// assert!(plan.preview()[0]
    ///     .as_ref()
    ///     .is_some_and(|command| command.summary().to_string().contains("new-session")));
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
            let window = plan.add(Self::window_op(session, config, directory));
            for (name, value) in &config.options {
                plan.add(SetOption::window(window, name.as_str(), value.as_str()));
            }

            // A new window arrives holding exactly one pane, so the count is
            // known rather than looked up: the first configured pane is that
            // one, and the rest are splits.
            let mut panes = vec![window.pane()];
            for pane in config.panes.iter().skip(1) {
                let directory = pane.start_directory.as_deref().or(directory);
                let mut split = SplitWindow::new(window);
                if let Some(directory) = directory {
                    split = split.start_directory(directory);
                }
                for (name, value) in &pane.environment {
                    split = split.environment(name.as_str(), value.as_str());
                }
                panes.push(plan.add(split));
            }

            // Layout is applied once the pane count is final, or tmux would
            // rebalance it away on the next split.
            if let Some(layout) = config.layout.as_deref() {
                plan.add(SelectLayout::new(window, layout));
            }

            let mut focus_pane = None;
            for (pane, pane_config) in panes.iter().zip(&config.panes) {
                // Narrowest wins: pane, then window, then workspace.
                let suppress = pane_config
                    .suppress_history
                    .or(config.suppress_history)
                    .unwrap_or(workspace.suppress_history);

                let before = workspace
                    .shell_command_before
                    .iter()
                    .chain(&config.shell_command_before);
                for command in before {
                    plan.add(Self::typing(*pane, command, suppress, true));
                }
                for command in &pane_config.shell_commands {
                    plan.add(Self::typing(*pane, command, suppress, pane_config.enter));
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
            if matches!(refusal, Some(libtmux::Error::SessionExists { .. })) {
                return Err(BuildError::SessionExists {
                    name: workspace.session_name.clone(),
                });
            }
            return Err(BuildError::Refused {
                name: workspace.session_name.clone(),
                detail: refusal.map_or_else(
                    || String::from("tmux refused a step without saying why"),
                    |error| error.to_string(),
                ),
            });
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
            .await?
            .ok_or_else(|| BuildError::MissingInitialWindow {
                name: workspace.session_name.clone(),
            })
    }

    fn session_op(workspace: &Workspace) -> NewSession {
        let mut session = NewSession::new(workspace.session_name.as_str());
        if let Some(directory) = workspace.start_directory.as_deref() {
            session = session.start_directory(directory);
        }
        session
    }

    fn window_op(
        session: Slot<SessionSlot>,
        config: &WindowConfig,
        directory: Option<&Path>,
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
        // typed into it, so the window closes when the command ends.
        if let Some(shell) = config.window_shell.as_deref() {
            window = window.command(shell);
        }
        for (name, value) in &config.environment {
            window = window.environment(name.as_str(), value.as_str());
        }
        window
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

/// Compiles the `libtmux-macros` README's examples, and nothing else.
///
/// It cannot be compiled from `libtmux`, where the derive resolves the crate
/// to `crate`, nor from `libtmux-macros`, whose only dependency on `libtmux`
/// is deliberately renamed so the UI tests prove that resolution works. Here
/// `libtmux` is an ordinary dependency under its own name, which is the one
/// case a reader of that README is actually in.
#[cfg(doctest)]
#[doc = include_str!("../../libtmux-macros/README.md")]
pub struct MacrosReadme;
