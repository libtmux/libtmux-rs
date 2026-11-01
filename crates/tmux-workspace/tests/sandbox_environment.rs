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
        None => assert!(output.stdout == b"unset", "the child saw a TMUX_TMPDIR"),
    }
}

#[test]
fn a_cleared_child_without_the_helper_loses_it() {
    let output = Command::new("sh")
        .args(["-c", "printf %s \"${TMUX_TMPDIR-unset}\""])
        .env_clear()
        .output()
        .expect("sh runs");

    assert!(
        output.stdout == b"unset",
        "the control: env_clear() drops it",
    );
}

#[test]
fn a_pane_environment_is_refused_and_no_value_is_printed() {
    if !support::in_checkout() {
        return;
    }
    let rerun = |environment: Option<&[(&str, &str)]>| {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command.args([
            "--exact",
            "a_cleared_child_keeps_the_callers_tmux_tmpdir",
            "--nocapture",
            "--test-threads=1",
        ]);
        if let Some(environment) = environment {
            command.env_clear().envs(environment.iter().copied());
        }
        let output = command.output().expect("the test executable runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        (output.status.success(), text)
    };

    let (passes, text) = rerun(None);
    assert!(
        passes && text.contains("1 passed"),
        "the control must pass:\n{text}",
    );

    let (passes, text) = rerun(Some(&[
        ("TMUX", "/nonexistent/decoy.sock,1,0"),
        ("TMUX_PANE", "%9"),
    ]));
    assert!(!passes && text.contains("1 failed"), "a pane got through");
    assert!(text.contains("TMUX is set"), "{text}");
    assert!(!text.contains("decoy"), "a failure printed a value");
}
