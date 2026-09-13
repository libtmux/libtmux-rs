//! Interactive bootstrap children preserve terminal and process ownership.
#![cfg(all(feature = "cli", target_os = "linux"))]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::{process::Stdio, time::Duration};

#[tokio::test]
async fn bootstrap_terminal_signals_preserve_owned_and_borrowed_sessions() {
    for append in [false, true] {
        for action in ["INT", "TERM", "terminal-INT"] {
            terminal_case(action, append, false).await;
        }
    }
}

#[tokio::test]
async fn bootstrap_stop_resume_keeps_foreground_ownership() {
    for action in ["terminal-TSTP", "background-resume", "background-read"] {
        for append in [false, true] {
            terminal_case(action, append, false).await;
        }
        terminal_case(action, false, true).await;
    }
}

#[tokio::test]
async fn bootstrap_exit_restores_terminal_and_retains_leader_through_drain() {
    for action in ["success", "failure", "pipe-owner-exits"] {
        terminal_case(action, false, false).await;
    }
    terminal_case("TERM", false, true).await;
}

async fn terminal_case(action: &str, append: bool, tostop: bool) {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let helper = directory.path().join("terminal_lifecycle.py");
    std::fs::write(&helper, include_str!("fixtures/terminal_lifecycle.py")).unwrap();
    let mut command = tokio::process::Command::new("python3");
    command
        .arg(&helper)
        .arg(env!("CARGO_BIN_EXE_tmux-workspace"))
        .arg(guard.server().tmux_executable())
        .arg("--socket")
        .arg(guard.socket_path())
        .arg("--fixture-root")
        .arg(directory.path())
        .args(["--action", action])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if append {
        command.arg("--append");
    }
    if tostop {
        command.arg("--tostop");
    }
    let output = tokio::time::timeout(Duration::from_secs(25), command.output())
        .await
        .expect("bounded terminal fixture")
        .unwrap();
    guard.shutdown().await.unwrap();
    assert!(
        output.status.success(),
        "{action} append={append} tostop={tostop}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["pass"], true, "{result}");
}
