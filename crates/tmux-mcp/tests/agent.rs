//! Live checks for the retained MCP tool families.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use libtmux::test::TestServer;
use libtmux::{Command, NewWindowOptions, Server, SplitDirection, SplitOptions};
use serde_json::Value;
use tmux_mcp::{CallerIdentity, TmuxTools};
use tokio_util::sync::CancellationToken;

mod support;

use support::{args, bare_tools, json, prompt_ready};

async fn fixture(name: &str) -> (TestServer, TmuxTools) {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": name})))
        .await
        .expect("session is created");
    (guard, tools)
}

async fn panes(tools: &TmuxTools) -> Vec<Value> {
    json(tools.list_panes().await.expect("panes"))["panes"]
        .as_array()
        .expect("pane rows")
        .clone()
}

async fn typing_fixture(name: &str) -> (TestServer, TmuxTools, String) {
    let (guard, tools) = fixture(name).await;
    let pane = panes(&tools).await[0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    prompt_ready(guard.server(), &pane).await;
    (guard, tools, pane)
}

async fn split(server: &Server, pane: &str) -> String {
    let pane = server
        .panes()
        .await
        .expect("panes list")
        .into_iter()
        .find(|candidate| candidate.id().to_string() == pane)
        .expect("pane exists");
    pane.split(SplitOptions::new(SplitDirection::Below))
        .await
        .expect("pane splits")
        .id()
        .to_string()
}

async fn socket_of(server: &Server) -> String {
    server
        .cmd(
            Command::new("display-message")
                .arg("-p")
                .arg("#{socket_path}"),
        )
        .await
        .expect("tmux reports its socket")
        .stdout_lossy()
        .trim()
        .to_owned()
}

async fn identity_for(server: &Server, pane: &str) -> CallerIdentity {
    CallerIdentity::from_values(
        Some(format!("{},1,$0", socket_of(server).await).into()),
        Some(pane.into()),
    )
    .expect("caller identity")
}

#[tokio::test]
async fn send_keys_reports_synchronized_target_expansion() {
    let (guard, tools) = fixture("synchronized-targets").await;
    let first = panes(&tools).await[0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    split(guard.server(), &first).await;
    let listed = panes(&tools).await;
    let window = listed[0]["window_id"].as_str().expect("window id");
    let expected: BTreeSet<_> = listed
        .iter()
        .map(|pane| pane["id"].as_str().expect("pane id"))
        .collect();
    guard
        .server()
        .windows()
        .await
        .expect("windows list")
        .into_iter()
        .find(|candidate| candidate.id().to_string() == window)
        .expect("window exists")
        .set_option("synchronize-panes", "on")
        .await
        .expect("synchronized input is enabled");

    let result = json(
        tools
            .send_keys(args(serde_json::json!({"pane": first, "keys": ["C-l"]})))
            .await
            .expect("keys are sent"),
    );
    let actual: BTreeSet<_> = result["panes"]
        .as_array()
        .expect("resolved panes")
        .iter()
        .map(|pane| pane.as_str().expect("pane id"))
        .collect();

    assert_eq!(actual, expected);
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn teardown_refuses_the_inherited_caller_pane() {
    let (guard, bare) = fixture("caller-guard").await;
    let own = panes(&bare).await[0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    let tools = TmuxTools::builder(guard.server().clone())
        .caller(Some(identity_for(guard.server(), &own).await))
        .build();

    let error = tools
        .kill_pane(args(serde_json::json!({"pane": own})))
        .await
        .map(|_| ())
        .expect_err("caller pane is protected");

    assert!(error.message.contains(&own), "{}", error.message);
    assert_eq!(panes(&tools).await.len(), 1);
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn run_shell_command_reports_output_status_and_cancellation() {
    let (guard, tools, pane) = typing_fixture("run").await;
    let finished = json(
        tools
            .run_command(
                args(serde_json::json!({
                    "pane": pane,
                    "command": "printf retained-output; exit 3",
                    "seconds": 20
                })),
                CancellationToken::new(),
                tmux_mcp::Reporter::none(),
            )
            .await
            .expect("command runs"),
    );
    assert_eq!(finished["outcome"], "completed");
    assert_eq!(finished["exit_status"], 3);
    assert!(
        finished["output"]
            .as_str()
            .expect("output")
            .contains("retained-output")
    );

    let cancelled = CancellationToken::new();
    let request = tokio::spawn({
        let tools = tools.clone();
        let pane = pane.clone();
        let cancelled = cancelled.clone();
        async move {
            tools
                .run_command(
                    args(serde_json::json!({
                        "pane": pane,
                        "command": "sleep 30",
                        "seconds": 60
                    })),
                    cancelled,
                    tmux_mcp::Reporter::none(),
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancelled.cancel();
    let stopped = json(request.await.expect("request joins").expect("run answers"));
    assert_eq!(stopped["outcome"], "cancelled");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn wait_and_cursor_tools_observe_live_output() {
    let (guard, tools, pane) = typing_fixture("observe").await;
    let opened = json(
        tools
            .capture_since(args(serde_json::json!({"pane": pane})))
            .await
            .expect("tail opens"),
    );
    let cursor = opened["cursor"].as_str().expect("cursor").to_owned();
    let waiting = tokio::spawn({
        let tools = tools.clone();
        let pane = pane.clone();
        async move {
            tools
                .wait_for_text(
                    args(serde_json::json!({
                        "pane": pane,
                        "patterns": ["live-marker"],
                        "seconds": 20
                    })),
                    CancellationToken::new(),
                    tmux_mcp::Reporter::none(),
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "printf live-marker",
            "enter": true
        })))
        .await
        .expect("input is sent");

    let waited = json(waiting.await.expect("wait joins").expect("wait answers"));
    assert_eq!(waited["outcome"], "matched");
    let mut since = Value::Null;
    for _ in 0..40 {
        since = json(
            tools
                .capture_since(args(serde_json::json!({"pane": pane, "cursor": cursor})))
                .await
                .expect("tail reads"),
        );
        if since["text"]
            .as_str()
            .is_some_and(|text| text.contains("live-marker"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(since["text"].as_str().unwrap().contains("live-marker"));

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn search_snapshot_and_configuration_reads_are_structured() {
    let (guard, tools, pane) = typing_fixture("inspect").await;
    tools
        .run_command(
            args(serde_json::json!({
                "pane": pane,
                "command": "echo searchable-marker",
                "seconds": 20
            })),
            CancellationToken::new(),
            tmux_mcp::Reporter::none(),
        )
        .await
        .expect("marker prints");
    guard
        .server()
        .set_global_option("@probe", "configured")
        .await
        .expect("fixture option is set");
    guard
        .server()
        .set_environment("TMUX_MCP_PROBE", "secret-like")
        .await
        .expect("fixture environment is set");

    let found = json(
        tools
            .search_panes(args(serde_json::json!({"pattern": "searchable-marker"})))
            .await
            .expect("search runs"),
    );
    assert!(!found["matches"].as_array().unwrap().is_empty());
    let snapshot = json(
        tools
            .snapshot_pane(args(serde_json::json!({"pane": pane, "max_lines": 5})))
            .await
            .expect("snapshot reads"),
    );
    assert_eq!(snapshot["pane"]["id"], pane);
    let variables = json(
        tools
            .get_tmux_variables(args(serde_json::json!({
                "names": ["pane_id", "session_name"],
                "pane": pane
            })))
            .await
            .expect("tmux variables read"),
    );
    assert_eq!(variables["values"]["pane_id"], pane);
    assert_eq!(variables["values"]["session_name"], "inspect");
    let option = json(
        tools
            .show_option(args(serde_json::json!({
                "name": "@probe",
                "scope": "global-session"
            })))
            .await
            .expect("option reads"),
    );
    assert_eq!(option["value"], "configured");
    let environment = json(
        tools
            .show_environment(args(serde_json::json!({})))
            .await
            .expect("environment reads"),
    );
    assert!(
        environment["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| { entry["name"] == "TMUX_MCP_PROBE" && entry["value"] == "secret-like" })
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn selection_paste_and_channel_handlers_change_tmux() {
    let (guard, tools, first) = typing_fixture("manage").await;
    let second = split(guard.server(), &first).await;
    let selected = json(
        tools
            .select_pane(args(serde_json::json!({"pane": second})))
            .await
            .expect("pane selects"),
    );
    assert_eq!(selected["id"], second);

    tools
        .paste_text(args(serde_json::json!({
            "pane": first,
            "text": "printf pasted-marker\n"
        })))
        .await
        .expect("text pastes");
    libtmux::test::retry_until(Duration::from_secs(2), async || {
        tools
            .capture_pane(args(serde_json::json!({"pane": first})))
            .await
            .ok()
            .is_some_and(|capture| {
                json(capture)["text"]
                    .as_str()
                    .unwrap()
                    .contains("pasted-marker")
            })
    })
    .await
    .expect("pasted text reaches the pane");

    let waiting = tokio::spawn({
        let tools = tools.clone();
        async move {
            tools
                .wait_for_channel(args(serde_json::json!({
                    "channel": "retained-channel",
                    "seconds": 20
                })))
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    tools
        .signal_channel(args(serde_json::json!({"channel": "retained-channel"})))
        .await
        .expect("channel signals");
    let released = json(waiting.await.expect("wait joins").expect("wait answers"));
    assert_eq!(released["outcome"], "signalled");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn window_selection_uses_core_fixture_setup() {
    let (guard, tools) = fixture("windows").await;
    let session = guard
        .server()
        .sessions()
        .await
        .expect("sessions list")
        .remove(0);
    let second = session
        .new_window(NewWindowOptions::new("second"))
        .await
        .expect("window starts")
        .id()
        .to_string();

    let selected = json(
        tools
            .select_window(args(serde_json::json!({"window": second})))
            .await
            .expect("window selects"),
    );
    assert!(
        selected["windows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|window| window["id"] == second && window["active"] == true)
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}
