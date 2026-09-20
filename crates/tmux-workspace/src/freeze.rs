//! Turning a session that exists back into a workspace that describes it.
//!
//! Building is the direction a file travels; freezing is the direction a
//! person travels. Someone who arranged a session by hand, or an agent that
//! built one a step at a time, has something worth keeping and no file to
//! keep it in.
//!
//! What can be recovered is the shape: the windows, their panes, where each
//! is rooted, and which is focused. What cannot is history -- tmux remembers
//! what a pane is *running*, not the command someone typed to start it, so a
//! frozen pane carries its current command and says nothing about how it got
//! there.

use libtmux::{Error, Session};

use crate::config::{PaneConfig, ShellCommand, WindowConfig, Workspace};

/// Describe a live session as a workspace.
///
/// The result is a [`Workspace`], so it can be rendered with
/// [`Workspace::to_yaml`] or handed straight back to
/// [`crate::WorkspaceBuilder`] to make another like it.
///
/// A pane is described by the command tmux says it is running, by name and
/// without its arguments. A pane at its shell's prompt freezes to a pane
/// with no command: tmux reports the shell itself, and recording it would
/// start a shell inside a shell when the file is built. A shell is the
/// session's `default-shell`, or a common interactive shell by name.
///
/// # Errors
///
/// Returns an error when the session's windows or panes cannot be listed.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::{SplitDirection, SplitOptions};
/// use tmux_workspace::freeze;
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("built-by-hand").await?;
/// let mut window = session.active_window().await?.expect("a window");
/// window.split(SplitOptions::new(SplitDirection::Below)).await?;
///
/// let workspace = freeze(&session).await?;
///
/// assert_eq!(workspace.session_name, "built-by-hand");
/// assert_eq!(workspace.windows.len(), 1);
/// assert_eq!(workspace.windows[0].panes.len(), 2);
///
/// // And it round-trips through the file format.
/// let yaml = workspace.to_yaml();
/// assert_eq!(tmux_workspace::Workspace::from_yaml(&yaml)?, workspace);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
pub async fn freeze(session: &Session) -> Result<Workspace, Error> {
    // A format, not `Session::get_option`: that answers only an override set
    // on this session, and `default-shell` is rarely set there.
    let default_shell = session.format("#{default-shell}").await?;
    let default_shell = basename(&default_shell.to_string_lossy()).to_owned();
    let mut windows = Vec::new();

    for window in session.windows().await? {
        let mut panes = Vec::new();
        for pane in window.panes().await? {
            let command = pane
                .current_command()
                .map(|command| command.to_string_lossy().into_owned())
                .filter(|command| {
                    let name = basename(command);
                    name != default_shell && !SHELLS.contains(&name)
                });
            panes.push(PaneConfig {
                shell_commands: command.map(ShellCommand::new).into_iter().collect(),
                start_directory: pane
                    .current_path()
                    .map(|path| path.to_string_lossy().into_owned().into()),
                focus: pane.is_active(),
                ..PaneConfig::default()
            });
        }

        windows.push(WindowConfig {
            window_name: Some(window.name().to_string_lossy().into_owned()),
            window_index: None,
            window_shell: None,
            environment: Vec::new(),
            // Taken as tmux's own layout string rather than a name: the
            // arrangement someone made by dragging a border has no name.
            layout: Some(window.layout().to_string_lossy().into_owned()),
            start_directory: None,
            focus: window.is_active(),
            options: Vec::new(),
            shell_command_before: Vec::new(),
            suppress_history: None,
            panes,
            unsupported_keys: Vec::new(),
        });
    }

    Ok(Workspace {
        session_name: session.name().to_string_lossy().into_owned(),
        start_directory: None,
        environment: Vec::new(),
        options: Vec::new(),
        global_options: Vec::new(),
        shell_command_before: Vec::new(),
        suppress_history: true,
        windows,
        unsupported_keys: Vec::new(),
    })
}

/// The last path component, which is how tmux reports a pane's command.
fn basename(text: &str) -> &str {
    text.rsplit('/').next().unwrap_or(text)
}

/// Shells common enough that a pane running one is at its prompt, whatever
/// `default-shell` is: on macOS `/bin/sh` runs as `bash`.
const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ash", "ksh", "mksh", "fish", "csh", "tcsh",
];
