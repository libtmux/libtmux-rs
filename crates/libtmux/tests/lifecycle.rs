//! Lifecycle operations run only on fixture-owned sockets.
#![cfg(feature = "test-support")]

use std::sync::Arc;
use std::time::Duration;

use libtmux::lifecycle::{
    Discovery, DiscoverySkip, DiscoveryTruncation, FindOrCreate, PaneIdentity,
};
use libtmux::test::{TestServer, install_executable, retry_until};
use libtmux::{Error, NewSessionOptions, Server, SplitDirection, SplitOptions};

fn split() -> SplitOptions {
    SplitOptions::new(SplitDirection::Below).command("sleep 300")
}

#[tokio::test]
async fn adoption_kills_ids_after_rename_move_and_link_changes() {
    let guard = TestServer::new().await.unwrap();
    let server = guard.server();
    let mut session = guard.session("adopt-session").await.unwrap();
    let session_owner = session.adopt().await.unwrap();
    session.rename("renamed-session").await.unwrap();
    let survivor = guard.session("survivor").await.unwrap();
    let mut window = session.new_window("adopt-window").await.unwrap();
    let window_owner = window.adopt().await.unwrap();
    window.rename("renamed-window").await.unwrap();
    window.move_to(&survivor, 9).await.unwrap();
    window.link_to(&session, Some(9)).await.unwrap();
    let pane = window.split(split()).await.unwrap();
    let pane_owner = pane.adopt().await.unwrap();
    let destination = survivor
        .active_window()
        .await
        .unwrap()
        .unwrap()
        .active_pane()
        .await
        .unwrap()
        .unwrap();
    pane.join_into(
        &destination,
        libtmux::JoinOptions::new(SplitDirection::Below),
    )
    .await
    .unwrap();
    pane_owner.close().await.unwrap();
    assert!(
        server
            .pane_by_id(pane_owner.resource().id())
            .await
            .unwrap()
            .is_none()
    );
    window_owner.close().await.unwrap();
    assert!(
        server
            .window_by_id(window_owner.resource().id())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        session
            .windows()
            .await
            .unwrap()
            .iter()
            .all(|w| w.id() != window_owner.resource().id())
    );
    session_owner.close().await.unwrap();
    session_owner.close().await.unwrap();
    assert!(session_owner.is_closed());
    assert!(server.session_by_id(session.id()).await.unwrap().is_none());
    assert!(server.session_by_id(survivor.id()).await.unwrap().is_some());
    assert!(server.drain_cleanup().await.is_empty());
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn client_shutdown_and_reused_results_leave_remote_objects_alive() {
    let guard = TestServer::new().await.unwrap();
    let original = guard.session("borrowed").await.unwrap();
    let client = Server::builder()
        .socket_path(guard.socket_path())
        .build()
        .unwrap();
    let reused = client.find_or_create_session("borrowed").await.unwrap();
    assert!(matches!(reused, FindOrCreate::Reused(_)));
    drop(reused);
    client.shutdown().await.unwrap();
    assert!(
        guard
            .server()
            .session_by_id(original.id())
            .await
            .unwrap()
            .is_some()
    );
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn find_create_matches_reuses_rejects_ambiguity_and_serializes_competitors() {
    let guard = TestServer::new().await.unwrap();
    let server = guard.server();
    let (a, b) = tokio::join!(
        server.find_or_create_session("exact"),
        server.find_or_create_session("exact")
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.is_created(), b.is_created());
    assert_eq!(a.resource().id(), b.resource().id());
    let session = a.resource();
    let (w1, w2) = tokio::join!(
        session.find_or_create_window("worker"),
        session.find_or_create_window("worker")
    );
    let (w1, w2) = (w1.unwrap(), w2.unwrap());
    assert_ne!(w1.is_created(), w2.is_created());
    assert_eq!(w1.resource().id(), w2.resource().id());
    let window = w1.resource();
    let identity = PaneIdentity::new("@application", "worker").unwrap();
    let (p1, p2) = tokio::join!(
        window.find_or_create_pane(identity.clone(), split()),
        window.find_or_create_pane(identity.clone(), split())
    );
    let (p1, p2) = (p1.unwrap(), p2.unwrap());
    assert_ne!(p1.is_created(), p2.is_created());
    assert_eq!(p1.resource().id(), p2.resource().id());
    let duplicate = window.split(split()).await.unwrap();
    duplicate
        .set_option("@application", "worker")
        .await
        .unwrap();
    assert!(matches!(
        window.find_or_create_pane(identity, split()).await,
        Err(Error::LifecycleAmbiguous {
            kind: "pane",
            matches: 2
        })
    ));
    session.new_window("worker").await.unwrap();
    assert!(matches!(
        session.find_or_create_window("worker").await,
        Err(Error::LifecycleAmbiguous {
            kind: "window",
            matches: 2
        })
    ));
    assert!(matches!(
        server.find_or_create_session("").await,
        Err(Error::LifecycleInput { .. })
    ));
    assert!(matches!(
        session.find_or_create_window("").await,
        Err(Error::LifecycleInput { .. })
    ));
    assert!(PaneIdentity::new("not-user-option", "a").is_err());
    // tmux's exact session name uniqueness supplies the session ambiguity boundary.
    assert!(server.new_session("exact").await.is_err());
    for result in [a, b] {
        if let FindOrCreate::Created(owner) = result {
            owner.close().await.unwrap();
        }
    }
    assert!(server.drain_cleanup().await.is_empty());
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn stale_owners_cannot_kill_replacement_daemon_or_reused_ids() {
    let guard = TestServer::new().await.unwrap();
    let server = guard.server();
    let session = guard.session("old").await.unwrap();
    let window = session.active_window().await.unwrap().unwrap();
    let pane = window.active_pane().await.unwrap().unwrap();
    let session_owner = session.adopt().await.unwrap();
    let window_owner = window.adopt().await.unwrap();
    let pane_owner = pane.adopt().await.unwrap();
    let old = server.adopt().await.unwrap();
    server.kill().await.unwrap();
    let mut replacement = tokio::process::Command::new(
        std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into()),
    )
    .args(["-D", "-S"])
    .arg(guard.socket_path())
    .args(["-f", "/dev/null"])
    .env_remove("TMUX")
    .env_remove("TMUX_PANE")
    .kill_on_drop(true)
    .spawn()
    .unwrap();
    retry_until(Duration::from_secs(5), || async {
        server
            .generation()
            .await
            .is_ok_and(|g| g != old.generation())
    })
    .await
    .unwrap();
    let fresh = server
        .new_session(NewSessionOptions::new("replacement").command("sleep 300"))
        .await
        .unwrap();
    for error in [
        old.close().await.unwrap_err(),
        session_owner.close().await.unwrap_err(),
        window_owner.close().await.unwrap_err(),
        pane_owner.close().await.unwrap_err(),
    ] {
        assert!(
            matches!(error, Error::ServerGenerationChanged { .. }),
            "{error:?}"
        );
    }
    assert!(!old.is_closed());
    assert!(server.session_by_id(fresh.id()).await.unwrap().is_some());
    server.adopt().await.unwrap().close().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), replacement.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(server.drain_cleanup().await.is_empty());
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn ownership_token_refuses_equal_pid_and_start_and_preserves_malformed_metadata() {
    let guard = TestServer::new().await.unwrap();
    let server = guard.server();
    let session = guard.session("same-generation").await.unwrap();
    server
        .cmd(
            libtmux::Command::new("set-option")
                .arg("-su")
                .arg("@libtmux_owner_generation"),
        )
        .await
        .unwrap();
    let owner = session.adopt().await.unwrap();
    let daemon = server.adopt().await.unwrap();
    let accepted = owner.generation();
    let replacement_token = "00112233445566778899AABBCCDDEEFF";
    server
        .cmd(
            libtmux::Command::new("set-option")
                .arg("-s")
                .arg("@libtmux_owner_generation")
                .arg(replacement_token),
        )
        .await
        .unwrap();
    assert_eq!(server.generation().await.unwrap(), accepted);
    for error in [
        owner.close().await.unwrap_err(),
        daemon.close().await.unwrap_err(),
    ] {
        assert!(matches!(error, Error::OwnershipTokenChanged), "{error:?}");
    }
    assert!(server.session_by_id(session.id()).await.unwrap().is_some());
    let accepted_again = session.adopt().await.unwrap();
    let token = server
        .cmd(
            libtmux::Command::new("show-options")
                .arg("-sv")
                .arg("@libtmux_owner_generation"),
        )
        .await
        .unwrap();
    assert_eq!(token.stdout(), format!("{replacement_token}\n").as_bytes());
    for malformed in [
        "",
        "not-a-token",
        "a".repeat(33).as_str(),
        "00112233445566778899aabbccddeeff00 ",
        "00112233445566778899aabbccddeeff00\n",
    ] {
        server
            .cmd(
                libtmux::Command::new("set-option")
                    .arg("-s")
                    .arg("@libtmux_owner_generation")
                    .arg(malformed),
            )
            .await
            .unwrap();
        assert!(matches!(
            server.adopt().await,
            Err(Error::OwnershipMetadata { .. })
        ));
        assert!(matches!(
            session.adopt().await,
            Err(Error::OwnershipMetadata { .. })
        ));
        let token = server
            .cmd(
                libtmux::Command::new("show-options")
                    .arg("-sv")
                    .arg("@libtmux_owner_generation"),
            )
            .await
            .unwrap();
        assert_eq!(token.stdout(), format!("{malformed}\n").as_bytes());
    }
    server
        .cmd(
            libtmux::Command::new("set-option")
                .arg("-s")
                .arg("@libtmux_owner_generation")
                .arg(replacement_token),
        )
        .await
        .unwrap();
    accepted_again.close().await.unwrap();
    assert!(server.drain_cleanup().await.is_empty());
    guard.shutdown().await.unwrap();
}

#[allow(clippy::unwrap_used, reason = "test fixture setup assertions")]
fn wrapper(guard: &TestServer) -> (Server, std::path::PathBuf) {
    let directory = guard.socket_path().parent().unwrap();
    let script = directory.join("fault.py");
    let real =
        std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "/usr/local/bin/tmux".into());
    let content = format!(
        r#"#!/bin/sh
exec /usr/bin/python3 - "$@" <<'PY'
import os, pathlib, subprocess, sys, time
root = pathlib.Path({directory:?})
args = sys.argv[1:]
if 'if-shell' in args and (root / 'deny-cleanup').exists():
    sys.stderr.write('injected cleanup refusal\n'); sys.exit(42)
if 'new-session' in args and (root / 'race-start').exists():
    (root / 'race-start').unlink()
    environment = os.environ.copy()
    environment.pop('LIBTMUX_LIFECYCLE_NONCE', None)
    socket = args[args.index('-S') + 1]
    subprocess.run([{real:?}, '-S', socket, '-f', '/dev/null', 'new-session', '-d', '-s', 'racer', 'sleep 300'], env=environment, check=True)
if any(x in args for x in ['new-session', 'new-window', 'split-window']) and (root / 'deny-create').exists():
    sys.stderr.write('injected create refusal\n'); sys.exit(43)
result = subprocess.run([{real:?}, *args], capture_output=True)
out = result.stdout
if any(x in args for x in ['new-session', 'new-window', 'split-window']) and result.returncode == 0:
    (root / 'created').write_bytes(out)
    if (root / 'hold-create').exists():
        while (root / 'hold-create').exists(): time.sleep(.01)
    if (root / 'corrupt-body').exists(): out = out.split(b'\t', 1)[0] + b'\tmalformed\n' + b'__libtmux_ownership__ ' + out.rsplit(b'__libtmux_ownership__ ', 1)[1]
    if (root / 'corrupt-id').exists(): out = b'malformed\n'
    if (root / 'empty-reply').exists(): out = b''
sys.stdout.buffer.write(out); sys.stderr.buffer.write(result.stderr); sys.exit(result.returncode)
PY
"#,
        directory = directory.to_string_lossy(),
        real = real.to_string_lossy()
    );
    install_executable(&script, &content).unwrap();
    (
        Server::builder()
            .socket_path(guard.socket_path())
            .tmux_executable(&script)
            .build()
            .unwrap(),
        directory.to_owned(),
    )
}

#[allow(clippy::unwrap_used, reason = "test fixture teardown assertions")]
fn remove_wrapper_files(root: &std::path::Path) {
    for name in [
        "fault.py",
        "created",
        "deny-cleanup",
        "corrupt-body",
        "corrupt-id",
        "empty-reply",
        "hold-create",
        "race-start",
        "deny-create",
    ] {
        let path = root.join(name);
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
    }
}

#[tokio::test]
async fn known_receipts_roll_back_decode_failures_and_unknown_receipts_stay_explicit() {
    let guard = TestServer::new().await.unwrap();
    let (client, root) = wrapper(&guard);
    std::fs::write(root.join("corrupt-body"), "").unwrap();
    let error = client.owned_session("rollback").await.unwrap_err();
    assert!(matches!(error, Error::AfterEffect { .. }));
    assert!(guard.server().session("rollback").await.unwrap().is_none());
    std::fs::write(root.join("deny-cleanup"), "").unwrap();
    let error = client.owned_session("both").await.unwrap_err();
    assert!(matches!(error, Error::AcquisitionRollback { .. }));
    assert!(guard.server().session("both").await.unwrap().is_some());
    std::fs::remove_file(root.join("deny-cleanup")).unwrap();
    std::fs::remove_file(root.join("corrupt-body")).unwrap();
    std::fs::write(root.join("corrupt-id"), "").unwrap();
    let error = client.owned_session("unknown").await.unwrap_err();
    assert!(
        matches!(error, Error::AfterEffect { source, .. } if matches!(*source, Error::UnknownCreation { .. }))
    );
    assert!(guard.server().session("unknown").await.unwrap().is_some());
    std::fs::remove_file(root.join("corrupt-id")).unwrap();
    std::fs::write(root.join("empty-reply"), "").unwrap();
    let error = client.owned_session("empty-reply").await.unwrap_err();
    assert!(matches!(error, Error::UnknownCreation { .. }));
    assert!(
        guard
            .server()
            .session("empty-reply")
            .await
            .unwrap()
            .is_some()
    );

    assert!(client.drain_cleanup().await.is_empty());
    client.shutdown().await.unwrap();
    remove_wrapper_files(&root);
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn truncated_create_reply_reports_unknown_result_and_preserves_original_error() {
    let guard = TestServer::new().await.unwrap();
    let client = Server::builder()
        .socket_path(guard.socket_path())
        .output_limits(libtmux::OutputLimits::default().max_stdout_bytes(32))
        .build()
        .unwrap();
    let error = client.owned_session("truncated-reply").await.unwrap_err();
    assert!(
        matches!(error, Error::UnknownCreation { source, .. } if matches!(*source, Error::OutputLimitExceeded { .. }))
    );
    let created = guard
        .server()
        .session("truncated-reply")
        .await
        .unwrap()
        .unwrap();
    created.adopt().await.unwrap().close().await.unwrap();
    assert!(client.drain_cleanup().await.is_empty());
    client.shutdown().await.unwrap();
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_close_retries_and_cancelled_body_cleanup_is_observable() {
    let guard = TestServer::new().await.unwrap();
    let (client, root) = wrapper(&guard);
    let owner = client.owned_session("retry").await.unwrap();
    let accepted = owner.generation();
    let displaced = root.join("displaced.sock");
    std::fs::rename(guard.socket_path(), &displaced).unwrap();
    let missing = owner.close().await;
    let still_alive = !process_exited(accepted.pid());
    std::fs::rename(&displaced, guard.socket_path()).unwrap();
    assert!(
        matches!(missing, Err(Error::ServerGone { .. })),
        "{missing:?}"
    );
    assert!(still_alive, "a missing endpoint does not prove daemon exit");
    assert!(!owner.is_closed());

    std::fs::write(root.join("deny-cleanup"), "").unwrap();
    assert!(owner.close().await.is_err());
    assert!(!owner.is_closed());
    let entered = Arc::new(tokio::sync::Notify::new());
    let scope_owner = owner.clone();
    let entered_body = Arc::clone(&entered);
    let task = tokio::spawn(async move {
        scope_owner
            .scope(async |_session| {
                entered_body.notify_one();
                std::future::pending::<Result<(), Error>>().await
            })
            .await
    });
    entered.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let failures = client.drain_cleanup().await;
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(!owner.is_closed());
    std::fs::remove_file(root.join("deny-cleanup")).unwrap();
    owner.close().await.unwrap();
    owner.close().await.unwrap();
    assert!(owner.is_closed());
    assert!(client.drain_cleanup().await.is_empty());
    client.shutdown().await.unwrap();
    remove_wrapper_files(&root);
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn find_create_failures_preserve_existing_objects_and_known_child_receipts_roll_back() {
    let guard = TestServer::new().await.unwrap();
    let (client, root) = wrapper(&guard);
    let session = client.new_session("failure-parent").await.unwrap();
    let window = session.active_window().await.unwrap().unwrap();
    let panes_before = window.panes().await.unwrap().len();
    let windows_before = session.windows().await.unwrap().len();
    std::fs::write(root.join("deny-create"), "").unwrap();
    assert!(client.find_or_create_session("fails").await.is_err());
    assert!(session.find_or_create_window("fails").await.is_err());
    assert!(
        window
            .find_or_create_pane(PaneIdentity::new("@app", "fails").unwrap(), split())
            .await
            .is_err()
    );
    std::fs::remove_file(root.join("deny-create")).unwrap();
    std::fs::write(root.join("corrupt-body"), "").unwrap();
    assert!(session.owned_window("decode-fails").await.is_err());
    assert!(window.owned_pane(split()).await.is_err());
    assert_eq!(window.panes().await.unwrap().len(), panes_before);
    assert_eq!(session.windows().await.unwrap().len(), windows_before);
    assert!(client.drain_cleanup().await.is_empty());
    client.shutdown().await.unwrap();
    remove_wrapper_files(&root);
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn startup_nonce_reuses_an_external_competitor_without_server_ownership() {
    let guard = TestServer::new().await.unwrap();
    let (_unused, root) = wrapper(&guard);
    let path = root.join("competitor");
    let client = Server::builder()
        .socket_path(&path)
        .config_file("/dev/null")
        .tmux_executable(root.join("fault.py"))
        .build()
        .unwrap();
    std::fs::write(root.join("race-start"), "").unwrap();
    let outcome = client.find_or_create_server().await.unwrap();
    assert!(matches!(outcome, FindOrCreate::Reused(_)));
    let sessions = client.sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].name().as_bytes(), b"racer");
    // Startup must not change the competing daemon's exit-empty default.
    let exit = client
        .cmd(
            libtmux::Command::new("show-options")
                .arg("-sv")
                .arg("exit-empty"),
        )
        .await
        .unwrap();
    assert_eq!(exit.stdout(), b"on\n");
    let generation = client.generation().await.unwrap();
    client.adopt().await.unwrap().close().await.unwrap();
    retry_until(Duration::from_secs(5), || async {
        process_exited(generation.pid())
    })
    .await
    .unwrap();
    assert!(client.drain_cleanup().await.is_empty());
    client.shutdown().await.unwrap();
    std::fs::remove_file(&path).unwrap();
    let lock = path.with_extension("lock");
    if lock.exists() {
        std::fs::remove_file(lock).unwrap();
    }
    remove_wrapper_files(&root);
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancellation_during_acquisition_rolls_back_the_known_id() {
    let guard = TestServer::new().await.unwrap();
    let (client, root) = wrapper(&guard);
    std::fs::write(root.join("hold-create"), "").unwrap();
    let acquiring = client.clone();
    let task = tokio::spawn(async move { acquiring.owned_session("cancelled-create").await });
    retry_until(Duration::from_secs(5), || async {
        root.join("created").exists()
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    std::fs::remove_file(root.join("hold-create")).unwrap();
    assert!(client.drain_cleanup().await.is_empty());
    assert!(
        guard
            .server()
            .session("cancelled-create")
            .await
            .unwrap()
            .is_none()
    );
    client.shutdown().await.unwrap();
    remove_wrapper_files(&root);
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn discovery_reports_two_roots_stale_socket_failed_probe_and_bounds_without_starting() {
    let a = TestServer::new().await.unwrap();
    let b = TestServer::new().await.unwrap();
    let first = a.socket_path().parent().unwrap().to_owned();
    let second = b.socket_path().parent().unwrap().to_owned();
    let stale = first.join("stale");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    std::os::unix::fs::symlink(a.socket_path(), first.join("symlink")).unwrap();
    let mut discovery =
        Discovery::new([first.clone(), second, first.join("absent"), first.clone()]);
    let report = discovery.scan().await;
    assert_eq!(report.servers.len(), 2, "{report:?}");
    assert!(report.truncated.is_none());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.path == stale && matches!(d.reason, DiscoverySkip::Probe(_)))
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| matches!(d.reason, DiscoverySkip::Io(_)))
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| matches!(d.reason, DiscoverySkip::Symlink))
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| matches!(d.reason, DiscoverySkip::DuplicatePath))
    );
    assert!(
        Server::builder()
            .socket_path(&stale)
            .build()
            .unwrap()
            .generation()
            .await
            .is_err()
    );
    std::fs::remove_file(stale).unwrap();
    std::fs::remove_file(first.join("symlink")).unwrap();
    discovery.max_probes = 0;
    assert_eq!(
        discovery.scan().await.truncated,
        Some(DiscoveryTruncation::Probes)
    );
    discovery.max_probes = 64;
    discovery.max_entries = 0;
    assert_eq!(
        discovery.scan().await.truncated,
        Some(DiscoveryTruncation::Entries)
    );
    discovery.max_entries = 1024;
    discovery.timeout = Duration::ZERO;
    assert_eq!(
        discovery.scan().await.truncated,
        Some(DiscoveryTruncation::Time)
    );
    discovery.timeout = Duration::from_secs(2);
    discovery.executable = "/bin/false".into();
    let report = discovery.scan().await;
    assert!(report.servers.is_empty());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| matches!(d.reason, DiscoverySkip::Probe(_)))
    );
    a.shutdown().await.unwrap();
    b.shutdown().await.unwrap();
}

