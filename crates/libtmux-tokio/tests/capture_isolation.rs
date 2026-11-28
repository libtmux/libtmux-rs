//! Live pane observations exclude hook output for every supported capture shape.

#![cfg(feature = "test-support")]
#![allow(clippy::expect_used)]

use std::time::Duration;

use libtmux_tokio::test::{TestServer, retry_until, scaled};
use libtmux_tokio::{CaptureOptions, Command, NewSession, PaneWait, Server, TmuxText};

async fn hook(server: &Server, event: &str, value: Option<&str>) {
    let mut request = Command::new("set-hook")
        .arg(if value.is_some() { "-g" } else { "-gu" })
        .arg(event);
    if let Some(value) = value {
        request = request.sensitive_arg(value);
    }
    assert!(server.cmd(request).await.expect("hook changes").success());
}

fn native_bytes(lines: &[TmuxText]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    if lines.is_empty() {
        bytes.push(b'\n');
    }
    bytes
}

#[tokio::test]
async fn live_capture_keeps_native_options_without_hook_bytes() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    let session = guard
        .session(
            NewSession::new("capture-options")
                .size(24, 6)
                .command("i=0; while [ \"$i\" -lt 12 ]; do printf 'history-%02d\\n' \"$i\"; i=$((i + 1)); done; printf 'a-long-wrapped-line-spanning-several-rows-with-a-tail\\n\\033[31mcoloured\\134literal\\033[0m\\nready-grid\\n'; exec sleep 60"),
        )
        .await
        .expect("pane is created");
    let pane = session.panes().await.expect("panes list").remove(0);
    assert_eq!(
        pane.wait_for_text("ready-grid", scaled(Duration::from_secs(5)))
            .await
            .expect("pane prints"),
        PaneWait::Arrived
    );
    let cases: &[(CaptureOptions, &[&str])] = &[
        (CaptureOptions::visible(), &[]),
        (CaptureOptions::visible().escape_sequences(), &["-e"]),
        (CaptureOptions::visible().trailing_spaces(), &["-N"]),
        (CaptureOptions::visible().join_wrapped(), &["-J"]),
        (CaptureOptions::history(), &["-S", "-"]),
        (CaptureOptions::history().join_wrapped(), &["-S", "-", "-J"]),
        (
            CaptureOptions::visible().start(-2).end(1),
            &["-S", "-2", "-E", "1"],
        ),
        (
            CaptureOptions::visible().start(3).end(-3),
            &["-S", "3", "-E", "-3"],
        ),
    ];
    for (options, arguments) in cases {
        let mut command = Command::new("capture-pane")
            .arg("-p")
            .arg("-t")
            .arg(pane.id().to_string());
        for argument in *arguments {
            command = command.arg(*argument);
        }
        let expected = server.cmd(command).await.expect("native capture runs");
        assert!(expected.success());
        hook(
            server,
            "after-capture-pane",
            Some("display-message -p private-capture-hook"),
        )
        .await;
        let captured = pane.capture_with(*options).await;
        hook(server, "after-capture-pane", None).await;
        assert_eq!(
            native_bytes(&captured.expect("capture succeeds")),
            expected.stdout(),
            "{arguments:?}"
        );
    }
    guard.shutdown().await.expect("fixture stops");
}

#[tokio::test]
async fn pending_capture_keeps_nul_and_backslashes_without_hook_bytes() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    let session =
        guard
            .session(NewSession::new("pending-capture").command(
                "printf 'ready-pending\\n\\033]0;part\\000after\\134slash'; exec sleep 60",
            ))
            .await
            .expect("pane is created");
    let pane = session.panes().await.expect("panes list").remove(0);
    assert_eq!(
        pane.wait_for_text("ready-pending", scaled(Duration::from_secs(5)))
            .await
            .expect("pane prints"),
        PaneWait::Arrived
    );
    let expected = server
        .cmd(
            Command::new("capture-pane")
                .arg("-pP")
                .arg("-t")
                .arg(pane.id().to_string()),
        )
        .await
        .expect("ordinary pending capture runs");
    assert!(expected.success());
    assert_eq!(expected.stdout(), b"\x1b]0;part\0after\\slash\n");
    hook(
        server,
        "after-capture-pane",
        Some("display-message -p private-pending-hook"),
    )
    .await;
    let captured = pane
        .capture_with(CaptureOptions::visible().pending_escape())
        .await;
    hook(server, "after-capture-pane", None).await;
    assert_eq!(
        native_bytes(&captured.expect("pending capture succeeds")),
        expected.stdout()
    );
    guard.shutdown().await.expect("fixture stops");
}

