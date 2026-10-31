//! What a cleared child keeps of the tmux sandbox.
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::process::Command;

mod support;

#[test]
fn a_cleared_child_keeps_the_callers_tmux_tmpdir() {
    let output = Command::new("sh")
        .args(["-c", "printf %s \"${TMUX_TMPDIR-unset}\""])
        .env_clear()
        .envs(support::kept_environment())
        .output()
        .expect("sh runs");
    let expected = std::env::var_os("TMUX_TMPDIR");

    assert!(output.status.success());
    match expected {
        Some(directory) => assert!(
            output.stdout == directory.as_encoded_bytes(),
            "the child did not keep TMUX_TMPDIR",
        ),
        None => assert_eq!(output.stdout, b"unset"),
    }
}

#[test]
fn a_cleared_child_without_the_helper_loses_it() {
    let output = Command::new("sh")
        .args(["-c", "printf %s \"${TMUX_TMPDIR-unset}\""])
        .env_clear()
        .output()
        .expect("sh runs");

    assert_eq!(output.stdout, b"unset", "the control: env_clear() drops it");
}
