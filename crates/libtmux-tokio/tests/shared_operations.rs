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

#[cfg(all(feature = "control-mode", feature = "plan"))]
#[tokio::test]
async fn a_control_creation_keeps_its_receipt_when_later_hook_blocks_fail() {
    use libtmux_tokio::control::ControlMode;
    use libtmux_tokio::plan::Plan;

    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    if !server
        .daemon_version()
        .await
        .expect("daemon version")
        .has_behavior(&libtmux_tokio::since::CONTROL_ACQUISITION)
    {
        guard
            .shutdown()
            .await
            .expect("unsupported control fixture stops");
        return;
    }
    let session = guard
        .session("receipt-observer")
        .await
        .expect("attached session");
    let (sender, events) = ControlMode::attach(server, session.id())
        .await
        .expect("qualified control client")
        .split();
    hook(
        server,
        "after-new-session",
        Some("display-message -p private-hook-payload ; select-pane -t %4294967295"),
    )
    .await
    .expect("failing hook is installed");
    let mut plan = Plan::new();
    plan.add(NewSession::new(unique_name("control-receipt")));
    let result = sender.run_plan(&plan).await;
    hook(server, "after-new-session", None)
        .await
        .expect("hook is unset");
    let error = result.expect_err("a later hook block failed");
    assert_eq!(error.operation_effect(), Some(Effect::Created));
    assert!(!error.is_transient());
    assert!(!format!("{error:?} {error}").contains("private-hook-payload"));
    let receipt = error
        .creation_receipt()
        .expect("the first complete block acknowledges creation");
    assert!(server.inspect_session(receipt).await.is_ok());
    events.shutdown().await.expect("control client stops");
    guard.shutdown().await.expect("fixture stops");
}

#[cfg(all(feature = "control-mode", feature = "plan"))]
#[tokio::test]
async fn control_creation_falls_back_before_writing_unrepresentable_arguments() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    use libtmux_tokio::ControlLimits;
    use libtmux_tokio::control::ControlMode;
    use libtmux_tokio::plan::{OperationValue, Plan};

    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    if !server
        .daemon_version()
        .await
        .expect("daemon version")
        .has_behavior(&libtmux_tokio::since::CONTROL_ACQUISITION)
    {
        guard
            .shutdown()
            .await
            .expect("unsupported control fixture stops");
        return;
    }
    let session = guard
        .session("creation-fallback")
        .await
        .expect("attached session");
    let (sender, events) = ControlMode::attach_with_limits(
        server,
        session.id(),
        ControlLimits::default().max_line_bytes(512),
    )
    .await
    .expect("qualified control client")
    .split();
    let invalid_name = NewSession::new(OsString::from_vec(b"native-\xff".to_vec()));
    let mut refused = Plan::new();
    refused.add(invalid_name);
    let error = sender
        .run_plan(&refused)
        .await
        .expect_err("tmux refuses an invalid name");
    assert_eq!(error.operation_effect(), Some(Effect::Unknown));
    assert!(!error.is_transient());

    let raw = OsString::from_vec(b"native-\xff".to_vec());
    let oversized = "x".repeat(512);
    for (request, format, expected) in [
        (
            NewSession::new(unique_name("raw-environment")).environment("LIBTMUX_RAW", raw),
            "#{LIBTMUX_RAW}",
            b"native-\xff\n".to_vec(),
        ),
        (
            NewSession::new(oversized.clone()),
            "#{session_name}",
            format!("{oversized}\n").into_bytes(),
        ),
    ] {
        let mut plan = Plan::new();
        plan.add(request);
        let result = sender
            .run_plan(&plan)
            .await
            .expect("one ordinary creation returns a receipt");
        let Some(OperationValue::CreatedSession(receipt)) = result.operations()[0].value() else {
            panic!("the plan did not preserve its creation receipt");
        };
        let named = server
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(format!("{}:", receipt.session()))
                    .arg(format),
            )
            .await
            .expect("read the actual native name");
        assert!(named.success());
        assert_eq!(named.stdout(), expected);
    }
    assert!(
        sender
            .send(
                Command::new("display-message")
                    .arg("-p")
                    .arg("still-qualified")
            )
            .await
            .expect("preflight fallback leaves control writable")
            .succeeded()
    );
    events.shutdown().await.expect("control client stops");
    guard.shutdown().await.expect("fixture stops");
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

#[tokio::test]
async fn a_session_handle_and_a_separate_listing_return_owned_core_snapshots() {
    use libtmux_core::SessionSnapshot;

    let fixture = TestServer::new().await.unwrap();
    let mut session = fixture.session("original").await.unwrap();
    let before: SessionSnapshot = session.snapshot().clone();
    session.rename("changed").await.unwrap();
    let rows = fixture
        .server()
        .snapshots::<SessionSnapshot>()
        .await
        .unwrap();
    let after = rows
        .iter()
        .find(|row| row.session_id() == before.session_id())
        .unwrap();
    assert_eq!(before.session_name().as_bytes(), b"original");
    assert_eq!(after.session_name().as_bytes(), b"changed");
    assert_eq!(session.snapshot().session_name(), after.session_name());
    fixture.shutdown().await.unwrap();
}