#[tokio::test]
async fn pane_waits_ignore_capture_and_display_hooks() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    let session = guard
        .session(NewSession::new("wait-hooks").command("printf 'actual-grid\\n'; exec sleep 60"))
        .await
        .expect("pane is created");
    let pane = session.panes().await.expect("panes list").remove(0);
    assert_eq!(
        pane.wait_for_text("actual-grid", scaled(Duration::from_secs(5)))
            .await
            .expect("pane prints"),
        PaneWait::Arrived
    );
    for event in ["after-capture-pane", "after-display-message"] {
        hook(server, event, Some("display-message -p hook-only-sentinel")).await;
    }
    let text = pane
        .wait_for_text("hook-only-sentinel", scaled(Duration::from_millis(150)))
        .await;
    let predicate = pane
        .wait_until(scaled(Duration::from_millis(150)), |lines| {
            lines
                .iter()
                .any(|line| line.as_bytes() == b"hook-only-sentinel")
        })
        .await;
    hook(
        server,
        "after-capture-pane",
        Some("display-message -p '#{client_pid}'"),
    )
    .await;
    hook(server, "after-display-message", None).await;
    let quiet = pane
        .wait_for_quiet(
            scaled(Duration::from_millis(50)),
            scaled(Duration::from_secs(2)),
        )
        .await;
    hook(server, "after-capture-pane", None).await;
    assert_eq!(text.expect("text wait finishes"), PaneWait::TimedOut);
    assert_eq!(
        predicate.expect("predicate wait finishes"),
        PaneWait::TimedOut
    );
    assert_eq!(quiet.expect("quiet wait finishes"), PaneWait::Arrived);
    guard.shutdown().await.expect("fixture stops");
}

#[tokio::test]
async fn joined_capture_checks_hooks_for_an_unqualified_client_pair() {
    let guard = TestServer::new().await.expect("tmux starts");
    let native = guard.server();
    let session = guard
        .session(NewSession::new("mixed-capture").command("printf 'mixed-grid\\n'; exec sleep 60"))
        .await
        .expect("pane is created");
    let pane = session.panes().await.expect("panes list").remove(0);
    assert_eq!(
        pane.wait_for_text("mixed-grid", scaled(Duration::from_secs(5)))
            .await
            .expect("pane prints"),
        PaneWait::Arrived
    );
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned wrapper directory");
    let wrapper = directory.path().join("reported-client");
    let executable = native
        .resolved_tmux_executable()
        .expect("native client resolves");
    let quoted = format!(
        "'{}'",
        executable
            .to_str()
            .expect("fixture path is UTF-8")
            .replace('\'', "'\\''")
    );
    let report = if native.daemon_version().await.expect("daemon version").raw() == "3.3a" {
        "3.8"
    } else {
        "3.3a"
    };
    libtmux_tokio::test::install_executable(&wrapper, &format!("#!/bin/sh\nif [ \"$1\" = -V ]; then printf 'tmux {report}\\n'; exit 0; fi\nexec {quoted} \"$@\"\n")).expect("wrapper is executable");
    let server = Server::builder()
        .socket_path(native.socket_path())
        .tmux_executable(&wrapper)
        .build()
        .expect("unqualified client configured");
    let pane = server
        .pane_by_id(pane.id())
        .await
        .expect("pane lookup")
        .expect("pane exists");
    let options = CaptureOptions::history().join_wrapped();
    let expected = pane
        .capture_with(options)
        .await
        .expect("unhooked mixed capture succeeds");
    assert!(expected.iter().any(|line| line.as_bytes() == b"mixed-grid"));
    hook(
        native,
        "after-capture-pane[17]",
        Some("display-message -p private-mixed-hook"),
    )
    .await;
    let captured = pane.capture_with(options).await;
    let waited = pane
        .wait_for_text("private-mixed-hook", scaled(Duration::from_millis(100)))
        .await;
    hook(native, "after-capture-pane", None).await;
    for error in [
        captured.expect_err("configured hooks have no mixed-client byte boundary"),
        waited.expect_err("a wait cannot match hook bytes"),
    ] {
        assert!(
            matches!(error, libtmux_tokio::Error::CoreOperation { ref source } if matches!(source.as_ref(), libtmux_core::Error::JoinedCaptureHook))
        );
        assert!(!format!("{error} {error:?}").contains("private-mixed-hook"));
    }
    assert_eq!(
        pane.capture_with(options)
            .await
            .expect("capture resumes once hook is unset"),
        expected
    );
    server.shutdown().await.expect("wrapper client stops");
    guard.shutdown().await.expect("fixture stops");
}

