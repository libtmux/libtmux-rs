//! Parked channel clients have a finite lane shared by cloned drivers.

#![cfg(feature = "test-support")]
#![allow(clippy::expect_used)]

use std::path::Path;
use std::time::Duration;

use libtmux_tokio::test::{TestServer, install_executable, retry_until};
use libtmux_tokio::{ChannelWait, Command, DispatchLimits, Error, Server};

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn live(pid: u32) -> bool {
    let pid = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .expect("fixture child PID");
    !matches!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH)
    )
}

fn pids(path: &Path) -> Vec<u32> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|pid| pid.parse().expect("recorded child PID"))
        .collect()
}

fn client(
    guard: &TestServer,
    directory: &Path,
    limits: DispatchLimits,
    timeout: Duration,
) -> Server {
    let executable = directory.join("tmux");
    let binary = guard
        .server()
        .resolved_tmux_executable()
        .expect("fixture executable resolves");
    install_executable(
        &executable,
        &format!(
            "#!/bin/sh\ncase \"$*\" in *'wait-for -- parked-'*|*'wait-for -L -- parked-'*) printf '%s\\n' \"$$\" >> {};; esac\nexec {} \"$@\"\n",
            quote(&directory.join("pids")),
            quote(&binary),
        ),
    )
    .expect("native witness wrapper installed");
    Server::builder()
        .socket_path(guard.socket_path())
        .config_file("/dev/null")
        .tmux_executable(executable)
        .dispatch_limits(limits.max_in_flight(1))
        .default_timeout(timeout)
        .build()
        .expect("bounded client configured")
}

#[tokio::test]
async fn distinct_timed_out_waits_cannot_accumulate_unlimited_native_clients() {
    let guard = TestServer::new().await.expect("private daemon starts");
    guard.session("keeper").await.expect("private session");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned witnesses");
    let server = client(
        &guard,
        directory.path(),
        DispatchLimits::default(),
        Duration::from_secs(5),
    );
    let limit = DispatchLimits::DEFAULT_PARKED;
    for index in 0..limit {
        assert_eq!(
            server
                .clone()
                .wait_for_channel(&format!("parked-{index}"), Duration::from_millis(100))
                .await
                .expect("a distinct channel is admitted"),
            ChannelWait::TimedOut
        );
    }
    retry_until(Duration::from_secs(2), async || {
        pids(&directory.path().join("pids")).len() >= limit
    })
    .await
    .expect("the admitted native waits started");
    let excess = server
        .wait_for_channel("parked-excess", Duration::from_millis(100))
        .await;
    let actual = pids(&directory.path().join("pids"))
        .into_iter()
        .filter(|pid| live(*pid))
        .count();
    server.shutdown().await.expect("all parked clients reaped");
    guard.shutdown().await.expect("private daemon stops");
    assert_eq!(
        actual, limit,
        "distinct timed-out waits retain live clients"
    );
    let error = excess.expect_err("an excess distinct channel must be refused before spawn");
    assert!(error.is_transient(), "no client was submitted");
}

fn full(error: &Error) {
    assert!(matches!(error, Error::ParkedLimitExceeded { limit: 2 }));
    assert!(
        error.is_transient(),
        "capacity was refused before submission"
    );
    assert!(!format!("{error:?} {error}").contains("parked-excess"));
}

