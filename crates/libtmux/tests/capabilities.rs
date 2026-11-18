//! Public feature decisions and fresh daemon discovery.

#![cfg(feature = "test-support")]
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

use libtmux::test::TestServer;
use libtmux::{CapabilityDefinition, CapabilitySupport, Server, TmuxVersion, WorkaroundDefinition};

fn version(raw: &str) -> TmuxVersion {
    TmuxVersion::parse_output(format!("tmux {raw}\n").as_bytes()).expect("a version fixture")
}

#[test]
fn feature_decisions_follow_patch_and_prerelease_boundaries() {
    for (id, raw, expected) in [
        (
            "buffer-delete-named",
            "3.3a",
            CapabilitySupport::Unavailable,
        ),
        ("buffer-delete-named", "3.4", CapabilitySupport::Available),
        ("control-pane-off", "3.6b", CapabilitySupport::Fallback),
        ("control-pane-off", "3.7", CapabilitySupport::Available),
        ("layout-json", "3.8-rc", CapabilitySupport::Unavailable),
        ("layout-json", "3.8", CapabilitySupport::Available),
        ("layout-json", "next-3.9", CapabilitySupport::Available),
        ("layout-json", "master", CapabilitySupport::Available),
    ] {
        let definition = CapabilityDefinition::find(id).expect("a known feature");
        assert_eq!(definition.support(&version(raw)), expected, "{id} on {raw}");
    }
    assert!(CapabilityDefinition::find("unknown-program-owner").is_none());
    assert!(TmuxVersion::parse_output(b"tmux unknown\n").is_err());
}

#[test]
fn workaround_intervals_distinguish_exact_broken_releases() {
    for (id, raw, expected) in [
        ("break-pane-misnames-windows", "3.6b", false),
        ("break-pane-misnames-windows", "3.7", true),
        ("break-pane-misnames-windows", "3.7a", false),
        ("name-separators-rejected", "3.7", true),
        ("name-separators-rejected", "3.7a", false),
        ("split-window-percent-size", "3.4", true),
        ("split-window-percent-size", "3.5", false),
    ] {
        let definition = WorkaroundDefinition::find(id).expect("a known workaround");
        assert_eq!(
            definition.is_active(&version(raw)),
            expected,
            "{id} on {raw}"
        );
    }
    assert!(WorkaroundDefinition::find("unknown-workaround").is_none());
}

fn scratch() -> tempfile::TempDir {
    std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
    tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("an owned fixture directory")
}

fn minimum_binary() -> Option<PathBuf> {
    let variable = "LIBTMUX_TEST_MINIMUM_TMUX";
    let found = std::env::var_os(variable).filter(|value| !value.is_empty());
    assert!(
        found.is_some()
            || !std::env::var("LIBTMUX_REQUIRE_NATIVE")
                .is_ok_and(|required| required.split(',').any(|name| name == variable)),
        "{variable} is required but unset"
    );
    found.map(PathBuf::from)
}

#[tokio::test]
async fn discovery_of_an_absent_daemon_does_not_start_one() {
    let directory = scratch();
    let socket = directory.path().join("s");
    let server = Server::builder()
        .socket_path(&socket)
        .build()
        .expect("a client");
    assert!(
        server
            .daemon_capabilities()
            .await
            .expect("a cold endpoint")
            .is_none()
    );
    assert!(!socket.exists(), "discovery cannot start a daemon");
    server.shutdown().await.expect("the client closes");
}

async fn native_oracle(socket: &Path, args: &[&str]) -> std::process::Output {
    let binary = std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into());
    let mut command = tokio::process::Command::new(binary);
    command
        .args(["-N", "-S"])
        .arg(socket)
        .args(args)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .kill_on_drop(true);
    tokio::time::timeout(std::time::Duration::from_millis(500), command.output())
        .await
        .expect("the independent native oracle finishes")
        .expect("the independent native oracle runs")
}

async fn discovery_alias_witnesses(socket: &Path) -> [std::process::Output; 2] {
    [
        native_oracle(socket, &["show-buffer", "-b", "discovery-run-alias"]).await,
        native_oracle(socket, &["show-buffer", "-b", "discovery-if-alias"]).await,
    ]
}

