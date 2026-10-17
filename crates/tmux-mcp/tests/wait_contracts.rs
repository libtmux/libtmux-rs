//! `wait_for_text` contracts that hold on every daemon: the same answers from
//! the polling path (below tmux 3.7) and the control path (3.7 or newer).

#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use libtmux::test::TestServer;
use libtmux::{Command, NewSessionOptions, Server};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

mod support;
use support::{args, bare_tools, json, prompt_ready, supports_control};

const SHELL: &str = "env ENV=/dev/null PS1='ready> ' sh";

async fn pane_in(server: &Server, name: &str, width: u32) -> String {
    // A second session keeps the daemon alive when the watched pane dies.
    if server.sessions().await.expect("sessions").is_empty() {
        server
            .new_session(NewSessionOptions::new("keeper").command("sleep 600"))
            .await
            .expect("keeper session");
    }
    let session = server
        .new_session(NewSessionOptions::new(name).command(SHELL).size(width, 12))
        .await
        .expect("session");
    let pane = session.panes().await.expect("panes").remove(0);
    let id = pane.id().to_string();
    prompt_ready(server, &id).await;
    id
}

async fn type_line(server: &Server, pane: &str, text: &str) {
    bare_tools(server)
        .send_keys(args(
            serde_json::json!({"pane": pane, "text": text, "enter": true}),
        ))
        .await
        .expect("the line is typed");
}

async fn wait(server: &Server, pane: &str, pattern: &str, seconds: u64) -> Value {
    json(
        bare_tools(server)
            .wait_for_text(
                args(serde_json::json!({
                    "pane": pane, "patterns": [pattern], "seconds": seconds
                })),
                CancellationToken::new(),
                tmux_mcp::Reporter::none(),
            )
            .await
            .expect("a wait ends as an outcome, never an error"),
    )
}

/// A match the pane wrapped across rows is found: on the poll by joining
/// wrapped rows, on the control path by confirming against the same join.
#[tokio::test]
async fn a_match_split_across_a_wrapped_row_is_found() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    let pane = pane_in(server, "wrapped", 40).await;
    let run =
        "printf '%s%s\\n' aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa WRAPPED";
    let (view, ()) = tokio::join!(
        wait(
            server,
            &pane,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaWRAPPED",
            10
        ),
        async {
            tokio::time::sleep(Duration::from_millis(700)).await;
            type_line(server, &pane, run).await;
        }
    );
    assert_eq!(view["outcome"], "matched", "{view}");
    guard.shutdown().await.expect("fixture closes");
}

/// A pane that dies mid-wait ends the wait as `pane_closed` with the output
/// so far, never as a stream loss.
#[tokio::test]
async fn a_pane_killed_mid_wait_ends_as_pane_closed_with_its_output() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    let pane = pane_in(server, "dying", 80).await;
    let (view, ()) = tokio::join!(wait(server, &pane, "never-printed", 20), async {
        tokio::time::sleep(Duration::from_millis(900)).await;
        type_line(server, &pane, "echo output-before-death").await;
        tokio::time::sleep(Duration::from_millis(900)).await;
        server
            .cmd(Command::new("kill-pane").arg("-t").arg(pane.clone()))
            .await
            .expect("pane is killed");
    });
    assert_eq!(view["outcome"], "pane_closed", "{view}");
    assert!(
        view["text"]
            .as_str()
            .is_some_and(|text| text.contains("output-before-death")),
        "the output so far is kept: {view}"
    );
    guard.shutdown().await.expect("fixture closes");
}

/// A server killed mid-wait gives the answer a dead pane gives.
#[tokio::test]
async fn a_server_killed_mid_wait_ends_as_pane_closed_with_its_output() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    let pane = pane_in(server, "doomed", 80).await;
    let (view, ()) = tokio::join!(wait(server, &pane, "never-printed", 20), async {
        tokio::time::sleep(Duration::from_millis(900)).await;
        type_line(server, &pane, "echo output-before-death").await;
        tokio::time::sleep(Duration::from_millis(900)).await;
        let _ = server.cmd(Command::new("kill-server")).await;
    });
    assert_eq!(view["outcome"], "pane_closed", "{view}");
    assert!(
        view["text"]
            .as_str()
            .is_some_and(|text| text.contains("output-before-death")),
        "the output so far is kept: {view}"
    );
    let _ = guard.shutdown().await;
}

/// Detaching the control client while the pane lives is a lost stream, not a
/// closed pane. The polling path has no stream to lose.
#[tokio::test]
async fn a_dropped_stream_on_a_live_pane_is_stream_lost() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    if !supports_control(server).await {
        eprintln!("the control path needs tmux 3.7 or newer");
        guard.shutdown().await.expect("fixture closes");
        return;
    }
    let pane = pane_in(server, "lively", 80).await;
    let (view, ()) = tokio::join!(wait(server, &pane, "never-printed", 20), async {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let listed = server
            .cmd(Command::new("list-clients").arg("-F").arg("#{client_name}"))
            .await
            .expect("clients list");
        for client in listed.stdout_lossy().lines() {
            server
                .cmd(Command::new("detach-client").arg("-t").arg(client))
                .await
                .expect("client detaches");
        }
    });
    assert_eq!(view["outcome"], "stream_lost", "{view}");
    guard.shutdown().await.expect("fixture closes");
}

/// A submitted line wider than the pane is still this server's own echo, so
/// waiting for text it contains does not match it.
#[tokio::test]
async fn a_submitted_line_that_wrapped_is_not_a_match() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    let pane = pane_in(server, "echoed", 40).await;
    // One tools instance both sends and waits: the echo record lives in it.
    let tools = bare_tools(server);
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": ": echo-only-marker aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "enter": true,
        })))
        .await
        .expect("the line is typed");
    let view = json(
        tools
            .wait_for_text(
                args(serde_json::json!({
                    "pane": pane, "patterns": ["echo-only-marker"], "seconds": 2
                })),
                CancellationToken::new(),
                tmux_mcp::Reporter::none(),
            )
            .await
            .expect("the wait ends as an outcome"),
    );
    assert_eq!(view["outcome"], "deadline", "{view}");
    guard.shutdown().await.expect("fixture closes");
}
