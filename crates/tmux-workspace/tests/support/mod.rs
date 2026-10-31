//! Shared helpers for the tests that start the installed command.

#![allow(dead_code)]

use std::ffi::OsString;

/// What a child started from `env_clear()` keeps of the sandbox: the caller's
/// `TMUX_TMPDIR`, so a command that names no socket cannot reach `/tmp`.
pub(crate) fn kept_environment() -> Vec<(&'static str, OsString)> {
    std::env::var_os("TMUX_TMPDIR")
        .map(|directory| ("TMUX_TMPDIR", directory))
        .into_iter()
        .collect()
}