#[tokio::test]
async fn server_find_create_proves_startup_and_competing_calls_reuse() {
    let fixture = TestServer::new().await.unwrap();
    let path = fixture.socket_path().parent().unwrap().join("owned-server");
    let server = Server::builder()
        .socket_path(&path)
        .config_file("/dev/null")
        .build()
        .unwrap();
    let (a, b) = tokio::join!(
        server.find_or_create_server(),
        server.find_or_create_server()
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.is_created(), b.is_created());
    assert!(server.sessions().await.unwrap().is_empty());
    assert!(matches!(
        server.find_or_create_server().await.unwrap(),
        FindOrCreate::Reused(_)
    ));
    let generation = server.generation().await.unwrap();
    for result in [a, b] {
        if let FindOrCreate::Created(owner) = result {
            owner.close().await.unwrap();
        }
    }
    retry_until(Duration::from_secs(5), || async {
        process_exited(generation.pid())
    })
    .await
    .unwrap();
    assert!(server.drain_cleanup().await.is_empty());
    server.shutdown().await.unwrap();
    std::fs::remove_file(&path).unwrap();
    let lock = path.with_extension("lock");
    if lock.exists() {
        std::fs::remove_file(lock).unwrap();
    }
    fixture.shutdown().await.unwrap();
}

