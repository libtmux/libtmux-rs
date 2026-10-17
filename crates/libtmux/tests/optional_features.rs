//! Native feature boundaries preserve ordinary operations on the core floor.

#![cfg(feature = "test-support")]
#![allow(clippy::expect_used)]

use std::path::PathBuf;

use libtmux::test::TestServer;
use libtmux::{Command, CommandChain, Error, ReleaseSuffix, ReleaseVersion};

/// Read a native-binary variable. A lane that lists the variable in
/// `LIBTMUX_REQUIRE_NATIVE` must have set it: skipping there would pass
/// without exercising the boundary the lane exists to test.
fn binary(variable: &str) -> Option<PathBuf> {
    let found = std::env::var_os(variable);
    assert!(
        found.is_some()
            || !std::env::var("LIBTMUX_REQUIRE_NATIVE")
                .is_ok_and(|required| required.split(',').any(|name| name == variable)),
        "{variable} is listed in LIBTMUX_REQUIRE_NATIVE but is unset"
    );
    found.map(PathBuf::from)
}

#[tokio::test]
async fn real_tmux_compat_optional_layout_refuses_before_a_prior_effect() {
    let Some(minimum) = binary("LIBTMUX_TEST_MINIMUM_TMUX") else {
        return;
    };
    for (name, arguments) in [
        ("select-layout", vec!["not-a-layout"]),
        ("selectl", vec!["--", "not-a-layout"]),
        ("select-l", vec!["-o", "tiled"]),
        ("select-la", vec!["-t", "-n", "not-a-layout"]),
    ] {
        let guard = TestServer::builder()
            .tmux_executable(&minimum)
            .start()
            .await
            .expect("3.3a starts");
        guard.server().new_session("keeper").await.expect("session");
        let marker = guard
            .socket_path()
            .with_extension("forbidden-layout-effect");
        let layout = arguments.into_iter().fold(Command::new(name), Command::arg);
        let result = guard
            .server()
            .chain(
                CommandChain::new(
                    Command::new("run-shell").arg(format!("touch '{}'", marker.display())),
                )
                .then(layout),
            )
            .await;
        let live = guard.server().format(None, "#{pid}").await;
        let effect = marker.exists();
        let cleanup = guard.shutdown().await;
        let _ = std::fs::remove_file(&marker);
        assert!(!effect, "{name}: a prior batch effect ran");
        assert!(live.is_ok(), "{name}: the daemon stopped: {live:?}");
        cleanup.expect("qualified fixture closes");
        assert!(matches!(result, Err(Error::InvalidLayout { .. })));
    }
}

#[tokio::test]
async fn real_tmux_compat_optional_layout_keeps_safe_native_forms() {
    for variable in ["LIBTMUX_TEST_MINIMUM_TMUX", "LIBTMUX_TEST_SAFE_BUFFER_TMUX"] {
        let Some(binary) = binary(variable) else {
            continue;
        };
        let guard = TestServer::builder()
            .tmux_executable(binary)
            .start()
            .await
            .expect("supported daemon starts");
        guard.server().new_session("keeper").await.expect("session");
        for arguments in [
            vec!["ti"],
            vec!["-n", "not-a-layout"],
            vec!["-p", "not-a-layout"],
            vec!["-E", "not-a-layout"],
            vec![],
            vec!["-o"],
        ] {
            let command = arguments
                .into_iter()
                .fold(Command::new("selectl"), Command::arg);
            assert!(
                guard
                    .server()
                    .cmd(command)
                    .await
                    .expect("native form")
                    .success()
            );
        }
        let invalid_tree = guard
            .server()
            .cmd(Command::new("select-layout").arg("0000,not-a-tree"))
            .await
            .expect("a serialized header retains native validation");
        assert!(!invalid_tree.success());
        if variable == "LIBTMUX_TEST_SAFE_BUFFER_TMUX" {
            let invalid_name = guard
                .server()
                .cmd(Command::new("select-layout").arg("not-a-layout"))
                .await
                .expect("3.4 initializes its error cause");
            assert!(!invalid_name.success());
        }
        guard
            .server()
            .format(None, "#{pid}")
            .await
            .expect("daemon remains live");
        guard.shutdown().await.expect("fixture closes");
    }
}

