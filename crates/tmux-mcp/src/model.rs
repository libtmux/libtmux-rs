use rmcp::schemars;
use serde::Deserialize;

use crate::schema::{
    OptionScopeSchema, ResizeDirectionSchema, SelectPaneDirectionSchema,
    SelectWindowDirectionSchema,
};

/// Arguments naming one session.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionArgs {
    /// The session name, as `list_sessions` reports it.
    pub session: String,
}

/// Arguments for creating a session.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSessionArgs {
    /// The name for the new session. It must not already exist.
    pub name: String,
    /// An optional working directory for the session's first window.
    pub start_directory: Option<String>,
}

/// Arguments naming one pane.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaneArgs {
    /// The `%`-prefixed pane id, as `list_panes` reports it.
    pub pane: String,
}

/// Arguments naming one window.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WindowArgs {
    /// The `@`-prefixed window id, as `list_windows` reports it.
    pub window: String,
}

/// Arguments for moving focus between panes.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectPaneArgs {
    /// The `%`-prefixed pane to select, or to move relative to.
    pub pane: String,
    /// Move relative to that pane instead of selecting it.
    ///
    /// `up`, `down`, `left`, and `right` follow the layout, so `up` selects
    /// whatever pane is drawn above. `last` returns to the previously active
    /// pane, and `next` and `previous` step through the window's panes in
    /// order. Omit to select the named pane itself.
    #[schemars(with = "Option<SelectPaneDirectionSchema>")]
    pub direction: Option<String>,
}

/// Arguments for running a command and waiting for it.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunCommandArgs {
    /// The `%`-prefixed pane to run in.
    pub pane: String,
    /// The command for the pane's trusted POSIX-compatible shell.
    ///
    /// Shell reserved words and special builtins must retain their standard
    /// meanings. The command runs inside a subshell, so several lines are fine
    /// and a bare `exit` does not end the pane's own shell. Invalid syntax is
    /// contained and completes with the shell's nonzero status.
    pub command: String,
    /// How long to allow, in seconds. Defaults to 30, capped at 600.
    pub seconds: Option<u64>,
    /// Whether to keep the command out of the shell's history.
    #[serde(default)]
    pub suppress_history: bool,
}

/// Arguments for reading a tmux environment.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShowEnvironmentArgs {
    /// The session whose environment to read. Omit for the server's own.
    pub session: Option<String>,
}

/// Arguments for reading the hooks set at a scope.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShowHooksArgs {
    /// The session whose hooks to read. Omit for the server's own.
    pub session: Option<String>,
}

/// Arguments for arranging a window's panes.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectLayoutArgs {
    /// The `@`-prefixed window to arrange.
    pub window: String,
    /// A named layout, or a layout string tmux produced earlier.
    ///
    /// The names are `even-horizontal`, `even-vertical`, `main-horizontal`,
    /// `main-vertical` and `tiled`.
    pub layout: String,
}

/// Arguments for putting text into a pane without typing it.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PasteTextArgs {
    /// The `%`-prefixed pane to paste into.
    pub pane: String,
    /// The text to deliver.
    pub text: String,
    /// Whether to append Enter to the same paste block.
    #[serde(default)]
    pub enter: bool,
}

/// Arguments for waiting until a pane says something.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitForTextArgs {
    /// The `%`-prefixed pane to watch.
    pub pane: String,
    /// Text that ends the wait successfully. Omit to wait for any output.
    #[schemars(length(max = 32))]
    pub patterns: Option<Vec<String>>,
    /// Text that ends the wait as a failure, reported as `stopped`.
    ///
    /// Give the failure markers you already know — `error:`, `Traceback` — and
    /// a failed run returns at once instead of at the deadline.
    #[schemars(length(max = 32))]
    pub stop: Option<Vec<String>>,
    /// Read both lists as regular expressions rather than literal text.
    #[serde(default)]
    pub regex: bool,
    /// Match case. Off by default.
    #[serde(default)]
    pub match_case: bool,
    /// How long to wait, in seconds. Defaults to 30, capped at 600.
    pub seconds: Option<u64>,
}

/// Arguments for reading what a pane wrote since last time.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptureSinceArgs {
    /// The `%`-prefixed pane to read.
    pub pane: String,
    /// The cursor from the previous call. Omit to start watching.
    pub cursor: Option<String>,
}

