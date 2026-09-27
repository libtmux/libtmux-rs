//! Deciding which keys a document may use. Each level -- workspace, window,
//! pane -- keeps a list of keys this parser acts on, and the workspace and
//! window also keep one of keys tmuxp writes that this parser only
//! tolerates.

use yaml_rust2::Yaml;

use super::Problem;

/// How this parser treats a key it does not recognize.
///
/// [`Strictness::Lenient`] accepts a richer tmuxp file and records what it
/// left out in [`super::Workspace::unsupported_keys`] and the same field on a
/// window or pane. [`Strictness::Strict`] refuses a key that is neither this
/// parser's own vocabulary nor tmuxp's -- `before_script`, `plugins`,
/// `workspace_builder`, `workspace_builder_options`, `config` and
/// `socket_name` on the workspace, `options_after` on a window -- so a typo
/// is refused rather than silently ignored. A key starting with `x-`, at any
/// level, is inert either way: accepted and never acted on. Checked on the
/// workspace, a window and a pane; a command mapping's own keys (`cmd`,
/// `enter`, `sleep_before`, `sleep_after`) are not.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Strictness {
    Lenient,
    Strict,
}

/// Keys this parser understands on a window.
pub(super) const WINDOW_KEYS: &[&str] = &[
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

/// Keys tmuxp gives a window that this parser recognizes but does not act
/// on. Strict parsing accepts them alongside [`WINDOW_KEYS`] rather than
/// refusing a document only the `tmux-workspace` command's own builder can
/// finish.
pub(super) const WINDOW_RECOGNIZED: &[&str] = &["options_after"];

/// Keys this parser understands on a pane.
pub(super) const PANE_KEYS: &[&str] = &[
    "shell_command",
    "shell_command_before",
    "environment",
    "start_directory",
    "focus",
    "shell",
    "enter",
    "sleep_before",
    "sleep_after",
    "suppress_history",
];

/// Keys this parser understands at the workspace level.
pub(super) const SESSION_KEYS: &[&str] = &[
    "session_name",
    "start_directory",
    "environment",
    "options",
    "global_options",
    "shell_command_before",
    "suppress_history",
    "windows",
];

/// Keys tmuxp gives a workspace that this parser recognizes but does not act
/// on -- the Python extension bridge and script hook the `tmux-workspace`
/// command's own builder implements, and the endpoint fields it routes
/// through its own flags instead. Strict parsing accepts them alongside
/// [`SESSION_KEYS`] rather than refusing a document only that builder can
/// finish.
pub(super) const SESSION_RECOGNIZED: &[&str] = &[
    "before_script",
    "plugins",
    "workspace_builder",
    "workspace_builder_options",
    "config",
    "socket_name",
];

/// Whether a mapping has this key at all, regardless of its value.
pub(super) fn has_key(value: &Yaml, key: &str) -> bool {
    matches!(value, Yaml::Hash(entries) if entries.contains_key(&Yaml::String(key.to_owned())))
}

/// Refuse a mapping's key that is neither `known` nor `extra` nor `x-`
/// prefixed, under [`Strictness::Strict`]. [`Strictness::Lenient`] never
/// refuses here: [`unsupported`] records the same keys instead. `at` is the
/// enclosing mapping's own key path, empty at the document root, matching
/// the paths [`Problem`] already uses elsewhere in this module.
pub(super) fn check_known(
    value: &Yaml,
    known: &[&str],
    extra: &[&str],
    strictness: Strictness,
    at: &str,
) -> Result<(), Problem> {
    if strictness == Strictness::Lenient {
        return Ok(());
    }
    let Yaml::Hash(entries) = value else {
        return Ok(());
    };
    for key in entries.keys().filter_map(Yaml::as_str) {
        if key.starts_with("x-") || known.contains(&key) || extra.contains(&key) {
            continue;
        }
        let path = if at.is_empty() {
            key.to_owned()
        } else {
            format!("{at}.{key}")
        };
        return Err(Problem::new(
            path,
            "is not a key this parser acts on; prefix it x- to keep it inert, \
             or read this document with Workspace::from_yaml instead",
        ));
    }
    Ok(())
}

/// Collect the keys present in a mapping that this parser does not act on.
pub(super) fn unsupported(document: &Yaml, known: &[&str]) -> Vec<String> {
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