fn assert_no_discovery_alias_effect(outputs: &[std::process::Output; 2]) {
    for output in outputs {
        assert!(
            !output.status.success() && output.stdout.is_empty(),
            "discovery executed an aliased command: {output:?}"
        );
    }
}

#[tokio::test]
async fn discovery_avoids_run_shell_and_if_shell_alias_effects() {
    let guard = TestServer::new().await.expect("a private daemon");
    #[cfg(feature = "control-mode")]
    let control = {
        use libtmux::control::ControlMode;

        let version = guard.server().daemon_version().await.expect("the release");
        if version.has_behavior(&libtmux::since::CONTROL_ACQUISITION) {
            let session = guard
                .server()
                .new_session("discovery")
                .await
                .expect("a session");
            let (sender, events) = ControlMode::attach(guard.server(), session.id())
                .await
                .expect("a connection")
                .split();
            let routed = guard
                .server()
                .over_control_mode(&sender)
                .await
                .expect("a route");
            Some((sender, events, routed))
        } else {
            None
        }
    };
    for (option, alias) in [
        (
            "command-alias[998]",
            "run-shell=set-buffer -b discovery-run-alias payload ; run",
        ),
        (
            "command-alias[999]",
            "if-shell=set-buffer -b discovery-if-alias payload ; if",
        ),
    ] {
        assert!(
            native_oracle(guard.socket_path(), &["set-option", "-s", option, alias])
                .await
                .status
                .success()
        );
    }
    assert_no_discovery_alias_effect(&discovery_alias_witnesses(guard.socket_path()).await);
    let unbound = guard.server().daemon_capabilities().await;
    let unbound_effects = discovery_alias_witnesses(guard.socket_path()).await;
    #[cfg(feature = "control-mode")]
    let routed = if let Some((_, _, routed)) = &control {
        Some(routed.daemon_capabilities().await)
    } else {
        None
    };
    let final_effects = discovery_alias_witnesses(guard.socket_path()).await;
    for option in ["command-alias[998]", "command-alias[999]"] {
        assert!(
            native_oracle(guard.socket_path(), &["set-option", "-s", "-u", option])
                .await
                .status
                .success()
        );
    }
    #[cfg(feature = "control-mode")]
    if let Some((_, events, _)) = control {
        events.shutdown().await.expect("the connection closes");
    }
    guard.shutdown().await.expect("the fixture closes");
    assert_no_discovery_alias_effect(&unbound_effects);
    assert_no_discovery_alias_effect(&final_effects);
    let report = unbound.expect("the native query").expect("a daemon");
    #[cfg(feature = "control-mode")]
    if let Some(routed) = routed {
        assert_eq!(
            routed.expect("the connected query").expect("a daemon"),
            report
        );
    }
    assert!(!report.tmux_version().raw().is_empty());
}

async fn check_older_client_against_current_daemon(current: &TestServer, minimum: &Path) {
    let identity = current
        .server()
        .format(None, "#{pid},#{start_time}")
        .await
        .expect("the daemon identity");
    let older_client = Server::builder()
        .socket_path(current.socket_path())
        .tmux_executable(minimum)
        .build()
        .expect("the older configured client");
    match older_client.daemon_capabilities().await {
        Ok(report) => assert_eq!(
            report
                .expect("a live daemon is not an absent endpoint")
                .tmux_version(),
            &current
                .server()
                .daemon_version()
                .await
                .expect("the actual release")
        ),
        Err(error) => {
            eprintln!("older-client discovery refused without a fallback: {error:?}");
            assert!(
                matches!(error, libtmux::Error::VersionProbeFailed { .. }),
                "{error:?}"
            );
        }
    }
    assert_eq!(
        current
            .server()
            .format(None, "#{pid},#{start_time}")
            .await
            .expect("the same daemon remains"),
        identity
    );
    older_client
        .shutdown()
        .await
        .expect("the older client closes");
}

