//! Shared creation receipts and pane keys through the native Tokio executor.

#![cfg(feature = "test-support")]

use std::time::Duration;

use libtmux_core::{Effect, NewSession};
use libtmux_tokio::test::{TestServer, scaled, unique_name};
use libtmux_tokio::{Command, Error, PaneWait, Server};

async fn hook(server: &Server, event: &str, command: Option<&str>) -> Result<(), Error> {
    let mut request = Command::new("set-hook")
        .arg(if command.is_some() { "-g" } else { "-gu" })
        .arg(event);
    if let Some(command) = command {
        request = request.sensitive_arg(command);
    }
    let result = server.cmd(request).await?;
    assert!(result.success());
    Ok(())
}

#[tokio::test]
async fn creation_and_observation_share_the_same_key_and_capture_excludes_hook_output() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    let receipt = server
        .new_session(
            NewSession::new(unique_name("shared-receipt"))
                .size(80, 8)
                .command("printf 'shared-pane\n\n'; exec sleep 30"),
        )
        .await
        .expect("creation is acknowledged");
    let pane = server
        .pane_by_id(&receipt.pane().id())
        .await
        .expect("pane lookup")
        .expect("created pane exists");
    assert_eq!(
        pane.wait_for_text("shared-pane", scaled(Duration::from_secs(5)))
            .await
            .expect("pane prints"),
        PaneWait::Arrived
    );
    let key = server
        .observe_pane(receipt.pane().id())
        .await
        .expect("exact pane observed");
    assert_eq!(&key, receipt.pane());
    assert_eq!(
        receipt.generation(),
        server.generation().await.expect("daemon generation")
    );
    let native = server
        .cmd(
            Command::new("capture-pane")
                .arg("-p")
                .arg("-t")
                .arg(key.id().to_string()),
        )
        .await
        .expect("ordinary native capture");
    assert!(native.success());
    hook(
        server,
        "after-capture-pane",
        Some("display-message -p private-hook-payload"),
    )
    .await
    .expect("capture hook is installed");
    let captured = server.capture(&key).await;
    hook(server, "after-capture-pane", None)
        .await
        .expect("capture hook is unset");
    assert_eq!(
        captured.expect("generation-checked capture"),
        native.stdout()
    );
    guard.shutdown().await.expect("fixture shuts down");
}

#[tokio::test]
async fn a_failed_after_hook_preserves_the_created_identities() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    hook(
        server,
        "after-new-session",
        Some("select-pane -t %4294967295"),
    )
    .await
    .expect("failing creation hook is installed");
    let result = server
        .new_session(NewSession::new(unique_name("failed-hook")))
        .await;
    hook(server, "after-new-session", None)
        .await
        .expect("creation hook is unset");
    let error = result.expect_err("the hook refuses its missing pane");
    assert_eq!(error.operation_effect(), Some(Effect::Created));
    assert!(!error.is_transient());
    let receipt = error
        .creation_receipt()
        .expect("action returned all identities");
    assert!(
        server
            .session_by_id(&receipt.session())
            .await
            .expect("session lookup")
            .is_some()
    );
    assert_eq!(
        receipt.generation(),
        server.generation().await.expect("daemon generation")
    );
    guard.shutdown().await.expect("fixture shuts down");
}

#[tokio::test]
async fn a_parked_after_hook_keeps_a_complete_receipt_across_the_deadline() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    let channel = unique_name("receipt-hook");
    hook(
        server,
        "after-new-session",
        Some(&format!("wait-for {channel}")),
    )
    .await
    .expect("parked creation hook is installed");
    let timed = Server::builder()
        .socket_path(server.socket_path())
        .tmux_executable(server.tmux_executable())
        .default_timeout(scaled(Duration::from_millis(500)))
        .build()
        .expect("timed native client");
    timed
        .daemon_version()
        .await
        .expect("warm the checked daemon version");
    let result = timed
        .new_session(NewSession::new(unique_name("parked-receipt")))
        .await;
    hook(server, "after-new-session", None)
        .await
        .expect("creation hook is unset");
    assert!(
        server
            .cmd(Command::new("wait-for").arg("-S").arg(channel))
            .await
            .expect("release hook")
            .success()
    );
    let receipt = match result {
        Ok(receipt) => receipt,
        Err(error) => {
            assert_eq!(error.operation_effect(), Some(Effect::Created));
            assert!(!error.is_transient());
            error
                .creation_receipt()
                .expect("complete receipt survives interruption")
                .clone()
        }
    };
    assert!(
        server
            .session_by_id(&receipt.session())
            .await
            .expect("session lookup")
            .is_some()
    );
    assert_eq!(
        receipt.generation(),
        server.generation().await.expect("daemon generation")
    );
    timed.shutdown().await.expect("timed client shuts down");
    guard.shutdown().await.expect("fixture shuts down");
}

#[tokio::test]
async fn pane_keys_refuse_another_endpoint_before_dispatch() {
    let owner = TestServer::new().await.expect("owner starts");
    let other = TestServer::new().await.expect("other fixture starts");
    let receipt = owner
        .server()
        .new_session(NewSession::new(unique_name("endpoint-key")))
        .await
        .expect("creation");
    let error = other
        .server()
        .capture(receipt.pane())
        .await
        .expect_err("key names another endpoint");
    assert_eq!(error.operation_effect(), Some(Effect::NotSubmitted));
    assert!(!error.is_transient());
    other.shutdown().await.expect("other fixture shuts down");
    owner.shutdown().await.expect("owner shuts down");
}

#[tokio::test]
async fn a_stale_link_cannot_unlink_the_window_that_reused_its_index() {
    let guard = TestServer::new().await.expect("tmux starts");
    let owner = guard.session("owner").await.expect("owner session");
    let borrower = guard.session("borrower").await.expect("borrower session");
    let original = owner.windows().await.expect("owner windows").remove(0);
    let replacement = owner
        .new_window("replacement")
        .await
        .expect("second window");
    original
        .link_to(&borrower, Some(9))
        .await
        .expect("first placement");
    let stale = guard
        .server()
        .window_links()
        .await
        .expect("placements")
        .into_iter()
        .find(|link| link.snapshot().session() == borrower.id() && link.snapshot().index() == 9)
        .expect("borrowed link");
    assert_eq!(stale.window(), &original);
    stale.clone().unlink().await.expect("withdraw link");
    replacement
        .link_to(&borrower, Some(9))
        .await
        .expect("reuse index");

    assert!(matches!(stale.unlink().await, Err(Error::LinkGone { .. })));
    let placements = guard
        .server()
        .window_links()
        .await
        .expect("current placements");
    assert!(
        placements
            .iter()
            .any(|link| link.snapshot().session() == borrower.id()
                && link.snapshot().index() == 9
                && link.window() == &replacement)
    );
    assert!(
        placements
            .iter()
            .any(|link| link.snapshot().session() == owner.id() && link.window() == &original)
    );
    guard.shutdown().await.expect("fixture shuts down");
}
