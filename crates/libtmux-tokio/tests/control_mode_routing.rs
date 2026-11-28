//! A recording tmux executable distinguishes control requests from ordinary clients.

#![cfg(all(feature = "control-mode", feature = "test-support"))]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used
)]

#[path = "support/versions.rs"]
mod versions;

use std::fs;
use std::path::Path;
use std::time::Duration;

use libtmux_tokio::control::ControlMode;
use libtmux_tokio::test::TestServer;
use libtmux_tokio::{Command, CommandChain, Server};

fn recording_tmux(directory: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let executable = directory.join("recording-client");
    let log = directory.join("recording-client.log");
    libtmux_tokio::test::install_executable(
        &executable,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$0.log\"\nexec \"${LIBTMUX_TEST_TMUX:-tmux}\" \"$@\"\n",
    )
    .expect("install the recording client");
    fs::write(&log, "").expect("initialize the process log");
    (executable, log)
}

fn spawned(log: &Path) -> Vec<String> {
    fs::read_to_string(log)
        .expect("the log is readable")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn the_handle_selects_one_route_for_each_command_or_chain() {
    let guard = TestServer::new().await.expect("a private tmux starts");
    let Some(guard) = versions::control_fixture(guard).await else {
        return;
    };
    let real = guard.server().clone();
    let session = guard
        .session("routed")
        .await
        .expect("the fixture holds a session");
    // The connection belongs to the caller. Everything below borrows it.
    let control = ControlMode::attach(&real, session.id())
        .await
        .expect("a control client attaches");
    let (sender, events) = control.split();

    let directory = tempfile::tempdir().expect("a scratch directory");
    let (executable, log) = recording_tmux(directory.path());
    let probe = Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(&executable)
        .build()
        .expect("a server pointed at the fixture's socket");

    let routed = probe
        .over_control_mode(&sender)
        .await
        .expect("the connection reaches the same server");

    let connected = spawned(&log);
    let projection = Command::new("display-message")
        .arg("-p")
        .arg("-t")
        .arg(session.id().to_string())
        .arg("#{session_id}");
    let projected = routed
        .cmd(projection)
        .await
        .expect("read the ID over control");
    assert_eq!(projected.stdout(), format!("{}\n", session.id()).as_bytes());
    assert_eq!(spawned(&log), connected);

    for flag in ["-V", "-h"] {
        let expected = real
            .cmd(Command::new(flag))
            .await
            .expect("native diagnostic");
        let actual = routed
            .cmd(Command::new(flag))
            .await
            .expect("routed diagnostic");
        assert_eq!(actual.exit_code(), expected.exit_code());
        assert_eq!(actual.stdout(), expected.stdout());
        assert_eq!(actual.stderr(), expected.stderr());
    }
    assert_eq!(spawned(&log).len(), connected.len() + 2);

    // A listing: the typed call renders a format plan and decodes rows.
    let names: Vec<String> = routed
        .sessions()
        .await
        .expect("the routed handle lists sessions")
        .iter()
        .map(|found| found.name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["routed"], "the rows came from the real daemon");

    // The second member prints arbitrary names. The chain needs one ordinary client.
    let window = session
        .new_window("second")
        .await
        .expect("the fixture can be changed through the connection's server");
    let before_chain = spawned(&log).len();
    let chained = routed
        .chain(
            CommandChain::new(
                Command::new("rename-window")
                    .arg("-t")
                    .arg(window.id().to_string())
                    .arg("renamed"),
            )
            .then(Command::new("list-windows").arg("-F").arg("#{window_name}")),
        )
        .await
        .expect("the chain is submitted");
    let invocations = spawned(&log);
    assert_eq!(invocations.len(), before_chain + 1);
    assert!(invocations[before_chain].contains("rename-window"));
    assert!(invocations[before_chain].contains("list-windows"));
    assert!(
        String::from_utf8_lossy(chained.stdout())
            .lines()
            .any(|name| name == "renamed"),
        "the chain's last command answered: {:?}",
        String::from_utf8_lossy(chained.stdout()),
    );

    // A chain stops at its first failure, and tmux sends no block for the
    // commands it skipped, so the next caller's reply is still its own.
    let stopped = routed
        .chain(
            CommandChain::new(
                Command::new("list-panes")
                    .arg("-t")
                    .arg("%4294967294")
                    .arg("-F")
                    .arg("#{pane_id}"),
            )
            .then(Command::new("display-message").arg("-p").arg("skipped")),
        )
        .await
        .expect("a refused chain still answers");
    assert!(!stopped.success(), "the first command was refused");
    assert!(
        stopped.stdout().is_empty(),
        "the skipped command printed nothing"
    );

    // A refusal is a result, not a transport error, exactly as it is for a
    // process: tmux answers the block with `%error`.
    let refused = routed
        .cmd(
            Command::new("list-panes")
                .arg("-t")
                .arg("%4294967294")
                .arg("-F")
                .arg("#{pane_id}"),
        )
        .await
        .expect("a refused command still answers");
    assert!(!refused.success(), "tmux refused the target");
    assert_eq!(refused.exit_code(), Some(1));
    assert!(
        !refused.stderr().is_empty(),
        "the refusal text lands where a process would have put it",
    );

    assert!(
        routed
            .pane_by_id(&"%4294967294".parse().expect("a well-formed pane id"))
            .await
            .expect("a filtered listing succeeds with no rows")
            .is_none(),
        "the predicate reached tmux intact and matched nothing",
    );

    let doomed = session
        .new_window("doomed")
        .await
        .expect("a window to take away again");
    let doomed_pane = doomed
        .active_pane()
        .await
        .expect("the new window reports its pane")
        .expect("a window has a pane");
    let routed_pane = routed
        .pane_by_id(doomed_pane.id())
        .await
        .expect("the routed handle finds it")
        .expect("it is still there");
    routed
        .cmd(
            Command::new("kill-window")
                .arg("-t")
                .arg(doomed.id().to_string()),
        )
        .await
        .expect("the window is killed over the connection");

    let gone = routed_pane
        .capture()
        .await
        .expect_err("capturing a pane that is gone fails");
    assert!(
        gone.is_object_gone(),
        "the vanished pane is classified on its capture route: {gone:?}",
    );

    let pane = routed
        .panes()
        .await
        .expect("the routed handle lists panes")
        .remove(0);
    pane.send_line("true")
        .await
        .expect("an inherited handle sends over the connection");

    // And the real daemon agrees the work happened.
    let windows = real.windows().await.expect("the fixture lists its windows");
    let window_names: Vec<String> = windows
        .iter()
        .map(|found| found.name().to_string_lossy().into_owned())
        .collect();
    assert!(
        window_names.contains(&"renamed".to_owned()),
        "the rename reached tmux: {window_names:?}",
    );

    events.shutdown().await.expect("the connection closes");
    guard.shutdown().await.expect("the fixture stops");
}

#[tokio::test]
async fn an_attached_client_can_signal_while_its_ordinary_client_waits() {
    let guard = TestServer::new().await.expect("a private tmux starts");
    let Some(guard) = versions::control_fixture(guard).await else {
        return;
    };
    let real = guard.server().clone();
    let session = guard
        .session("routed")
        .await
        .expect("the fixture holds a session");

    let control = ControlMode::attach(&real, session.id())
        .await
        .expect("a control client attaches");
    let (sender, events) = control.split();
    let routed = real
        .over_control_mode(&sender)
        .await
        .expect("the connection reaches the same server");

    assert_eq!(
        routed
            .wait_for_channel("handoff", Duration::from_millis(100))
            .await
            .unwrap(),
        libtmux_tokio::ChannelWait::TimedOut
    );
    routed.signal_channel("handoff").await.unwrap();
    assert_eq!(
        routed
            .wait_for_channel("handoff", Duration::from_secs(3))
            .await
            .unwrap(),
        libtmux_tokio::ChannelWait::Signalled
    );
    routed.lock_channel("handoff-lock").await.unwrap();
    routed.unlock_channel("handoff-lock").await.unwrap();
    assert_eq!(
        routed
            .sessions()
            .await
            .expect("the connection still answers")
            .len(),
        1,
    );

    events.shutdown().await.expect("the connection closes");
    guard.shutdown().await.expect("the fixture stops");
}

#[tokio::test]
async fn a_sender_for_another_server_is_refused() {
    let first = TestServer::new().await.expect("a private tmux starts");
    let Some(first) = versions::control_fixture(first).await else {
        return;
    };
    let second = TestServer::new()
        .await
        .expect("a second private tmux starts");
    let session = first
        .session("origin")
        .await
        .expect("the first fixture holds a session");

    let control = ControlMode::attach(first.server(), session.id())
        .await
        .expect("a control client attaches");
    let (sender, events) = control.split();

    let error = second
        .server()
        .over_control_mode(&sender)
        .await
        .expect_err("a sender reaching another server is refused");
    assert!(
        matches!(error, libtmux_tokio::Error::ServerMismatch { .. }),
        "the mismatch is reported rather than silently followed: {error:?}",
    );

    events.shutdown().await.expect("the connection closes");
    first.shutdown().await.expect("the first fixture stops");
    second.shutdown().await.expect("the second fixture stops");
}

/// A layout plan uses the daemon version qualified during acquisition.
#[cfg(feature = "plan")]
#[tokio::test]
async fn a_routed_plan_validates_layouts_without_asking_the_connection() {
    use libtmux_tokio::plan::{Plan, Planner, SelectLayout};

    let guard = TestServer::new().await.expect("a private tmux starts");
    let Some(guard) = versions::control_fixture(guard).await else {
        return;
    };
    let real = guard.server().clone();
    let session = guard.session("routed-plan").await.expect("session");
    let window = session.windows().await.expect("windows").remove(0);

    let control = ControlMode::attach(&real, session.id())
        .await
        .expect("a control client attaches");
    let (sender, events) = control.split();
    let directory = tempfile::tempdir().expect("a scratch directory");
    let (executable, log) = recording_tmux(directory.path());
    let probe = Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(&executable)
        .build()
        .expect("a server pointed at the fixture's socket");
    let routed = probe
        .over_control_mode(&sender)
        .await
        .expect("the connection reaches the same server");
    let before = spawned(&log);

    let mut plan = Plan::new();
    plan.add(SelectLayout::new(window.id().clone(), "tiled"));
    let result = routed
        .run_plan(&plan, Planner::Sequential)
        .await
        .expect("the routed plan runs");
    assert!(result.is_complete(), "{result:?}");
    for argv in &spawned(&log)[before.len()..] {
        assert_ne!(argv, "-V", "the executable version was already known");
        assert!(
            !argv.contains("libtmux-version-"),
            "unexpected version probe: {argv}"
        );
    }

    events.shutdown().await.expect("events shut down");
    guard.shutdown().await.expect("fixture shuts down");
}
