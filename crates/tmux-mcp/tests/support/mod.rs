//! Shared adapters for direct tool integration tests.

#![allow(dead_code)]

use std::time::Duration;

use libtmux::test::TestServer;
use libtmux::{Command, ReleaseVersion, Server, TmuxVersion};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::{ServiceExt as _, serve_server};
use serde_json::Value;
use tmux_mcp::{Selection, TmuxTools};

/// Build tool arguments from JSON, as the protocol delivers them.
pub(crate) fn args<T: serde::de::DeserializeOwned>(value: Value) -> Parameters<T> {
    Parameters(serde_json::from_value(value).expect("arguments deserialize"))
}

/// Render a typed answer as the structured content a client receives.
pub(crate) fn json<T: serde::Serialize>(answer: Json<T>) -> Value {
    serde_json::to_value(answer.0).expect("a tool answer serialises")
}

/// Read the id from an answer that made or destroyed one object.
pub(crate) fn id<T: serde::Serialize>(answer: Json<T>) -> String {
    json(answer)["id"]
        .as_str()
        .expect("the answer carries an id")
        .to_owned()
}

/// Build tools without inheriting the developer's tmux identity or policy.
pub(crate) fn bare_tools(server: &Server) -> TmuxTools {
    TmuxTools::builder(server.clone())
        .caller(None)
        .selection(
            Selection::parse(Some("inspect,manage,execute"), None, None).expect("test surface"),
        )
        .build()
}

/// Invoke one tool through its public MCP route.
pub(crate) async fn call_tool(
    tools: TmuxTools,
    name: &'static str,
    arguments: Value,
) -> CallToolResult {
    let (client_transport, server_transport) = tokio::io::duplex(1 << 20);
    let server = tokio::spawn(async move {
        let service = serve_server(tools, server_transport)
            .await
            .expect("server starts");
        let _ = service.waiting().await;
    });
    let client = ().serve(client_transport).await.expect("client connects");
    let request = CallToolRequestParams::new(name)
        .with_arguments(arguments.as_object().cloned().expect("object arguments"));
    let result = client.call_tool(request).await.expect("tool call answers");
    client.cancel().await.expect("client shuts down");
    let _ = server.await;
    result
}

/// Wait until a pane's shell can receive input.
pub(crate) async fn prompt_ready(server: &Server, pane: &str) {
    let mut last = String::new();
    for _ in 0..600 {
        let reading = server
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(pane)
                    .arg("#{cursor_x},#{cursor_y}"),
            )
            .await
            .expect("tmux reports the cursor")
            .stdout_lossy()
            .trim()
            .to_owned();
        if !reading.is_empty() && reading != "0,0" {
            return;
        }
        last = reading;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let state = server
        .cmd(
            Command::new("display-message")
                .arg("-p")
                .arg("-t")
                .arg(pane)
                .arg("running=#{pane_current_command} dead=#{pane_dead}"),
        )
        .await
        .expect("tmux reports the pane")
        .stdout_lossy()
        .trim()
        .to_owned();
    panic!("the pane never drew a prompt; cursor stayed at {last:?}, {state}");
}

async fn daemon_version(server: &Server) -> TmuxVersion {
    let report = server
        .format(None, "tmux #{version}")
        .await
        .expect("daemon release");
    let mut report = report.as_bytes().to_vec();
    report.push(b'\n');
    TmuxVersion::parse_output(&report).expect("numbered daemon release")
}

pub(crate) async fn supports(server: &Server, needs: &ReleaseVersion) -> bool {
    daemon_version(server).await.has_behavior(needs)
}

pub(crate) async fn supports_control(server: &Server) -> bool {
    daemon_version(server)
        .await
        .release()
        .is_some_and(|release| {
            *release >= libtmux::since::CONTROL_ACQUISITION
                && !release.suffix().is_release_candidate()
        })
}

pub(crate) async fn feature_fixture(
    guard: TestServer,
    needs: &ReleaseVersion,
) -> Option<TestServer> {
    if supports(guard.server(), needs).await {
        Some(guard)
    } else {
        eprintln!("positive feature fixture needs tmux {needs}");
        guard.shutdown().await.expect("ineligible fixture closes");
        None
    }
}

pub(crate) async fn control_fixture(guard: TestServer) -> Option<TestServer> {
    if supports_control(guard.server()).await {
        Some(guard)
    } else {
        eprintln!("positive control fixture needs stable tmux 3.7 or newer");
        guard.shutdown().await.expect("ineligible fixture closes");
        None
    }
}
