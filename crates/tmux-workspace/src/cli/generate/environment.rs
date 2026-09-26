use serde_json::{Value, json};

pub(super) fn rules() -> Value {
    let rules = [
        (
            "HOME",
            "Path expansion and default configuration directories",
        ),
        ("PATH", "Native executable, editor, and Python lookup"),
        (
            "TMUXP_CONFIGDIR",
            "Nonempty value selects the workspace configuration directory",
        ),
        (
            "XDG_CONFIG_HOME",
            "Configuration directory fallback when TMUXP_CONFIGDIR is absent or empty",
        ),
        ("TMUXINATOR_CONFIG", "Tmuxinator import source directory"),
        (
            "TMUX",
            "Selected socket fallback, current-session identity, and attach versus switch-client",
        ),
        ("TMUX_PANE", "Current pane and append target selection"),
        ("LIBTMUX_TEST_TMUX", "Explicit tmux executable selection"),
        (
            "SHELL",
            "Pane shell fallback when the workspace does not select one",
        ),
        ("EDITOR", "Editor command; defaults to vi"),
        (
            "TMUX_WORKSPACE_PYTHON",
            "Explicit Python bridge interpreter; defaults to python3",
        ),
        (
            "TMUXP_DEFAULT_COLUMNS",
            "Initial width: first present value from this variable or COLUMNS; invalid values use 80",
        ),
        (
            "COLUMNS",
            "Initial width fallback when TMUXP_DEFAULT_COLUMNS is absent",
        ),
        (
            "TMUXP_DEFAULT_ROWS",
            "Initial height: first present value from this variable or ROWS; invalid values use 24",
        ),
        (
            "ROWS",
            "Initial height fallback when TMUXP_DEFAULT_ROWS is absent",
        ),
        ("NO_COLOR", "Nonempty value disables human color"),
        (
            "FORCE_COLOR",
            "Nonempty value enables human color unless explicitly disabled",
        ),
        (
            "CLICOLOR_FORCE",
            "Nonempty nonzero value enables human color unless explicitly disabled",
        ),
        ("CLICOLOR", "Zero disables automatic human color"),
        ("TERM", "dumb disables terminal progress"),
        ("TMUXP_PROGRESS", "Zero disables terminal progress"),
        (
            "TMUXP_PROGRESS_LINES",
            "Read only when terminal progress is active and --progress-lines was omitted",
        ),
        (
            "TMUXP_PROGRESS_FORMAT",
            "Read only when terminal progress is active and --progress-format was omitted",
        ),
    ];
    json!(
        rules.map(
            |(name, condition)| json!({"name":name,"binding":"runtime","condition":condition})
        )
    )
}