#[tokio::test]
async fn real_tmux_compat_discovery_uses_the_daemon_and_preserves_stale_admission() {
    let Some(minimum) = minimum_binary() else {
        return;
    };
    let old = TestServer::builder()
        .tmux_executable(&minimum)
        .start()
        .await
        .expect("the floor daemon");
    let current = TestServer::new().await.expect("the current daemon");
    if current
        .server()
        .daemon_version()
        .await
        .expect("the current release")
        .raw()
        == "3.3a"
    {
        old.shutdown().await.expect("the floor fixture closes");
        current
            .shutdown()
            .await
            .expect("the same-floor fixture closes");
        return;
    }
    check_older_client_against_current_daemon(&current, &minimum).await;
    let directory = scratch();
    let socket = directory.path().join("s");
    std::fs::hard_link(old.socket_path(), &socket).expect("an alias to the old daemon");
    let client = Server::builder()
        .socket_path(&socket)
        .build()
        .expect("a current client");
    let seeded = client
        .daemon_version()
        .await
        .expect("the old daemon seeds admission");
    assert_eq!(seeded.raw(), "3.3a");
    let report = client
        .daemon_capabilities()
        .await
        .expect("fresh discovery")
        .expect("a daemon");
    assert_eq!(report.tmux_version().raw(), "3.3a");
    assert_eq!(
        report
            .feature("buffer-delete-named")
            .expect("a known feature")
            .support,
        CapabilitySupport::Unavailable
    );

    std::fs::remove_file(&socket).expect("remove only the owned alias");
    std::fs::hard_link(current.socket_path(), &socket).expect("alias the replacement");
    let report = client
        .daemon_capabilities()
        .await
        .expect("the replacement report")
        .expect("a daemon");
    assert_ne!(
        report.tmux_version().raw(),
        "3.3a",
        "the qualifying client is newer than the floor"
    );
    let result = client
        .cmd(
            libtmux::Command::new("set-buffer")
                .arg("-b")
                .arg("must-not-exist")
                .arg("payload"),
        )
        .await;
    let effect = current
        .server()
        .buffer("must-not-exist")
        .await
        .expect("replacement buffer");
    assert!(
        effect.is_none(),
        "discovery granted stale mutation authority: {effect:?}"
    );
    let error = result.expect_err("discovery must preserve the old mutation fence");
    assert!(
        matches!(error, libtmux::Error::ServerReplaced { .. }),
        "{error:?}"
    );
    client.shutdown().await.expect("the client closes");
    old.shutdown().await.expect("the floor fixture closes");
    current
        .shutdown()
        .await
        .expect("the current fixture closes");
}

#[tokio::test]
async fn discovery_timeout_and_cancellation_reap_the_native_client() {
    use rustix::process::{Pid, test_kill_process};
    use std::time::Duration;

    for cancel in [true, false] {
        let directory = scratch();
        let ready = directory.path().join("ready");
        let script = directory.path().join("held.py");
        std::fs::write(&script, format!(
                "import os,signal\nready={}\nwith open(ready+'.tmp','w') as output: output.write(str(os.getpid()))\nos.replace(ready+'.tmp',ready)\nsignal.alarm(1)\nsignal.pause()\n",
            serde_json::to_string(&ready).expect("fixture path")
        )).expect("the native stall fixture");
        let wrapper = directory.path().join("tmux");
        libtmux::test::install_executable(
            &wrapper,
            &format!("#!/bin/sh\nexec /usr/bin/python3 '{}'\n", script.display()),
        )
        .expect("the fixture executable");
        let server = Server::builder()
            .socket_path(directory.path().join("s"))
            .tmux_executable(&wrapper)
            .default_timeout(Duration::from_millis(500))
            .build()
            .expect("the bounded client");
        let request = server.clone();
        let pending = tokio::spawn(async move { request.daemon_capabilities().await });
        libtmux::test::retry_until(Duration::from_millis(500), async || ready.exists())
            .await
            .expect("the native client publishes readiness while Tokio remains responsive");
        assert!(
            !pending.is_finished(),
            "discovery remains held at the native read"
        );
        let pid = std::fs::read_to_string(&ready)
            .expect("native PID")
            .parse::<i32>()
            .expect("a PID");
        if cancel {
            pending.abort();
            assert!(
                pending
                    .await
                    .expect_err("the observer was cancelled")
                    .is_cancelled()
            );
        } else {
            let error = pending
                .await
                .expect("the task finishes")
                .expect_err("the native read times out");
            assert!(matches!(error, libtmux::Error::Timeout { .. }), "{error:?}");
        }
        libtmux::test::retry_until(Duration::from_millis(250), async || {
            test_kill_process(Pid::from_raw(pid).expect("positive PID"))
                == Err(rustix::io::Errno::SRCH)
        })
        .await
        .expect("the native client exits before explicit shutdown");
        server.shutdown().await.expect("native clients drain");
    }
}

