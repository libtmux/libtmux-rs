//! Semicolon-chain dispatch against real tmux.
//!
//! These lock in what a chain can and cannot tell you. tmux runs a chain up to
//! the first failure and drops the remainder, and the merged result it returns
//! is the same whichever member failed -- so the tests below assert the absence
//! of attribution as deliberately as they assert the presence of output.

#![cfg(feature = "test-support")]

use libtmux::test::TestServer;
use libtmux::{Command, CommandChain, CommandResult};

/// Print one literal line, which tmux emits on stdout.
fn say(text: &str) -> Command {
    Command::new("display-message").arg("-p").arg(text)
}

/// A command tmux always refuses: no server ever holds pane `%999`.
fn refused() -> Command {
    Command::new("kill-pane").arg("-t").arg("%999")
}

fn stdout(result: &CommandResult) -> String {
    result.stdout_lossy().into_owned()
}

#[tokio::test]
async fn a_chain_runs_every_member_in_order_as_one_dispatch() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    server
        .new_session("chain-order")
        .await
        .expect("session is created");

    let result = server
        .chain(
            CommandChain::new(say("first"))
                .then(say("second"))
                .then(say("third")),
        )
        .await
        .expect("chain dispatches");

    assert!(result.success());
    assert_eq!(stdout(&result), "first\nsecond\nthird\n");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_chain_stops_at_its_first_failure_and_drops_the_remainder() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    server
        .new_session("chain-abort")
        .await
        .expect("session is created");

    let result = server
        .chain(
            CommandChain::new(say("ran"))
                .then(refused())
                .then(say("never")),
        )
        .await
        .expect("chain dispatches");

    assert!(!result.success());
    assert_eq!(
        stdout(&result),
        "ran\n",
        "the member before the failure ran; the member after it did not",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_merged_chain_result_cannot_say_which_member_failed() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    server
        .new_session("chain-attribution")
        .await
        .expect("session is created");

    // The same failure at three different positions. Only the stdout of the
    // members that already ran differs; the status and stderr do not.
    let mut outcomes = Vec::new();
    for chain in [
        CommandChain::new(refused()).then(say("a")).then(say("b")),
        CommandChain::new(say("a")).then(refused()).then(say("b")),
        CommandChain::new(say("a")).then(say("b")).then(refused()),
    ] {
        let result = server.chain(chain).await.expect("chain dispatches");
        outcomes.push((result.exit_code(), result.stderr_lossy().into_owned()));
    }

    let first = outcomes.first().expect("three outcomes were recorded");
    assert!(
        outcomes.iter().all(|outcome| outcome == first),
        "exit code and stderr are identical whichever member failed, so a \
         planner cannot infer a per-command status from them: {outcomes:?}",
    );

    // Mutating commands print nothing, so a chain of them leaves even the
    // stdout-length signal empty. Attribution has no evidence at all here.
    let silent = server
        .chain(
            CommandChain::new(
                Command::new("set-option")
                    .arg("-g")
                    .arg("history-limit")
                    .arg("100"),
            )
            .then(refused()),
        )
        .await
        .expect("chain dispatches");
    assert!(!silent.success());
    assert_eq!(stdout(&silent), "");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_caller_supplied_semicolon_stays_an_argument_in_a_chain() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    server
        .new_session("chain-literal")
        .await
        .expect("session is created");

    // If `;` leaked through as a boundary, tmux would parse `kill-server` as a
    // command and the fixture would die instead of echoing the text back.
    let result = server
        .chain(CommandChain::new(say("; kill-server")).then(say("after")))
        .await
        .expect("chain dispatches");

    assert!(result.success());
    assert_eq!(stdout(&result), "; kill-server\nafter\n");
    assert!(
        server.is_alive().await,
        "the literal argument never reached tmux as a command",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_created_id_is_still_captured_when_a_later_member_fails() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server
        .new_session("chain-capture")
        .await
        .expect("session is created");

    let result = server
        .chain(
            CommandChain::new(
                Command::new("split-window")
                    .arg("-t")
                    .arg(session.id().to_string())
                    .arg("-P")
                    .arg("-F")
                    .arg("#{pane_id}"),
            )
            .then(refused()),
        )
        .await
        .expect("chain dispatches");

    assert!(!result.success());
    assert!(
        stdout(&result).starts_with('%'),
        "the split's printed pane id survives a later failure, so a fold that \
         captures an id can still bind it: {:?}",
        stdout(&result),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}