async fn flag_shaped_buffer_target(batch: bool) {
    let Some(minimum) = binary("LIBTMUX_TEST_MINIMUM_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(minimum)
        .start()
        .await
        .expect("3.3a starts");
    let server = guard.server();
    server.new_session("keeper").await.expect("session");
    server
        .set_buffer(Some("sentinel"), "retained")
        .await
        .expect("sentinel buffer");
    let rename = Command::new("set-buffer")
        .arg("-t")
        .arg("-n")
        .arg("-b")
        .arg("absent")
        .arg("-n")
        .arg("renamed");
    let result = if batch {
        server
            .chain(
                CommandChain::new(
                    Command::new("set-option")
                        .arg("-g")
                        .arg("@must-not-run")
                        .arg("yes"),
                )
                .then(rename),
            )
            .await
    } else {
        server.cmd(rename).await
    };
    let retained = server.buffer("sentinel").await.expect("sentinel read");
    let renamed = server.buffer("renamed").await.expect("renamed read");
    let earlier_effect = server
        .cmd(Command::new("show-options").arg("-gv").arg("@must-not-run"))
        .await
        .expect("option read")
        .success();
    guard.shutdown().await.expect("fixture closes");
    assert_eq!(retained, Some(b"retained".to_vec()), "sentinel was renamed");
    assert!(
        renamed.is_none(),
        "missing name never falls back to top buffer"
    );
    assert!(
        !earlier_effect,
        "the entire batch refuses before its first effect"
    );
    assert!(matches!(result, Err(Error::UnsupportedCapability { .. })));
}

#[tokio::test]
async fn real_tmux_compat_optional_scanner_buffer_target_operand_is_not_a_flag() {
    flag_shaped_buffer_target(false).await;
}

#[tokio::test]
async fn real_tmux_compat_optional_scanner_buffer_target_guards_the_whole_batch() {
    flag_shaped_buffer_target(true).await;
}

#[tokio::test]
async fn real_tmux_compat_optional_scanner_split_percent_refuses_before_a_prior_effect() {
    let Some(binary) = binary("LIBTMUX_TEST_SAFE_BUFFER_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(binary)
        .start()
        .await
        .expect("3.4 starts");
    let server = guard.server();
    let session = server.new_session("keeper").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);
    let mut observed = Vec::new();
    for name in ["split-window", "splitw", "sp", "split-"] {
        let result = server
            .chain(
                CommandChain::new(
                    Command::new("set-option")
                        .arg("-g")
                        .arg("@must-not-run")
                        .arg("yes"),
                )
                .then(
                    Command::new(name)
                        .arg("-d")
                        .arg("-t")
                        .arg(pane.id().to_string())
                        .arg("-p")
                        .arg("50"),
                ),
            )
            .await;
        let earlier_effect = server
            .cmd(Command::new("show-options").arg("-gv").arg("@must-not-run"))
            .await
            .expect("option read")
            .success();
        observed.push((name, earlier_effect, result));
        server
            .cmd(Command::new("set-option").arg("-gu").arg("@must-not-run"))
            .await
            .expect("witness reset");
    }
    pane.split(
        libtmux::SplitOptions::new(libtmux::SplitDirection::Right)
            .size(libtmux::PaneSize::Percent(50))
            .command("exec sleep 60"),
    )
    .await
    .expect("typed percentage uses the working -l N% form");
    assert_eq!(session.panes().await.expect("panes").len(), 2);
    guard.shutdown().await.expect("fixture closes");
    for (name, earlier_effect, result) in observed {
        assert!(!earlier_effect, "{name}: a prior batch mutation ran");
        assert!(matches!(result, Err(Error::CapabilityDefective { .. })));
    }
}

/// Run `earlier ; later` and report whether `earlier` took effect, so a
/// refusal can be told apart from a dispatch that merely failed.
async fn chain_marker(server: &libtmux::Server, later: Command) -> (bool, Result<(), Error>) {
    let result = server
        .chain(
            CommandChain::new(
                Command::new("set-option")
                    .arg("-g")
                    .arg("@must-not-run")
                    .arg("yes"),
            )
            .then(later),
        )
        .await
        .map(|_| ());
    let ran = server
        .cmd(Command::new("show-options").arg("-gv").arg("@must-not-run"))
        .await
        .expect("option read")
        .success();
    server
        .cmd(Command::new("set-option").arg("-gu").arg("@must-not-run"))
        .await
        .expect("witness reset");
    (ran, result)
}

#[tokio::test]
async fn real_tmux_compat_optional_literal_capture_flags_refuse_before_a_prior_effect() {
    let Some(minimum) = binary("LIBTMUX_TEST_MINIMUM_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(minimum)
        .start()
        .await
        .expect("3.3a starts");
    let server = guard.server();
    let session = server.new_session("keeper").await.expect("session");
    let pane = session
        .panes()
        .await
        .expect("panes")
        .remove(0)
        .id()
        .to_string();
    let capture = |flag: &str| {
        Command::new("capture-pane")
            .arg("-p")
            .arg(flag.to_owned())
            .arg("-t")
            .arg(pane.clone())
    };
    let mut observed = Vec::new();
    for flag in ["-T", "-F"] {
        observed.push((flag, chain_marker(server, capture(flag)).await));
    }
    let control = chain_marker(server, capture("-J")).await;
    guard.shutdown().await.expect("fixture closes");
    for (flag, (earlier_effect, result)) in observed {
        assert!(!earlier_effect, "{flag}: a prior batch mutation ran");
        assert!(
            matches!(result, Err(Error::UnsupportedCapability { .. })),
            "{flag} refuses before dispatch"
        );
    }
    assert!(
        control.0,
        "an allowed capture flag leaves the chain running"
    );
    assert!(control.1.is_ok(), "an allowed capture flag is dispatched");
}

#[tokio::test]
async fn real_tmux_compat_optional_literal_refresh_client_pane_off_refuses_before_a_prior_effect() {
    let Some(minimum) = binary("LIBTMUX_TEST_MINIMUM_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(minimum)
        .start()
        .await
        .expect("3.3a starts");
    let server = guard.server();
    server.new_session("keeper").await.expect("session");
    let refresh = |state: &str| {
        Command::new("refresh-client")
            .arg("-A")
            .arg(format!("%0:{state}"))
    };
    let refused = chain_marker(server, refresh("off")).await;
    let control = chain_marker(server, refresh("on")).await;
    guard.shutdown().await.expect("fixture closes");
    assert!(!refused.0, "a prior batch mutation ran");
    assert!(
        matches!(refused.1, Err(Error::UnsupportedCapability { .. })),
        "pane off refuses before dispatch"
    );
    assert!(control.0, "pane on leaves the chain running");
}

#[tokio::test]
async fn real_tmux_compat_optional_named_buffer_operations_refuse_before_collateral_effects() {
    let Some(minimum) = binary("LIBTMUX_TEST_MINIMUM_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(minimum)
        .start()
        .await
        .expect("3.3a starts");
    let server = guard.server();
    server.new_session("keeper").await.expect("basic creation");
    server
        .set_buffer(Some("sentinel"), "retained")
        .await
        .expect("ordinary named creation works");
    for name in ["delete-buffer", "deleteb", "del", "delete-"] {
        let result = server.cmd(Command::new(name).arg("-bmissing")).await;
        assert_eq!(
            server.buffer("sentinel").await.expect("buffer read"),
            Some(b"retained".to_vec()),
            "{name} must not delete the unrelated top buffer"
        );
        assert!(
            matches!(result, Err(Error::UnsupportedCapability { .. })),
            "{name} refuses before dispatch"
        );
    }
    let result = server.delete_buffer("missing").await;
    assert!(
        server
            .buffer("sentinel")
            .await
            .expect("buffer read")
            .is_some()
    );
    assert!(matches!(result, Err(Error::UnsupportedCapability { .. })));
    for name in ["set-buffer", "setb", "set-b"] {
        let result = server
            .cmd(Command::new(name).arg("-bmissing").arg("-nrenamed"))
            .await;
        assert!(
            server
                .buffer("sentinel")
                .await
                .expect("buffer read")
                .is_some(),
            "{name} must not rename the unrelated top buffer"
        );
        assert!(
            server
                .buffer("renamed")
                .await
                .expect("buffer read")
                .is_none()
        );
        assert!(matches!(result, Err(Error::UnsupportedCapability { .. })));
    }
    let chain = CommandChain::new(
        Command::new("set-buffer")
            .arg("-b")
            .arg("must-not-stage")
            .arg("payload"),
    )
    .then(Command::new("delete-buffer").arg("-b").arg("missing"));
    let result = server.chain(chain).await;
    assert!(
        server
            .buffer("must-not-stage")
            .await
            .expect("buffer read")
            .is_none(),
        "whole flat batch refuses before its first effect"
    );
    assert!(matches!(result, Err(Error::UnsupportedCapability { .. })));
    server
        .set_buffer(Some("literal"), "-n")
        .await
        .expect("payload is not parsed as flags");
    guard.shutdown().await.expect("fixture closes");
}

#[tokio::test]
async fn real_tmux_compat_optional_staging_requirement_is_carried_by_the_entire_chain() {
    let Some(minimum) = binary("LIBTMUX_TEST_MINIMUM_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(minimum)
        .start()
        .await
        .expect("3.3a starts");
    let server = guard.server();
    server.new_session("keeper").await.expect("basic creation");
    let requirement = ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL);
    let chain = CommandChain::new(
        Command::new("set-buffer")
            .arg("-b")
            .arg("first")
            .arg("payload"),
    )
    .then(
        Command::new("set-buffer")
            .arg("-b")
            .arg("staged")
            .arg("payload")
            .requires_tmux("temporary buffer abort cleanup", requirement),
    );
    let result = server.chain(chain).await;
    assert!(
        server.buffer("first").await.expect("buffer read").is_none(),
        "no earlier chain member ran"
    );
    assert!(
        server
            .buffer("staged")
            .await
            .expect("buffer read")
            .is_none(),
        "no unsafe staging ran"
    );
    assert!(matches!(result, Err(Error::UnsupportedCapability { .. })));
    guard.shutdown().await.expect("fixture closes");
}

#[tokio::test]
async fn real_tmux_compat_optional_named_paste_and_delete_is_safe_on_3_3a() {
    let Some(minimum) = binary("LIBTMUX_TEST_MINIMUM_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(minimum)
        .start()
        .await
        .expect("3.3a starts");
    let server = guard.server();
    let session = server.new_session("keeper").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);
    let original_window = pane.window_id().clone();
    let native = server
        .cmd(
            Command::new("break-pane")
                .arg("-d")
                .arg("-P")
                .arg("-F")
                .arg("#{window_id}")
                .arg("-s")
                .arg(pane.id().to_string())
                .arg("-n")
                .arg("retained-name"),
        )
        .await
        .expect("old single-pane break answers");
    assert!(native.success());
    assert!(
        native.stdout().is_empty(),
        "old single-pane branch has no printed ID"
    );
    let refreshed = pane
        .clone()
        .break_out()
        .await
        .expect("typed break refreshes by pane ID");
    assert_eq!(refreshed.id(), pane.id());
    assert_eq!(refreshed.window_id(), &original_window);
    assert_eq!(
        server
            .window_by_id(&original_window)
            .await
            .expect("window lookup")
            .expect("window remains")
            .name(),
        "retained-name"
    );
    server
        .set_buffer(Some("sentinel"), "retained")
        .await
        .expect("sentinel");
    let missing = server
        .cmd(
            Command::new("paste-buffer")
                .arg("-d")
                .arg("-bmissing")
                .arg("-t")
                .arg(pane.id().to_string()),
        )
        .await
        .expect("native refusal is data");
    assert!(!missing.success());
    assert!(
        server
            .buffer("sentinel")
            .await
            .expect("sentinel survives")
            .is_some()
    );
    let pasted = server
        .cmd(
            Command::new("paste-buffer")
                .arg("-d")
                .arg("-bsentinel")
                .arg("-t")
                .arg(pane.id().to_string()),
        )
        .await
        .expect("safe direct paste");
    assert!(pasted.success());
    assert!(
        server
            .buffer("sentinel")
            .await
            .expect("only selected buffer deleted")
            .is_none()
    );
    guard.shutdown().await.expect("fixture closes");
}

#[tokio::test]
async fn real_tmux_compat_optional_named_buffers_work_on_3_4() {
    let Some(binary) = binary("LIBTMUX_TEST_SAFE_BUFFER_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(binary)
        .start()
        .await
        .expect("3.4 starts");
    let server = guard.server();
    server.new_session("keeper").await.expect("session");
    server
        .set_buffer(Some("sentinel"), "retained")
        .await
        .expect("buffer");
    let missing = server
        .cmd(Command::new("set-buffer").arg("-bmissing").arg("-nrenamed"))
        .await
        .expect("native missing rename result");
    assert!(!missing.success());
    assert!(
        server
            .buffer("sentinel")
            .await
            .expect("top survives")
            .is_some()
    );
    assert!(
        server
            .cmd(
                Command::new("set-buffer")
                    .arg("-bsentinel")
                    .arg("-nrenamed")
            )
            .await
            .expect("qualified rename")
            .success()
    );
    server
        .delete_buffer("renamed")
        .await
        .expect("qualified exact deletion");
    assert!(
        server
            .buffer("renamed")
            .await
            .expect("buffer read")
            .is_none()
    );
    guard.shutdown().await.expect("fixture closes");
}

#[tokio::test]
async fn real_tmux_compat_optional_exact_3_7_break_pane_refuses_all_literal_forms() {
    let Some(binary) = binary("LIBTMUX_TEST_BROKEN_BREAK_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(binary)
        .start()
        .await
        .expect("exact3.7 starts");
    let server = guard.server();
    let session = server.new_session("keeper").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);
    server
        .cmd(
            Command::new("split-window")
                .arg("-d")
                .arg("-t")
                .arg(pane.id().to_string())
                .arg("exec sleep 300"),
        )
        .await
        .expect("second pane");
    for name in ["break-pane", "breakp", "br", "break-"] {
        let result = server
            .cmd(
                Command::new(name)
                    .arg("-d")
                    .arg("-s")
                    .arg(pane.id().to_string()),
            )
            .await;
        assert_eq!(
            session
                .panes()
                .await
                .expect("daemon and panes survive")
                .len(),
            2,
            "{name} must not move or crash"
        );
        assert!(matches!(result, Err(Error::CapabilityDefective { .. })));
        let named = server
            .cmd(
                Command::new(name)
                    .arg("-d")
                    .arg("-s")
                    .arg(pane.id().to_string())
                    .arg("-n")
                    .arg("requested-name"),
            )
            .await;
        assert!(
            matches!(named, Err(Error::CapabilityDefective { .. })),
            "explicit naming alone does not repair3.7"
        );
    }
    assert!(matches!(
        pane.clone().break_out().await,
        Err(Error::CapabilityDefective { .. })
    ));
    #[cfg(feature = "control-mode")]
    {
        let control = libtmux::control::ControlMode::attach(server, session.id())
            .await
            .expect("3.7 control acquisition is qualified");
        let result = control
            .send(
                Command::new("breakp")
                    .arg("-d")
                    .arg("-s")
                    .arg(pane.id().to_string())
                    .arg("-n")
                    .arg("requested-name"),
            )
            .await;
        let windows = session.windows().await.expect("windows survive").len();
        control.shutdown().await.expect("control closes");
        assert_eq!(windows, 1, "direct sender refused before creating a window");
        assert!(matches!(result, Err(Error::CapabilityDefective { .. })));
        assert_eq!(session.panes().await.expect("panes survive").len(), 2);
    }
    guard.shutdown().await.expect("fixture closes");
}

#[tokio::test]
async fn real_tmux_compat_optional_shell_output_uses_the_daemon_release() {
    let Some(minimum) = binary("LIBTMUX_TEST_MINIMUM_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(minimum)
        .start()
        .await
        .expect("3.3a starts");
    guard.server().new_session("keeper").await.expect("session");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned effect witness");
    let marker = directory.path().join("effect");
    let modern = std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into());
    let server = libtmux::Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(modern)
        .build()
        .expect("newer client");
    let result = server
        .run_shell(format!("touch '{}'", marker.display()))
        .await;
    assert!(
        !marker.exists(),
        "no shell effect ran before refusing broken output"
    );
    assert!(matches!(result, Err(Error::CapabilityDefective { .. })));
    server.shutdown().await.expect("client closes");
    guard.shutdown().await.expect("fixture closes");
}

#[cfg(feature = "control-mode")]
#[tokio::test]
async fn real_tmux_compat_optional_control_uses_daemon_defects_with_a_newer_client() {
    let (Some(client_binary), Some(modern)) = (
        binary("LIBTMUX_TEST_NEWER_TMUX"),
        binary("LIBTMUX_TEST_BROKEN_BREAK_TMUX"),
    ) else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(modern)
        .start()
        .await
        .expect("3.7 starts");
    let session = guard
        .server()
        .new_session("mixed-control")
        .await
        .expect("session");
    let pane = session.panes().await.expect("panes").remove(0);
    guard
        .server()
        .cmd(
            Command::new("split-window")
                .arg("-d")
                .arg("-t")
                .arg(pane.id().to_string())
                .arg("exec sleep 300"),
        )
        .await
        .expect("second pane");
    let client = libtmux::Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(client_binary)
        .build()
        .expect("newer client");
    assert!(
        client
            .capabilities()
            .await
            .expect("diagnostic release")
            .tmux_version()
            .has_behavior(&ReleaseVersion::new(
                3,
                7,
                ReleaseSuffix::patch('a').expect("patch letter"),
            )),
        "the configured client reports the fixed break-pane release"
    );
    let control = libtmux::control::ControlMode::attach(&client, session.id())
        .await
        .expect("3.7 daemon qualifies with newer client");
    let captured = control
        .send(
            Command::new("capture-pane")
                .arg("-p")
                .arg("-F")
                .arg("-t")
                .arg(pane.id().to_string()),
        )
        .await;
    let refused = control
        .send(
            Command::new("break-pane")
                .arg("-d")
                .arg("-s")
                .arg(pane.id().to_string())
                .arg("-n")
                .arg("requested"),
        )
        .await;
    let windows = session.windows().await.expect("window count").len();
    control.shutdown().await.expect("control closes");
    client.shutdown().await.expect("client closes");
    guard.shutdown().await.expect("fixture closes");
    assert!(captured.expect("daemon capture flag qualifies").succeeded());
    assert!(
        matches!(refused, Err(Error::CapabilityDefective { .. })),
        "actual3.7 defect is refused"
    );
    assert_eq!(windows, 1, "refused break did not create a window");
}