#[cfg(feature = "control-mode")]
async fn alias_floor_and_refresh_admission(server: &Server, socket: &Path) -> Option<TestServer> {
    if let Some(binary) = minimum_binary() {
        let floor = TestServer::builder()
            .tmux_executable(binary)
            .start()
            .await
            .expect("the replacement daemon");
        std::fs::hard_link(floor.socket_path(), socket).expect("the owned replacement alias");
        server
            .validate_layouts(std::iter::empty())
            .await
            .expect("the parent refreshes its cache without effects");
        assert_eq!(
            server
                .daemon_version()
                .await
                .expect("the parent cache")
                .raw(),
            "3.3a"
        );
        Some(floor)
    } else {
        None
    }
}

#[cfg(feature = "control-mode")]
#[tokio::test]
async fn control_discovery_stays_on_the_accepted_connection_and_checks_liveness() {
    use libtmux::control::ControlMode;

    let guard = TestServer::new().await.expect("a private daemon");
    if !guard
        .server()
        .daemon_version()
        .await
        .expect("the native version")
        .has_behavior(&libtmux::since::CONTROL_ACQUISITION)
    {
        guard.shutdown().await.expect("ineligible fixture closes");
        return;
    }
    let directory = scratch();
    let socket = directory.path().join("s");
    std::fs::hard_link(guard.socket_path(), &socket).expect("an owned socket alias");
    let server = Server::builder()
        .socket_path(&socket)
        .build()
        .expect("the aliased client");
    let session = server.new_session("discovery").await.expect("a session");
    let control = ControlMode::attach(&server, session.id())
        .await
        .expect("the accepted connection");
    let (sender, events) = control.split();
    let routed = server
        .over_control_mode(&sender)
        .await
        .expect("the native route");
    for hook in [
        "after-display-message",
        "after-list-clients",
        "command-error",
    ] {
        guard
            .server()
            .cmd(
                libtmux::Command::new("set-hook")
                    .arg("-g")
                    .arg(hook)
                    .arg("set-buffer -b discovery-fired-hook hook"),
            )
            .await
            .expect("the hook witness installs");
    }
    let accepted = routed
        .daemon_capabilities()
        .await
        .expect("the live connection")
        .expect("a daemon");
    std::fs::remove_file(&socket).expect("remove only the owned alias");
    assert!(
        server
            .daemon_capabilities()
            .await
            .expect("the absent configured endpoint")
            .is_none()
    );
    let floor = alias_floor_and_refresh_admission(&server, &socket).await;
    assert_eq!(
        routed
            .daemon_capabilities()
            .await
            .expect("the original connection")
            .expect("a daemon"),
        accepted,
        "control report must use acquisition metadata after the parent cache changes"
    );
    if floor.is_some() {
        assert_eq!(
            server
                .daemon_version()
                .await
                .expect("the unchanged parent cache")
                .raw(),
            "3.3a"
        );
    }
    assert!(
        guard
            .server()
            .buffer("discovery-fired-hook")
            .await
            .expect("the hook witness")
            .is_none(),
        "discovery must not run a user hook"
    );
    events.shutdown().await.expect("the connection closes");
    assert!(
        routed.daemon_capabilities().await.is_err(),
        "a closed route cannot return cached availability"
    );
    server.shutdown().await.expect("the unbound client closes");
    if let Some(floor) = floor {
        floor
            .shutdown()
            .await
            .expect("the replacement fixture closes");
    }
    guard.shutdown().await.expect("the fixture closes");
}
