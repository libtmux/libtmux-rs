/// Pin the tmux wording that says how an option was refused.
///
/// The three answers need three different fixes, and tmux distinguishes
/// them only in stderr: every one of these exits 1. It also spells a
/// rejected value two ways, "bad value" for a flag and "value is invalid"
/// for a number, which is why the kind exists rather than the text.
#[cfg(feature = "test-support")]
#[tokio::test]
async fn real_tmux_compat_error_option_refusal_wording_is_recognized() {
    use crate::test::TestServer;
    use crate::{Error, ErrorKind, OptionErrorKind};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    for (name, value, expected) in [
        ("no-such-option", "x", OptionErrorKind::Unknown),
        // A prefix of `status-left`, `status-left-length`, and
        // `status-left-style` on every supported release, so tmux will not
        // choose. A release that left only one of them would turn this
        // answer into a different kind, which is the point of pinning it.
        ("status-l", "x", OptionErrorKind::Ambiguous),
        ("mouse", "notabool", OptionErrorKind::BadValue),
        (
            "status-left-length",
            "notanumber",
            OptionErrorKind::BadValue,
        ),
    ] {
        let error = server
            .set_global_option(name, value)
            .await
            .expect_err("tmux refuses it");
        assert!(
            matches!(&error, Error::OptionRejected { kind, .. } if *kind == expected),
            "{name}={value} should be {expected:?}, got {error:?}",
        );
        assert_eq!(error.kind(), ErrorKind::Refused);
        assert!(!error.is_object_gone(), "a refusal is not a missing object");
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Pin the tmux wording that separates a missing target from a refusal.
///
/// `Error::refused` reads tmux's stderr because tmux exits 1 for both, so
/// this asserts against the tmux the lane is running rather than against
/// the source this was written from. Every compatibility lane runs it, so
/// a release that rewords these is a failure here rather than a silently
/// wrong `is_object_gone` in the field.
#[cfg(feature = "test-support")]
#[tokio::test]
async fn real_tmux_compat_error_missing_target_wording_is_recognized() {
    use crate::ErrorKind;
    use crate::test::TestServer;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("compat-missing").await.expect("session");

    // One live session, so tmux can resolve a current target and reports
    // the specific object it could not find.
    for (label, error) in [
        (
            "window",
            server
                .window_by_id(&"@4242".parse().expect("a window id"))
                .await
                .map(|found| assert!(found.is_none(), "the window does not exist"))
                .err(),
        ),
        (
            "pane",
            server
                .pane_by_id(&"%4242".parse().expect("a pane id"))
                .await
                .map(|found| assert!(found.is_none(), "the pane does not exist"))
                .err(),
        ),
    ] {
        assert!(error.is_none(), "a lookup reports absence, not {label}");
    }

    // A mutation against a target tmux does not have is where the wording
    // matters: it is the only signal separating this from a bad argument.
    let mut window = session.windows().await.expect("windows").remove(0);
    let doomed = session
        .new_window(crate::NewWindowOptions::new("doomed").command("sleep 300"))
        .await
        .expect("window");
    let mut stale = doomed.clone();
    doomed.kill().await.expect("the window is killed");

    let error = stale.rename("gone").await.expect_err("the window is gone");
    assert_eq!(
        error.kind(),
        ErrorKind::ObjectGone,
        "tmux 'can't find window' is recognized: {error}",
    );

    // And a refusal that is not a missing target stays a refusal, so the
    // classification is not simply calling everything gone.
    let refused = server
        .delete_buffer("never-existed")
        .await
        .expect_err("tmux has no such buffer");
    assert_eq!(refused.kind(), ErrorKind::Refused, "{refused}");

    // With no session left, tmux cannot resolve a current target and says
    // so instead, for the same request. Both wordings mean gone.
    window
        .rename("last")
        .await
        .expect("the window still exists");
    session.kill().await.expect("the session is killed");

    let error = stale
        .rename("still gone")
        .await
        .expect_err("the window is gone");
    assert_eq!(
        error.kind(),
        ErrorKind::ObjectGone,
        "tmux 'no current target' is recognized: {error}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Pin the tmux wording that says the server, not the request, is the
/// problem.
///
/// tmux exits 1 for a command it refused and for a command that found no
/// server, and separates them only in stderr. Reading the second as the
/// first tells a caller to fix arguments that were never the trouble.
#[cfg(feature = "test-support")]
#[tokio::test]
async fn real_tmux_compat_error_absent_server_wording_is_recognized() {
    use std::time::Duration;

    use crate::test::{TestServer, retry_until};
    use crate::{Command, ErrorKind, ServerGoneKind};

    let mut guard = TestServer::builder().start().await.expect("tmux starts");
    guard.session("compat-gone").await.expect("session");

    guard
        .server()
        .cmd(Command::new("kill-server"))
        .await
        .expect("the server is killed");

    // tmux stops answering on the socket before the kernel has a status
    // for the process behind it, so this waits for the daemon rather than
    // for a duration.
    retry_until(Duration::from_secs(5), async || {
        !guard.daemon_state().is_running()
    })
    .await
    .expect("the daemon exits");

    let error = guard
        .server()
        .sessions()
        .await
        .expect_err("there is no server to list");
    assert_eq!(
        error.kind(),
        ErrorKind::ServerGone,
        "tmux 'no server running' is recognized: {error}",
    );
    assert!(
        matches!(&error, crate::Error::ServerGone { kind, .. } if *kind == ServerGoneKind::NotRunning),
        "the absence is named: {error:?}",
    );
    assert!(
        !error.is_object_gone(),
        "an absent server is not a missing object: {error}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}
/// Pin the half of tmux's answer that says how much a miss proves.
///
/// tmux echoes back the part of a target it could not resolve, and drops
/// the session from it when that part is a coordinate. `Error::refused`
/// reads the sigil on what comes back to tell a place from an object, so a
/// release that echoed the whole target, or that stripped the sigil, would
/// change what `is_object_gone` answers -- and that is the predicate a
/// caller consults before discarding a handle. Asserted against whichever
/// tmux the lane is running.
#[cfg(feature = "test-support")]
#[tokio::test]
async fn real_tmux_compat_a_coordinate_miss_is_not_an_object_miss() {
    use crate::test::TestServer;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("compat-echo").await.expect("session");

    for (target, gone) in [
        // A place in a session that holds nothing there.
        (format!("{}:9", session.id()), false),
        // A window id no server ever issued.
        (format!("{}:@4242", session.id()), true),
    ] {
        let result = server
            .cmd(
                crate::Command::new("unlink-window")
                    .arg("-t")
                    .arg(target.clone()),
            )
            .await
            .expect("the command runs");
        assert!(!result.success(), "{target} resolves to nothing");

        let stderr = result.stderr_lossy().into_owned();
        let error = crate::Error::refused(
            "unlink-window",
            result.exit_code(),
            stderr.clone(),
            Some(std::ffi::OsStr::new(&target)),
        );
        assert_eq!(
            error.is_object_gone(),
            gone,
            "{target} answered {stderr:?}, classified {error:?}",
        );
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}