#[tokio::test]
async fn find_create_session_starts_an_absent_endpoint_and_start_failure_is_visible() {
    let fixture = TestServer::new().await.unwrap();
    let root = fixture.socket_path().parent().unwrap();
    let path = root.join("first-session");
    let client = Server::builder()
        .socket_path(&path)
        .config_file("/dev/null")
        .build()
        .unwrap();
    let first = client.find_or_create_session("first").await.unwrap();
    let generation = client.generation().await.unwrap();
    assert!(first.is_created());
    assert!(matches!(
        client.find_or_create_session("first").await.unwrap(),
        FindOrCreate::Reused(_)
    ));
    if let FindOrCreate::Created(owner) = first {
        owner.close().await.unwrap();
    }
    retry_until(Duration::from_secs(5), || async {
        process_exited(generation.pid())
    })
    .await
    .unwrap();
    assert!(client.drain_cleanup().await.is_empty());
    client.shutdown().await.unwrap();
    std::fs::remove_file(&path).unwrap();
    let lock = path.with_extension("lock");
    if lock.exists() {
        std::fs::remove_file(lock).unwrap();
    }
    let absent = root.join("absent");
    let failing = Server::builder()
        .socket_path(absent.join("socket"))
        .config_file("/dev/null")
        .build()
        .unwrap();
    assert!(failing.find_or_create_server().await.is_err());
    assert!(!absent.exists());
    assert!(failing.drain_cleanup().await.is_empty());
    failing.shutdown().await.unwrap();
    fixture.shutdown().await.unwrap();
}

fn process_exited(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
        stat.rsplit_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with('Z'))
    })
}
