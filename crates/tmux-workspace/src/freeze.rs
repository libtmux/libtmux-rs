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

use crate::config::{PaneConfig, WindowConfig, Workspace};

/// Describe a live session as a workspace.
///
/// The result is a [`Workspace`], so it can be rendered with
/// [`Workspace::to_yaml`] or handed straight back to
/// [`crate::WorkspaceBuilder`] to make another like it.
///
/// A pane is described by the command tmux says it is running. For a pane
/// sitting at a shell that is the shell itself, which would restart as an
/// empty pane -- correct, and rarely what was meant. Panes running something
/// are the ones worth freezing.
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
    // A pane sitting at the session's own default shell restarts as that
    // same plain shell on reload, which is what it started as. Recording
    // its command as a shell_command would instead reload the shell as a
    // pane command, running the shell inside itself. Only a pane running
    // something else needs to say so.
    //
    // `#{default-shell}` rather than Session::get_option: tmux keeps
    // default-shell in the session table, but `show-options -t <session>`
    // (no `-g`) answers only that session's own override, which is almost
    // never set -- the value nearly every session actually runs comes from
    // the global table. A format expands the effective value regardless of
    // which table holds it.
    let default_shell_text = session.format("#{default-shell}").await?;
    let default_shell = Some(default_shell_text.to_string_lossy())
        .filter(|value| !value.is_empty())
        .map(|value| basename(&value).to_owned());

    let mut windows = Vec::new();

    for window in session.windows().await? {
        let mut panes = Vec::new();
        for pane in window.panes().await? {
            let command = pane
                .current_command()
                .map(|command| command.to_string_lossy().into_owned());
            let is_default_shell = match (&command, &default_shell) {
                (Some(command), Some(shell)) => basename(command) == shell,
                _ => false,
            };
            panes.push(PaneConfig {
                shell_commands: if is_default_shell {
                    Vec::new()
                } else {
                    command.map_or_else(Vec::new, |command| vec![command])
                },
                environment: Vec::new(),
                start_directory: pane
                    .current_path()
                    .map(|path| path.to_string_lossy().into_owned().into()),
                focus: pane.is_active(),
                enter: true,
                suppress_history: None,
                unsupported_keys: Vec::new(),
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
        suppress_history: false,
        windows,
        unsupported_keys: Vec::new(),
    })
}

/// The final path component, tmux's own convention for `#{pane_current_command}`
/// and for the shell tail of a `default-shell` path.
fn basename(text: &str) -> &str {
    text.rsplit('/').next().unwrap_or(text)
}
