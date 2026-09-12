//! Live checks for retained inspect and manage handlers.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use libtmux::test::TestServer;
use libtmux::{SplitDirection, SplitOptions};
use rmcp::model::ErrorCode;
use serde_json::Value;

mod support;

use support::{args, bare_tools, json, prompt_ready};

#[tokio::test]
async fn listings_report_the_live_hierarchy() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": "work"})))
        .await
        .expect("session starts");

    let sessions = json(tools.list_sessions().await.expect("sessions"));
    let windows = json(tools.list_windows().await.expect("windows"));
    let panes = json(tools.list_panes().await.expect("panes"));
    let tree = json(tools.describe().await.expect("server tree"));

    assert_eq!(sessions["sessions"][0]["name"], "work");
    assert_eq!(windows["windows"].as_array().unwrap().len(), 1);
    assert_eq!(panes["panes"].as_array().unwrap().len(), 1);
    assert_eq!(
        tree["sessions"][0]["windows"][0]["panes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn unknown_targets_are_structured_invalid_input() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": "work"})))
        .await
        .expect("session starts");

    let error = tools
        .capture_pane(args(serde_json::json!({"pane": "%999999"})))
        .await
        .map(|_| ())
        .expect_err("missing pane fails");

    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    let data = error.data.expect("classification");
    assert_eq!(data["kind"], "object_gone");
    assert_eq!(data["retryable"], false);
    assert_eq!(data["stale"], true);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_malformed_id_is_bad_input_and_a_padded_one_still_resolves() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": "work"})))
        .await
        .expect("session starts");

    // Text that is not an id cannot become one by looking again, so it is the
    // caller's mistake rather than state that moved. `%999999` above is the
    // other case: well formed, and genuinely gone.
    let error = tools
        .capture_pane(args(serde_json::json!({"pane": "not-a-pane"})))
        .await
        .map(|_| ())
        .expect_err("a malformed id fails");

    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    let data = error.data.expect("classification");
    assert_eq!(data["kind"], "invalid_input");
    assert_eq!(data["retryable"], false);
    assert_eq!(data["stale"], false);

    // A tmux id canonicalizes its digits, so `%00` addresses the pane `%0`.
    // Comparing the rendered string against a listing called that one missing.
    let pane = json(tools.list_panes().await.expect("panes"))["panes"][0]["id"]
        .as_str()
        .expect("a pane id")
        .to_owned();
    let padded = format!("%0{}", pane.strip_prefix('%').expect("a pane sigil"));
    tools
        .capture_pane(args(serde_json::json!({"pane": padded})))
        .await
        .expect("a padded id addresses the same pane");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn capture_can_include_scrollback() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": "capture"})))
        .await
        .expect("session starts");
    let pane = json(tools.list_panes().await.expect("panes"))["panes"][0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    prompt_ready(guard.server(), &pane).await;
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "seq 1 200",
            "enter": true
        })))
        .await
        .expect("input is sent");
    libtmux::test::retry_until(std::time::Duration::from_secs(2), async || {
        tools
            .capture_pane(args(serde_json::json!({"pane": pane, "history": true})))
            .await
            .ok()
            .is_some_and(|capture| json(capture)["text"].as_str().unwrap().contains("200"))
    })
    .await
    .expect("history contains command output");

    let visible = json(
        tools
            .capture_pane(args(serde_json::json!({"pane": pane})))
            .await
            .expect("visible capture"),
    );
    let history = json(
        tools
            .capture_pane(args(serde_json::json!({"pane": pane, "history": true})))
            .await
            .expect("history capture"),
    );
    assert!(history["lines"].as_u64().unwrap() >= visible["lines"].as_u64().unwrap());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn layout_input_shaped_like_a_flag_is_not_obeyed() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": "layout"})))
        .await
        .expect("session starts");
    let window = guard
        .server()
        .windows()
        .await
        .expect("windows list")
        .remove(0);
    let pane = window.panes().await.expect("panes list").remove(0);
    pane.split(SplitOptions::new(SplitDirection::Below))
        .await
        .expect("pane splits");
    let before: Vec<Value> = json(tools.list_panes().await.expect("panes"))["panes"]
        .as_array()
        .unwrap()
        .clone();

    let error = tools
        .select_layout(args(serde_json::json!({
            "window": window.id().to_string(),
            "layout": "-E"
        })))
        .await
        .map(|_| ())
        .expect_err("flag-shaped layout is data");
    assert!(error.message.contains("-E"), "{}", error.message);
    let after: Vec<Value> = json(tools.list_panes().await.expect("panes"))["panes"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(before.len(), after.len());

    guard.shutdown().await.expect("tmux fixture shuts down");
}
