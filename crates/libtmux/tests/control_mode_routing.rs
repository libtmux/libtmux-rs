//! Typed calls routed over an open control-mode connection.
//!
//! The claim under test is that `Server::over_control_mode` moves the whole
//! typed API onto a connection someone else attached, and that it costs no
//! process to do it.
//!
//! Proving the second half needs a witness. The routed handle is built from a
//! server whose `tmux` executable is a shell stub that records every argv it
//! is given, so a dispatch that fell back to a process cannot go unnoticed:
//! it would appear in the stub's log, and the stub answers nothing but `-V`,
//! so the call would fail as well.

#![cfg(all(feature = "control-mode", feature = "test-support"))]
// The routing proof is one long function on purpose: the stub's log has to
// stay at one line across every dispatch, and splitting it would mean a second
// control client whose count says nothing about the first.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use libtmux::control::ControlMode;
use libtmux::test::TestServer;
use libtmux::{Command, CommandChain, Server};

/// Write a `tmux` that logs its argv, answers `-V`, and refuses the rest.
fn stub_tmux(directory: &Path, version: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let log = directory.join("spawned.log");
    let stub = directory.join("tmux");
    let script = format!(
        "#!/bin/sh\n\
         printf '%s\\n' \"$*\" >> '{log}'\n\
         if [ \"$1\" = '-V' ]; then printf 'tmux {version}\\n'; exit 0; fi\n\
         printf 'stub tmux was asked to run a command: %s\\n' \"$*\" >&2\n\
         exit 97\n",
        log = log.display(),
    );

    fs::write(&stub, script).expect("the stub is written");
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("the stub is executable");
    fs::write(&log, "").expect("the log starts empty");

    (stub, log)
}

/// Return one line per process the stub was asked to run.
fn spawned(log: &Path) -> Vec<String> {
    fs::read_to_string(log)
        .expect("the log is readable")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn typed_calls_route_over_the_connection_and_spawn_nothing() {
    let guard = TestServer::new().await.expect("a private tmux starts");
    let real = guard.server().clone();
    let session = real
        .new_session("routed")
        .await
        .expect("the fixture holds a session");
    let version = real
        .capabilities()
        .await
        .expect("the fixture reports its release")
        .tmux_version()
        .raw()
        .to_owned();

    // The connection belongs to the caller. Everything below borrows it.
    let control = ControlMode::attach(&real, session.id())
        .await
        .expect("a control client attaches");
    let (sender, events) = control.split();

    let directory = tempfile::tempdir().expect("a scratch directory");
    let (stub, log) = stub_tmux(directory.path(), &version);
    let probe = Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(&stub)
        .build()
        .expect("a server pointed at the fixture's socket");

    let routed = probe
        .over_control_mode(&sender)
        .await
        .expect("the connection reaches the same server");

    // The version probe is the one process this whole test lets the routed
    // handle's transport start, and it happens before the switch.
    assert_eq!(
        spawned(&log),
        vec!["-V".to_owned()],
        "only the release probe ran as a process",
    );

    // A listing: the typed call renders a format plan and decodes rows.
    let names: Vec<String> = routed
        .sessions()
        .await
        .expect("the routed handle lists sessions")
        .iter()
        .map(|found| found.name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["routed"], "the rows came from the real daemon");

    // A mutation, and a chain, which takes the other dispatch path.
    let window = session
        .new_window("second")
        .await
        .expect("the fixture can be changed through the connection's server");
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
        .expect("a chain routes as one line");
    // tmux answers each command of a chain with its own block, so the last
    // command's output is only here if every block was read.
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
            CommandChain::new(Command::new("list-panes").arg("-t").arg("%4294967294"))
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
        .cmd(Command::new("list-panes").arg("-t").arg("%4294967294"))
        .await
        .expect("a refused command still answers");
    assert!(!refused.success(), "tmux refused the target");
    assert_eq!(refused.exit_code(), Some(1));
    assert!(
        !refused.stderr().is_empty(),
        "the refusal text lands where a process would have put it",
    );

    // A `-f` predicate survives being rendered as a control-mode line. It is
    // the token most likely not to: `#{==:#{pane_id},%1}` opens with `#` and
    // carries braces and a comma, so tmux has to be given it quoted.
    assert!(
        routed
            .pane_by_id(&"%4294967294".parse().expect("a well-formed pane id"))
            .await
            .expect("a filtered listing succeeds with no rows")
            .is_none(),
        "the predicate reached tmux intact and matched nothing",
    );

    // And the typed layer reads an `%error` block the way it reads stderr.
    // This is the assertion that discriminates: `Pane::capture` classifies a
    // refusal from the text tmux gave, so putting that text on the wrong
    // stream would make the error generic instead of "the pane is gone".
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
        "an `%error` block classifies from its text, not from a status: {gone:?}",
    );

    // A handle the routed server found dispatches over the connection too,
    // rather than falling back to the process transport it was built from.
    let pane = routed
        .panes()
        .await
        .expect("the routed handle lists panes")
        .remove(0);
    pane.send_line("true")
        .await
        .expect("an inherited handle sends over the connection");

    // Nothing above added a line: every one of those calls was written to the
    // connection instead of forked.
    assert_eq!(
        spawned(&log).len(),
        1,
        "routed dispatches spawned no process: {:?}",
        spawned(&log),
    );

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

/// A connection runs one command at a time, and tmux closes a blocking
/// `wait-for` as soon as it queues it: routed, the wait would report a signal
/// nobody sent and the connection would answer nothing else until the channel
/// was signalled.
#[tokio::test]
async fn a_blocking_channel_call_is_refused_rather_than_routed() {
    let guard = TestServer::new().await.expect("a private tmux starts");
    let real = guard.server().clone();
    let session = real
        .new_session("routed")
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

    for refused in [
        routed
            .wait_for_channel("never-signalled", std::time::Duration::from_secs(5))
            .await
            .err(),
        routed.lock_channel("never-signalled").await.err(),
    ] {
        let refused = refused.expect("a blocking channel call is refused");
        assert!(
            matches!(
                refused,
                libtmux::Error::ControlMode {
                    kind: libtmux::ControlModeErrorKind::BlockingCommand,
                    ..
                }
            ),
            "refused for blocking the connection: {refused:?}",
        );
    }

    // The connection still answers, and the half that does not block routes.
    routed
        .signal_channel("never-signalled")
        .await
        .expect("signalling does not block, so it routes");
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
    let second = TestServer::new()
        .await
        .expect("a second private tmux starts");
    let session = first
        .server()
        .new_session("origin")
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
        matches!(error, libtmux::Error::ServerMismatch { .. }),
        "the mismatch is reported rather than silently followed: {error:?}",
    );

    events.shutdown().await.expect("the connection closes");
    first.shutdown().await.expect("the first fixture stops");
    second.shutdown().await.expect("the second fixture stops");
}