async fn signals_and_joins(left: &Server, right: &Server, directory: &Path) {
    for (server, channel) in [(left, "parked-a"), (right, "parked-a"), (right, "parked-b")] {
        assert_eq!(
            server
                .wait_for_channel(channel, Duration::from_millis(50))
                .await
                .expect("wait uses its admitted channel"),
            ChannelWait::TimedOut
        );
    }
    let witness = directory.join("pids");
    retry_until(Duration::from_secs(2), async || pids(&witness).len() == 2)
        .await
        .expect("two clients serve three timed-out waits");
    let admitted = pids(&witness);
    assert!(admitted.iter().all(|pid| live(*pid)));
    full(
        &left
            .wait_for_channel("parked-excess", Duration::from_millis(50))
            .await
            .expect_err("a distinct channel cannot bypass the shared limit"),
    );
    full(
        &right
            .lock_channel("parked-excess-lock")
            .await
            .expect_err("lock admission uses the same pool"),
    );
    left.signal_channel("parked-a")
        .await
        .expect("signal has ordinary capacity");
    retry_until(Duration::from_secs(2), async || !live(admitted[0]))
        .await
        .expect("the signalled client was reaped");
    full(
        &right
            .wait_for_channel("parked-excess", Duration::from_millis(50))
            .await
            .expect_err("a kept signal still occupies its place"),
    );
    assert_eq!(
        right
            .wait_for_channel("parked-a", Duration::from_secs(2))
            .await
            .expect("kept signal"),
        ChannelWait::Signalled
    );
    assert_eq!(
        pids(&witness).len(),
        2,
        "the kept signal starts no new client"
    );
    assert_eq!(
        left.wait_for_channel("parked-c", Duration::from_millis(50))
            .await
            .expect("consuming a signal returns its place"),
        ChannelWait::TimedOut
    );
    retry_until(Duration::from_secs(2), async || pids(&witness).len() == 3)
        .await
        .expect("one new native wait starts");
    assert_eq!(
        pids(&witness).into_iter().filter(|pid| live(*pid)).count(),
        2
    );
    for channel in ["parked-b", "parked-c"] {
        right
            .signal_channel(channel)
            .await
            .expect("full parked pool cannot starve a signal");
        assert_eq!(
            left.wait_for_channel(channel, Duration::from_secs(2))
                .await
                .expect("later signal"),
            ChannelWait::Signalled
        );
    }
}

#[tokio::test]
async fn cloned_waits_keep_signals_and_leave_a_lane_for_release() {
    let guard = TestServer::new().await.expect("private daemon starts");
    guard.session("keeper").await.expect("private session");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned witnesses");
    let server = client(
        &guard,
        directory.path(),
        DispatchLimits::default().max_parked(2),
        Duration::from_secs(3),
    );
    signals_and_joins(&server, &server.clone(), directory.path()).await;
    server.shutdown().await.expect("parked clients reaped");
    guard.shutdown().await.expect("private daemon stops");
}

#[cfg(feature = "control-mode")]
#[tokio::test]
async fn control_derived_waits_share_the_native_parked_budget() {
    use libtmux_tokio::control::ControlMode;

    let guard = TestServer::new().await.expect("private daemon starts");
    if !guard
        .server()
        .daemon_version()
        .await
        .expect("daemon version")
        .has_behavior(&libtmux_tokio::since::CONTROL_ACQUISITION)
    {
        guard.shutdown().await.expect("ineligible daemon stops");
        return;
    }
    let session = guard.session("keeper").await.expect("private session");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned witnesses");
    let server = client(
        &guard,
        directory.path(),
        DispatchLimits::default().max_parked(2),
        Duration::from_secs(3),
    );
    let (sender, events) = ControlMode::attach(&server, session.id())
        .await
        .expect("qualified control")
        .split();
    let routed = server
        .over_control_mode(&sender)
        .await
        .expect("same daemon");
    signals_and_joins(&server, &routed, directory.path()).await;
    events.shutdown().await.expect("control client reaped");
    server.shutdown().await.expect("parked clients reaped");
    guard.shutdown().await.expect("private daemon stops");
}

#[tokio::test]
async fn a_cancelled_locker_retains_capacity_until_its_deferred_unlock() {
    let guard = TestServer::new().await.expect("private daemon starts");
    guard.session("keeper").await.expect("private session");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned witnesses");
    let server = client(
        &guard,
        directory.path(),
        DispatchLimits::default().max_parked(2),
        Duration::from_secs(3),
    );
    server
        .lock_channel("parked-lock")
        .await
        .expect("first holder");
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            server.lock_channel("parked-lock")
        )
        .await
        .is_err()
    );
    assert_eq!(
        server
            .wait_for_channel("parked-wait", Duration::from_millis(50))
            .await
            .expect("other parked client"),
        ChannelWait::TimedOut
    );
    full(
        &server
            .wait_for_channel("parked-excess", Duration::from_millis(50))
            .await
            .expect_err("two native clients fill the pool"),
    );
    assert_eq!(
        pids(&directory.path().join("pids"))
            .into_iter()
            .filter(|pid| live(*pid))
            .count(),
        2
    );
    server
        .unlock_channel("parked-lock")
        .await
        .expect("unlock keeps ordinary capacity");
    let queued = pids(&directory.path().join("pids"))[1];
    retry_until(Duration::from_secs(2), async || !live(queued))
        .await
        .expect("queued locker reaped");
    server
        .lock_channel("parked-lock")
        .await
        .expect("abandoned grant was unlocked");
    server
        .unlock_channel("parked-lock")
        .await
        .expect("new holder unlocks");
    server.shutdown().await.expect("parked clients reaped");
    guard.shutdown().await.expect("private daemon stops");
}

