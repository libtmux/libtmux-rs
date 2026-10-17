//! Product callers retain basic operations below optional feature cutoffs.

#![allow(clippy::expect_used, clippy::panic)]

use libtmux::NewSessionOptions;
use libtmux::test::TestServer;

mod support;
use support::{args, bare_tools, prompt_ready};

#[tokio::test]
async fn paste_staging_refuses_3_3a_while_basic_tools_remain_available() {
    let Some(binary) = std::env::var_os("LIBTMUX_TEST_MINIMUM_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(binary)
        .start()
        .await
        .expect("3.3a starts");
    let server = guard.server();
    let session = server
        .new_session(NewSessionOptions::new("keeper").command("env ENV=/dev/null PS1='ready> ' sh"))
        .await
        .expect("basic creation");
    let pane = session.panes().await.expect("panes").remove(0);
    prompt_ready(server, pane.id().as_ref()).await;
    let tools = bare_tools(server);
    server
        .set_buffer(Some("sentinel"), "retained")
        .await
        .expect("ordinary buffer creation");
    let before = server.buffer_names().await.expect("buffer names");
    let result = tools
        .paste_text(args(
            serde_json::json!({"pane":pane.id().to_string(),"text":"unsafe-stage"}),
        ))
        .await;
    assert_eq!(
        server.buffer_names().await.expect("buffers after refusal"),
        before,
        "no temporary staging buffer was created"
    );
    let error = result
        .err()
        .expect("managed paste needs safe abort cleanup")
        .into_error_data();
    assert!(error.message.contains("3.4"), "{error:?}");
    let detail = error.data.expect("structured refusal");
    assert_eq!(detail["kind"], "unsupported_version");
    assert_eq!(detail["retryable"], false);
    assert_eq!(detail["stale"], false);
    tools
        .paste_text(args(
            serde_json::json!({"pane":pane.id().to_string(),"text":""}),
        ))
        .await
        .expect("empty paste needs no staging");
    tools
        .list_sessions()
        .await
        .expect("basic listing remains available");
    tools
        .send_keys(args(
            serde_json::json!({"pane":pane.id().to_string(),"text":"ordinary-input"}),
        ))
        .await
        .expect("basic direct input remains available");
    guard.shutdown().await.expect("fixture closes");
}

#[tokio::test]
async fn paste_staging_works_on_3_4_without_acquiring_control() {
    let Some(binary) = std::env::var_os("LIBTMUX_TEST_SAFE_BUFFER_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(binary)
        .start()
        .await
        .expect("3.4 starts");
    let server = guard.server();
    let session = server
        .new_session(NewSessionOptions::new("keeper").command("env ENV=/dev/null PS1='ready> ' sh"))
        .await
        .expect("session");
    let pane = session.panes().await.expect("panes").remove(0);
    prompt_ready(server, pane.id().as_ref()).await;
    let tools = bare_tools(server);
    tools
        .paste_text(args(
            serde_json::json!({"pane":pane.id().to_string(),"text":"qualified-paste"}),
        ))
        .await
        .expect("safe staging works below the control cutoff");
    assert!(
        server.buffer_names().await.expect("buffers").is_empty(),
        "private buffer was cleaned up"
    );
    let captured = pane.capture().await.expect("ordinary capture");
    assert!(
        captured
            .iter()
            .any(|line| String::from_utf8_lossy(line.as_bytes()).contains("qualified-paste")),
        "paste reached the pane"
    );
    assert!(
        server.clients().await.expect("clients").is_empty(),
        "paste did not acquire a control client"
    );
    guard.shutdown().await.expect("fixture closes");
}
