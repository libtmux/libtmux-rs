//! Shared helpers for the tests that start the installed command.

#![allow(dead_code)]

use std::ffi::OsString;
use std::path::Path;

/// Whether this is a checkout, which has the sandbox's `.cargo/config.toml`.
///
/// A package does not, and there nothing applies one.
pub(crate) fn in_checkout() -> bool {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cargo/config.toml")
        .is_file()
}

/// Refuse to start a child when a checkout's sandbox did not reach this process,
/// naming the variable at fault and never its value.
pub(crate) fn require_sandbox() {
    if !in_checkout() {
        return;
    }
    for name in ["TMUX", "TMUX_PANE", "LIBTMUX_SOCKET", "LIBTMUX_SOCKET_PATH"] {
        assert!(
            std::env::var_os(name).is_none(),
            "{name} is set; run this from the checkout, where Cargo sandboxes it",
        );
    }
    assert!(
        std::env::var_os("LIBTMUX_TEST_SANDBOX").is_some(),
        "LIBTMUX_TEST_SANDBOX is unset; run this from the checkout, where Cargo sandboxes it",
    );
}

/// What a child started from `env_clear()` keeps of the sandbox: the caller's
/// `TMUX_TMPDIR`, so a command that names no socket cannot reach `/tmp`.
pub(crate) fn kept_environment() -> Vec<(&'static str, OsString)> {
    require_sandbox();
    std::env::var_os("TMUX_TMPDIR")
        .map(|directory| ("TMUX_TMPDIR", directory))
        .into_iter()
        .collect()
}