#[tokio::test]
async fn a_raw_wait_uses_parked_capacity_and_keeps_its_ordinary_deadline() {
    let guard = TestServer::new().await.expect("private daemon starts");
    guard.session("keeper").await.expect("private session");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned witnesses");
    let server = client(
        &guard,
        directory.path(),
        DispatchLimits::default().max_parked(1),
        Duration::from_millis(250),
    );
    let waiting = server.clone();
    let raw = tokio::spawn(async move {
        waiting
            .cmd(Command::new("wait-for").arg("--").arg("parked-raw"))
            .await
    });
    retry_until(Duration::from_secs(2), async || {
        pids(&directory.path().join("pids")).len() == 1
    })
    .await
    .expect("raw native wait started");
    let child = pids(&directory.path().join("pids"))[0];
    server
        .signal_channel("independent")
        .await
        .expect("raw wait leaves ordinary capacity");
    let error = raw
        .await
        .expect("raw caller joins")
        .expect_err("raw wait retains its deadline");
    assert!(matches!(
        error,
        Error::Timeout {
            before_dispatch: false,
            ..
        }
    ));
    assert!(!error.is_transient());
    retry_until(Duration::from_secs(2), async || !live(child))
        .await
        .expect("raw timeout reaped its child");
    assert_eq!(
        server
            .wait_for_channel("parked-after-raw", Duration::from_millis(50))
            .await
            .expect("cleanup returned parked capacity"),
        ChannelWait::TimedOut
    );
    server.shutdown().await.expect("parked clients reaped");
    guard.shutdown().await.expect("private daemon stops");
}

#[tokio::test]
async fn fresh_discovery_retires_a_kept_signal_from_the_replaced_daemon() {
    let mut guard = TestServer::new().await.expect("private daemon starts");
    guard.session("keeper").await.expect("private session");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned witnesses");
    let server = client(
        &guard,
        directory.path(),
        DispatchLimits::default().max_parked(1),
        Duration::from_secs(3),
    );
    let original = server.generation().await.expect("original generation");
    assert_eq!(
        server
            .wait_for_channel("parked-old", Duration::from_millis(50))
            .await
            .expect("original wait"),
        ChannelWait::TimedOut
    );
    server
        .signal_channel("parked-old")
        .await
        .expect("late signal");
    let child = pids(&directory.path().join("pids"))[0];
    retry_until(Duration::from_secs(2), async || !live(child))
        .await
        .expect("signal retained after reaping");
    assert!(matches!(
        server
            .wait_for_channel("parked-other", Duration::from_millis(50))
            .await,
        Err(Error::ParkedLimitExceeded { limit: 1 })
    ));
    server.kill().await.expect("old daemon stopped");
    retry_until(Duration::from_secs(2), async || {
        !guard.daemon_state().is_running()
    })
    .await
    .expect("old daemon reaped");
    let started = std::process::Command::new(guard.server().tmux_executable())
        .arg("-S")
        .arg(guard.socket_path())
        .arg("-f")
        .arg("/dev/null")
        .args(["new-session", "-d", "-s", "replacement", "exec sleep 30"])
        .env_remove("TMUX")
        .output()
        .expect("replacement native process");
    assert!(started.status.success());
    let fresh = server
        .sessions()
        .await
        .expect("fresh discovery of replacement");
    assert_eq!(fresh.len(), 1);
    assert_ne!(fresh[0].key().generation(), original);
    let outcome = server
        .wait_for_channel("parked-old", Duration::from_millis(50))
        .await;
    server.shutdown().await.expect("client cleanup");
    guard
        .server()
        .kill()
        .await
        .expect("replacement daemon stops");
    guard.shutdown().await.expect("fixture closes");
    assert_eq!(
        outcome.expect("old kept signal must release its retired slot"),
        ChannelWait::TimedOut,
        "an old signal cannot be delivered to the replacement"
    );
}