#[tokio::test]
async fn retained_and_missing_panes_keep_their_wait_outcomes_with_hooks() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    guard.session("keeper").await.expect("daemon stays alive");
    assert!(
        server
            .cmd(
                Command::new("set-option")
                    .arg("-g")
                    .arg("remain-on-exit")
                    .arg("on")
            )
            .await
            .expect("retain exited panes")
            .success()
    );
    let session = guard
        .session(
            NewSession::new("terminal-capture")
                .environment(
                    "LIBTMUX_NATIVE",
                    server
                        .resolved_tmux_executable()
                        .expect("client resolves")
                        .into_os_string(),
                )
                .command(
                    "printf 'final-grid\\n'; \"$LIBTMUX_NATIVE\" wait-for terminal-release; exit 0",
                ),
        )
        .await
        .expect("terminal pane starts");
    let pane = session.panes().await.expect("panes list").remove(0);
    assert_eq!(
        pane.wait_for_text("final-grid", scaled(Duration::from_secs(5)))
            .await
            .expect("pane prints before exiting"),
        PaneWait::Arrived
    );
    server
        .signal_channel("terminal-release")
        .await
        .expect("pane can exit");
    retry_until(scaled(Duration::from_secs(5)), async || {
        pane.format("#{pane_dead}")
            .await
            .is_ok_and(|value| value.as_bytes() == b"1")
    })
    .await
    .expect("the native process is dead");
    for event in ["after-capture-pane", "after-display-message"] {
        hook(server, event, Some("display-message -p private-dead-hook")).await;
    }
    let present = pane
        .wait_for_text("final-grid", scaled(Duration::from_secs(1)))
        .await;
    let absent = pane
        .wait_for_text("not-on-screen", scaled(Duration::from_secs(1)))
        .await;
    for event in ["after-capture-pane", "after-display-message"] {
        hook(server, event, None).await;
    }
    assert_eq!(
        present.expect("retained text is readable"),
        PaneWait::Arrived
    );
    assert_eq!(
        absent.expect("the process cannot print more"),
        PaneWait::Dead
    );
    pane.clone().kill().await.expect("pane is killed");
    for options in [
        CaptureOptions::visible(),
        CaptureOptions::history().join_wrapped(),
        CaptureOptions::visible().pending_escape(),
    ] {
        let error = pane
            .capture_with(options)
            .await
            .expect_err("a killed pane is absent");
        assert!(error.is_object_gone(), "{options:?}: {error:?}");
    }
    assert_eq!(
        pane.wait_for_text("anything", scaled(Duration::from_secs(1)))
            .await
            .expect("missing pane is settled"),
        PaneWait::Dead
    );
    guard.shutdown().await.expect("fixture stops");
}

#[tokio::test]
async fn failed_capture_hooks_never_expose_pane_or_hook_bytes() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    let session = guard
        .session(
            NewSession::new("capture-refused").command("printf 'private-screen\\n'; exec sleep 60"),
        )
        .await
        .expect("pane starts");
    let pane = session.panes().await.expect("panes list").remove(0);
    assert_eq!(
        pane.wait_for_text("private-screen", scaled(Duration::from_secs(5)))
            .await
            .expect("pane prints"),
        PaneWait::Arrived
    );
    hook(
        server,
        "after-capture-pane",
        Some("display-message -p private-failed-hook ; select-pane -t %4294967295"),
    )
    .await;
    let mut failures = Vec::new();
    for options in [
        CaptureOptions::visible(),
        CaptureOptions::history(),
        CaptureOptions::history().join_wrapped(),
        CaptureOptions::visible().pending_escape(),
    ] {
        failures.push(
            pane.capture_with(options)
                .await
                .expect_err("the native hook fails"),
        );
    }
    hook(server, "after-capture-pane", None).await;
    for error in failures {
        assert_eq!(
            error.operation_effect(),
            Some(libtmux_core::Effect::Observation)
        );
        let diagnostic = format!("{error} {error:?}");
        assert!(!diagnostic.contains("private-screen"));
        assert!(!diagnostic.contains("private-failed-hook"));
    }
    guard.shutdown().await.expect("fixture stops");
}