/// Arguments for moving focus between windows.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectWindowArgs {
    /// The `@`-prefixed window to select, or to move relative to.
    pub window: String,
    /// Move relative to that window instead of selecting it.
    ///
    /// `next` and `previous` step through the session in index order, and
    /// `last` returns to the previously active window. Omit to select the
    /// named window itself.
    #[schemars(with = "Option<SelectWindowDirectionSchema>")]
    pub direction: Option<String>,
}

/// Arguments for searching what panes are showing.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchPanesArgs {
    /// The text to look for.
    #[schemars(length(max = 4096))]
    pub pattern: String,
    /// Read the pattern as a regular expression rather than literal text.
    #[serde(default)]
    pub regex: bool,
    /// Match case. Off by default.
    #[serde(default)]
    pub match_case: bool,
    /// Search scrollback as well as the visible screen.
    #[serde(default)]
    pub history: bool,
    /// Only search panes in this session, by name.
    pub session: Option<String>,
    /// Only search panes in this window, by `@`-prefixed id.
    pub window: Option<String>,
}

/// Arguments for reading a tmux option.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OptionArgs {
    /// The option name, such as `history-limit` or a user option like `@theme`.
    pub name: String,
    /// Which tmux object the option belongs to.
    ///
    /// One of `server`, `global-session`, `global-window`, `session`,
    /// `window`, or `pane`. Defaults to `global-session`, which is what
    /// setting an option without a target means in tmux.
    #[schemars(with = "Option<OptionScopeSchema>")]
    pub scope: Option<String>,
    /// The `$`, `@` or `%`-prefixed id, for the scopes that need one.
    pub target: Option<String>,
}

/// Arguments for reading a pane's whole state.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotArgs {
    /// The `%`-prefixed pane id.
    pub pane: String,
    /// The most content lines to return, oldest dropped first.
    ///
    /// Defaults to the whole visible screen. The end of a pane is what says
    /// what just happened, so a limit keeps the end.
    pub max_lines: Option<usize>,
    /// Include scrollback rather than only the visible screen.
    #[serde(default)]
    pub history: bool,
}

/// Arguments naming a `wait-for` channel.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChannelArgs {
    /// The channel name, which is any string both sides agree on.
    pub channel: String,
    /// How long to wait, in seconds. Defaults to 30, capped at 600.
    pub seconds: Option<u64>,
}

/// Arguments for resizing a pane.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResizePaneArgs {
    /// The `%`-prefixed pane id.
    pub pane: String,
    /// Which edge to move: `up`, `down`, `left`, or `right`.
    #[schemars(with = "ResizeDirectionSchema")]
    pub direction: String,
    /// How many rows or columns to move it by.
    pub cells: u32,
}

/// Arguments for reading a pane.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapturePaneArgs {
    /// The `%`-prefixed pane id.
    pub pane: String,
    /// Read the whole history rather than the visible screen.
    #[serde(default)]
    pub history: bool,
    /// Return only the last command's output, when the shell marks its
    /// prompts.
    ///
    /// Answers far less than the history, because it starts where the last
    /// command's output began. When the pane's shell does not mark its
    /// prompts -- fish does, bash and zsh do not -- this reports
    /// `marks: "absent"` and returns the visible screen instead.
    #[serde(default)]
    pub last_command: bool,
    /// Start at this line. Zero is the top of the screen, negative is
    /// scrollback.
    pub start: Option<i32>,
    /// End at this line.
    pub end: Option<i32>,
}

/// Arguments for sending input to a pane.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendKeysArgs {
    /// The `%`-prefixed pane id.
    pub pane: String,
    /// Text typed literally into the pane. Key names are not interpreted.
    pub text: Option<String>,
    /// tmux key names to press, in order, after any text.
    ///
    /// These are interpreted rather than typed, which is the only way to send
    /// a key that has no character: `C-c` to interrupt, `Escape`, `Up`,
    /// `C-d`. Sending `C-c` as `text` would type those three characters.
    pub keys: Option<Vec<String>>,
    /// Whether to press Enter afterwards.
    #[serde(default)]
    pub enter: bool,
}
