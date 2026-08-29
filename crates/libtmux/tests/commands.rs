//! Buffers, keys, formats, and scoped operations against real tmux.

#![cfg(feature = "test-support")]

use std::sync::Arc;
use std::time::Duration;

use libtmux::test::{TestServer, retry_until};
use libtmux::{ChannelWait, Command, NewWindowOptions, SplitDirection, SplitOptions};

#[tokio::test]
async fn buffers_hold_exact_bytes_and_report_absence() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    assert!(server.buffer_names().await.expect("names").is_empty());
    assert!(
        server.buffer("absent").await.expect("read").is_none(),
        "an unknown buffer is absent rather than an error",
    );

    // A buffer holds bytes, including ones no string type would keep.
    server
        .set_buffer(Some("payload"), "line one\nline two\ttabbed")
        .await
        .expect("buffer is stored");

    assert_eq!(
        server.buffer("payload").await.expect("read"),
        Some(b"line one\nline two\ttabbed".to_vec()),
    );

    let names = server.buffer_names().await.expect("names");
    assert_eq!(names, ["payload"]);

    server
        .delete_buffer("payload")
        .await
        .expect("buffer is deleted");
    assert!(server.buffer("payload").await.expect("read").is_none());
    assert!(
        server.delete_buffer("payload").await.is_err(),
        "deleting a buffer that is gone is a failure, not a silent success",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn key_bindings_can_be_added_and_removed() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    server
        .bind_key("prefix", "Y", "display-message bound")
        .await
        .expect("key is bound");

    let listed = server
        .key_bindings(Some("prefix"))
        .await
        .expect("bindings are listed");
    assert!(
        listed
            .iter()
            .any(|line| line.contains(" Y ") && line.contains("display-message")),
        "the new binding appears in tmux's own listing",
    );

    server
        .unbind_key("prefix", "Y")
        .await
        .expect("key is unbound");
    let listed = server
        .key_bindings(Some("prefix"))
        .await
        .expect("bindings are listed");
    assert!(!listed.iter().any(|line| line.contains(" Y ")));

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn formats_expand_against_a_target() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("formatted").await.expect("session");

    // A format is evaluated against a pane, and resolves the session and
    // window around it from there.
    let pane = session
        .panes()
        .await
        .expect("panes")
        .into_iter()
        .next()
        .expect("one pane");

    // The trailing newline display-message adds is framing, not content.
    let name = server
        .format(Some(&pane), "#{session_name}")
        .await
        .expect("format expands");
    assert_eq!(name.as_bytes(), b"formatted");

    let combined = server
        .format(Some(&pane), "#{session_name}:#{session_windows}")
        .await
        .expect("format expands");
    assert_eq!(combined.as_bytes(), b"formatted:1");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn sourcing_a_file_applies_its_commands() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let directory = tempfile::tempdir().expect("temporary directory");
    let config = directory.path().join("extra.conf");
    std::fs::write(&config, "set-option -s @sourced yes\n").expect("config is written");

    server.source_file(&config).await.expect("file is sourced");
    assert_eq!(
        server
            .get_option("@sourced")
            .await
            .expect("read")
            .expect("the sourced option is set")
            .as_bytes(),
        b"yes",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A caller's own error type, carrying `From<libtmux::Error>`.
///
/// That conversion is what lets a scope's setup and teardown failures join
/// the same channel as the operation's own.
#[derive(Debug, PartialEq)]
enum Failure {
    Deliberate,
    Tmux(String),
}

impl From<libtmux::Error> for Failure {
    fn from(error: libtmux::Error) -> Self {
        Self::Tmux(error.to_string())
    }
}

#[tokio::test]
async fn scoped_operations_clean_up_after_success_and_failure() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // Success: the session exists inside the scope and not after it.
    let seen = server
        .with_session("scoped", async |session| {
            Ok::<_, libtmux::Error>(session.id().to_string())
        })
        .await
        .expect("the scope runs and the operation succeeds");
    assert!(seen.starts_with('$'));
    assert!(server.sessions().await.expect("sessions").is_empty());

    // Failure: the operation's error comes back, and cleanup still ran.
    // The operation's error comes back directly: one `?`, not two.
    let outcome = server
        .with_session("failing", async |_session| {
            Err::<(), Failure>(Failure::Deliberate)
        })
        .await;
    assert_eq!(outcome, Err(Failure::Deliberate));
    assert!(
        server.sessions().await.expect("sessions").is_empty(),
        "cleanup runs even when the operation failed",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn scoped_windows_and_panes_nest() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("nested").await.expect("session");

    let before = session.windows().await.expect("windows").len();

    let pane_count = session
        .with_window(
            NewWindowOptions::new("temporary").command("sleep 300"),
            async |window| {
                window
                    .with_pane(
                        SplitOptions::new(SplitDirection::Below).command("sleep 300"),
                        async |_pane| {
                            Ok::<_, libtmux::Error>(window.panes().await.expect("panes").len())
                        },
                    )
                    .await
            },
        )
        .await
        .expect("both scopes run and the operation succeeds");

    assert_eq!(pane_count, 2, "the scoped pane existed inside its scope");
    assert_eq!(
        session.windows().await.expect("windows").len(),
        before,
        "the scoped window is gone afterwards",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn scoped_operations_clean_up_after_cancellation() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server().clone();
    let anchor = server.new_session("anchor").await.expect("session");
    let window = anchor
        .active_window()
        .await
        .expect("window lookup")
        .expect("window");
    let ready = Arc::new(tokio::sync::Barrier::new(4));

    let session_scope = tokio::spawn({
        let server = server.clone();
        let ready = Arc::clone(&ready);
        async move {
            server
                .with_session("cancelled-session", async move |_session| {
                    ready.wait().await;
                    std::future::pending::<Result<(), libtmux::Error>>().await
                })
                .await
        }
    });
    let window_scope = tokio::spawn({
        let anchor = anchor.clone();
        let ready = Arc::clone(&ready);
        async move {
            anchor
                .with_window("cancelled-window", async move |_window| {
                    ready.wait().await;
                    std::future::pending::<Result<(), libtmux::Error>>().await
                })
                .await
        }
    });
    let pane_scope = tokio::spawn({
        let window = window.clone();
        let ready = Arc::clone(&ready);
        async move {
            window
                .with_pane(SplitDirection::Below, async move |_pane| {
                    ready.wait().await;
                    std::future::pending::<Result<(), libtmux::Error>>().await
                })
                .await
        }
    });

    tokio::time::timeout(Duration::from_secs(5), ready.wait())
        .await
        .expect("all scoped objects are created");
    session_scope.abort();
    window_scope.abort();
    pane_scope.abort();
    for scope in [session_scope, window_scope, pane_scope] {
        assert!(scope.await.expect_err("scope is aborted").is_cancelled());
    }

    let cleanup = retry_until(Duration::from_secs(5), async || {
        matches!(server.sessions().await, Ok(objects) if objects.len() == 1)
            && matches!(anchor.windows().await, Ok(objects) if objects.len() == 1)
            && matches!(window.panes().await, Ok(objects) if objects.len() == 1)
    })
    .await;
    let sessions = server.sessions().await.expect("sessions").len();
    let windows = anchor.windows().await.expect("windows").len();
    let panes = window.panes().await.expect("panes").len();
    guard.shutdown().await.expect("tmux fixture shuts down");

    assert!(
        cleanup.is_ok(),
        "aborted scopes left sessions={sessions}, windows={windows}, panes={panes}",
    );
}

#[tokio::test]
async fn shell_commands_run_through_tmux_and_report_their_output() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // tmux 3.3 through 3.4 route run-shell output into a pane's copy-mode
    // buffer rather than to the client, and still exit zero. The crate refuses
    // there rather than reporting an empty listing, which would be a wrong
    // answer the caller could not tell from a command that printed nothing.
    let outcome = server.run_shell("printf 'first\\nsecond\\n'").await;
    let Ok(lines) = outcome else {
        let error = outcome.expect_err("refused");
        assert!(
            matches!(&error, libtmux::Error::CapabilityDefective { capability, .. }
                if *capability == "run-shell output"),
            "an affected release refuses with the reason, got {error:?}",
        );
        assert_eq!(error.kind(), libtmux::ErrorKind::UnsupportedVersion);
        guard.shutdown().await.expect("tmux fixture shuts down");
        return;
    };

    assert_eq!(
        lines
            .iter()
            .map(libtmux::TmuxText::as_bytes)
            .collect::<Vec<_>>(),
        [&b"first"[..], &b"second"[..]],
    );

    // A command producing nothing is an empty listing, not a failure.
    assert!(server.run_shell("true").await.expect("runs").is_empty());

    // Background acceptance says nothing about the command's own outcome.
    server
        .spawn_shell("false")
        .await
        .expect("tmux accepts a background command whatever it later does");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn wait_for_channels_lock_and_release() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // Locking an unheld channel takes it; unlocking releases it again.
    server
        .lock_channel("gate")
        .await
        .expect("channel is locked");
    server
        .unlock_channel("gate")
        .await
        .expect("channel is unlocked");

    // Signalling a channel nobody waits on is accepted, and tmux keeps it:
    // the next wait spends the latch rather than blocking.
    server
        .signal_channel("gate")
        .await
        .expect("channel is signalled");
    assert_eq!(
        server
            .wait_for_channel("gate", Duration::from_secs(5))
            .await
            .expect("waiting is not an error"),
        ChannelWait::Signalled,
        "a signal with nobody waiting is kept, not dropped",
    );
    // One-shot: the latch is spent, so the next wait runs out of time. That is
    // an outcome, not an error, which is the distinction the return type
    // exists to carry.
    assert_eq!(
        server
            .wait_for_channel("gate", Duration::from_millis(300))
            .await
            .expect("running out of time is not an error"),
        ChannelWait::TimedOut,
        "the latch releases one wait, not every later one",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Waiting blocks until something signals, rather than returning at once.
///
/// The latch makes the easy case indistinguishable from a broken one: a wait
/// that returned immediately would satisfy a test that only checks the
/// outcome. So this signals from elsewhere, after a delay, and requires that
/// the wait actually spanned it.
#[tokio::test]
async fn a_wait_blocks_until_something_signals_the_channel() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let signaller = server.clone();
    let signalling = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        signaller
            .signal_channel("ready")
            .await
            .expect("channel is signalled");
    });

    let started = std::time::Instant::now();
    let outcome = server
        .wait_for_channel("ready", Duration::from_secs(10))
        .await
        .expect("waiting is not an error");
    let waited = started.elapsed();

    signalling.await.expect("the signalling task finishes");
    assert_eq!(outcome, ChannelWait::Signalled);
    assert!(
        waited >= Duration::from_millis(300),
        "the wait returned in {waited:?}, so it did not wait for the signal",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn client_operations_need_a_client_and_report_when_there_is_none() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    server.new_session("headless").await.expect("session");

    // A test server has no terminal attached, so there are no clients and the
    // operations that need one fail loudly rather than pretending to work.
    assert!(server.clients().await.expect("clients list").is_empty());
    assert!(
        server.display_popup(None, "true").await.is_err(),
        "a popup needs a client with a terminal",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Exercise `Client` against the one kind of client a headless test can make.
///
/// Every other client needs a terminal. A control-mode attach does not, and
/// tmux counts it as a client like any other, so it is what makes the
/// positive case testable at all.
#[cfg(feature = "control-mode")]
#[tokio::test]
async fn a_client_reports_itself_while_it_is_attached() {
    use libtmux::control::ControlMode;
    use std::time::Duration;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let first = server.new_session("client-one").await.expect("session");
    let second = server.new_session("client-two").await.expect("session");

    let control = ControlMode::attach(server, first.id())
        .await
        .expect("control mode attaches");

    // tmux registers the client as part of attaching, but the listing is a
    // separate command, so wait for it rather than assuming the order.
    retry_until(Duration::from_secs(10), async || {
        server
            .clients()
            .await
            .is_ok_and(|clients| !clients.is_empty())
    })
    .await
    .expect("the attached client is listed");

    let mut client = server.clients().await.expect("clients").remove(0);
    assert!(client.is_control_mode(), "this client attached with -C");
    assert!(!client.name().as_bytes().is_empty());
    assert!(client.pid() > 0);

    // A client is attached to exactly one session, and switching moves it.
    client
        .switch_to(&second)
        .await
        .expect("the client switches session");
    retry_until(Duration::from_secs(10), async || {
        second
            .refreshed()
            .await
            .is_ok_and(|session| session.is_attached())
    })
    .await
    .expect("the second session reports the client");

    client.refresh().await.expect("the client still exists");

    // Detaching consumes the handle, and the listing empties out.
    client.detach().await.expect("the client detaches");
    retry_until(Duration::from_secs(10), async || {
        server
            .clients()
            .await
            .is_ok_and(|clients| clients.is_empty())
    })
    .await
    .expect("the detached client stops being listed");

    let _ = control.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[cfg(feature = "control-mode")]
#[tokio::test]
async fn server_operations_reject_foreign_handles() {
    use libtmux::control::ControlMode;
    use libtmux::{Chooser, Error, ErrorKind};

    macro_rules! error_kind {
        ($future:expr) => {
            match $future.await {
                Err(error @ Error::ServerMismatch { .. }) => error.kind(),
                Err(error) => panic!("foreign handle returned {error:?}"),
                Ok(_) => panic!("foreign handle was accepted"),
            }
        };
    }

    let left_guard = TestServer::builder().start().await.expect("tmux starts");
    let right_guard = TestServer::builder().start().await.expect("tmux starts");
    let left = left_guard.server();
    let right = right_guard.server();
    let left_session = left.new_session("left").await.expect("left session");
    let right_session = right.new_session("right").await.expect("right session");
    let right_pane = right_session.panes().await.expect("right panes").remove(0);
    let control = ControlMode::attach(right, right_session.id())
        .await
        .expect("control mode attaches");

    retry_until(Duration::from_secs(10), async || {
        right
            .clients()
            .await
            .is_ok_and(|clients| !clients.is_empty())
    })
    .await
    .expect("the foreign client is listed");
    let foreign_client = right.clients().await.expect("right clients").remove(0);

    let kinds = [
        error_kind!(left.format(Some(&right_pane), "#{session_name}")),
        error_kind!(left.display_popup(Some(&foreign_client), "true")),
        error_kind!(left.display_menu(
            Some(&foreign_client),
            "menu",
            [("Item".into(), "i".into(), "display-message item".into())],
        )),
        error_kind!(left.command_prompt(
            Some(&foreign_client),
            Some("question"),
            "display-message answer",
        )),
        error_kind!(left.choose(Chooser::Tree, Some(&foreign_client))),
        error_kind!(left.find_window(Some(&foreign_client), "needle")),
        error_kind!(left.display_panes(Some(&foreign_client))),
        error_kind!(foreign_client.switch_to(&left_session)),
    ];

    assert_eq!(kinds, [ErrorKind::InvalidInput; 8]);

    let _ = control.shutdown().await;
    right_guard
        .shutdown()
        .await
        .expect("right tmux fixture shuts down");
    left_guard
        .shutdown()
        .await
        .expect("left tmux fixture shuts down");
}

#[tokio::test]
async fn pane_modes_enter_and_leave() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("modes").await.expect("session");
    let mut pane = session
        .panes()
        .await
        .expect("panes")
        .into_iter()
        .next()
        .expect("one pane");

    assert!(!pane.is_in_mode(), "a fresh pane has no mode open");

    pane.copy_mode().await.expect("copy mode is entered");
    pane.refresh().await.expect("the pane still exists");
    assert!(pane.is_in_mode(), "copy mode is a pane mode");

    pane.exit_mode().await.expect("the mode is cancelled");
    pane.refresh().await.expect("the pane still exists");
    assert!(!pane.is_in_mode(), "cancelling leaves the mode");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A mode that is not copy mode must be leavable too.
///
/// `exit_mode` sent the cancel key, which reaches a pane through the copy-mode
/// key table. Clock mode and tree mode have none, so the key was answered "not
/// in a mode" while the pane stayed in one, and `is_in_mode` and `exit_mode`
/// disagreed about the same pane. `clock_mode` is a public way in, so the
/// crate could put a pane into a state only a raw command could clear.
///
/// The old test entered copy mode, which is the one mode the cancel key does
/// leave, so it passed throughout.
#[tokio::test]
async fn a_pane_leaves_a_mode_that_is_not_copy_mode() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("clocked").await.expect("session");
    let mut pane = session
        .panes()
        .await
        .expect("panes")
        .into_iter()
        .next()
        .expect("one pane");

    pane.clock_mode().await.expect("clock mode is entered");
    pane.refresh().await.expect("the pane still exists");
    assert!(pane.is_in_mode(), "clock mode is a pane mode");

    pane.exit_mode().await.expect("clock mode is left");
    pane.refresh().await.expect("the pane still exists");
    assert!(
        !pane.is_in_mode(),
        "a mode that is not copy mode is left as well"
    );

    // A pane in no mode is left alone rather than refused, so a caller can
    // reach a known state without asking first.
    pane.exit_mode()
        .await
        .expect("leaving no mode at all is not a failure");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A pane's edge flags must follow the split that made it.
///
/// These read from the snapshot rather than dispatching, so nothing catches
/// them being wired to the wrong format field: `pane_at_left` and
/// `pane_at_right` differ only in a word.
#[tokio::test]
async fn edge_flags_follow_the_split_that_made_them() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("edges").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a session has a window");

    let only = window.panes().await.expect("panes").remove(0);
    assert!(only.is_at_left(), "an undivided pane touches both edges");
    assert!(only.is_at_right(), "an undivided pane touches both edges");

    only.split(SplitOptions::new(SplitDirection::Right))
        .await
        .expect("the pane splits sideways");

    let panes = window.panes().await.expect("panes");
    assert_eq!(panes.len(), 2, "the split produced a second pane");
    let (left, right) = (&panes[0], &panes[1]);

    assert!(left.is_at_left(), "the original keeps the left edge");
    assert!(!left.is_at_right(), "and gives up the right one");
    assert!(right.is_at_right(), "the new pane takes the right edge");
    assert!(!right.is_at_left(), "and does not hold the left one");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A global window option must be read from the global table, not the window
/// the server happens to be pointing at.
///
/// A plain round trip cannot say that: writing and reading through the same
/// wrong scope agrees with itself. So this gives the window a different value
/// and checks the global read is unmoved by it.
///
/// What this cannot pin is the `-w` half of tmux's `-w -g`. tmux resolves an
/// option's table from its name, so `show-options -g main-pane-width` and
/// `-wg` return the same value and no assertion here can tell them apart.
/// Dropping `-w` in the scope leaves this test passing, which was checked
/// rather than assumed.
#[tokio::test]
async fn a_global_window_option_is_read_globally() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("options").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a session has a window");

    server
        .set_global_window_option("main-pane-width", "123")
        .await
        .expect("the global window option is set");
    window
        .set_option("main-pane-width", "456")
        .await
        .expect("the window takes a value of its own");

    let read = server
        .get_global_window_option("main-pane-width")
        .await
        .expect("the option is readable")
        .expect("the option is set");
    assert_eq!(
        read.as_str().expect("the width is text"),
        "123",
        "the global read is not answered by the window's own value"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Piping a pane must start and stop with the same call.
///
/// `pipe-pane` toggles when given no command, so a caller who cannot stop it
/// leaves tmux writing into a process for the life of the pane.
///
/// The sink lives in a directory of its own rather than beside the fixture's
/// socket. An earlier version wrote it next to the socket and removed it on
/// the way out, which meant a failing run left a file behind that outlived
/// the fixture's own cleanup and kept the directory in the shared root --
/// `just fixture-root` reported it. A temporary directory is removed whether
/// the test returns or panics.
#[tokio::test]
async fn a_pane_pipes_until_it_is_told_to_stop() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("piped").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);

    let mut pane = pane;
    assert!(!pane.is_piped(), "nothing yet");

    let sink_dir = tempfile::Builder::new()
        .prefix("libtmux-pipe-")
        .tempdir()
        .expect("a directory of its own");
    let sink = sink_dir.path().join("seen");

    pane.pipe(Some(format!("cat >{}", sink.display())))
        .await
        .expect("the pane pipes");
    pane.refresh().await.expect("the pane still exists");
    assert!(pane.is_piped(), "now piping");

    pane.send_keys("printf 'piped\n'").await.expect("keys");
    pane.send_key_names(["Enter"]).await.expect("enter");
    retry_until(Duration::from_secs(10), async || {
        std::fs::read(&sink).is_ok_and(|seen| seen.windows(5).any(|word| word == b"piped"))
    })
    .await
    .expect("what the pane printed reaches the pipe");

    pane.pipe(None::<String>).await.expect("the pipe stops");
    pane.refresh().await.expect("the pane still exists");
    assert!(!pane.is_piped(), "the same call with no command stops it");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A pane must report the terminal tmux gave it.
///
/// Reads from the snapshot, so a field wired to the wrong name would return
/// some other pane's string rather than fail.
#[tokio::test]
async fn a_pane_reports_its_own_terminal() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("ttys").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);

    let reported = pane.tty().as_str().expect("a tty path is text").to_owned();
    let asked = server
        .format(Some(&pane), "#{pane_tty}")
        .await
        .expect("tmux answers for the same pane");

    assert_eq!(
        reported,
        asked.as_str().expect("a tty path is text"),
        "the accessor and tmux name the same terminal"
    );
    assert!(reported.starts_with("/dev/"), "and it is a device path");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A window's activity and bell flags must each read their own field.
///
/// They differ by a word and both are `bool`, so a swap type-checks. Reading
/// them on a fresh window cannot catch one: both are false there, and a swap
/// between two equal values is invisible. So this drives activity high and
/// leaves the bell low first, which was measured rather than assumed -- the
/// first version of this test passed with `has_bell` wired to the activity
/// field.
#[tokio::test]
async fn window_flags_each_read_their_own_field() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("flags").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a session has a window");

    // Activity is only recorded for a window nobody is looking at, so the
    // second window is what makes the first one eligible.
    server
        .set_global_window_option("monitor-activity", "on")
        .await
        .expect("activity is monitored");
    session
        .new_window(NewWindowOptions::new("elsewhere"))
        .await
        .expect("a second window takes the focus");

    let panes = window.panes().await.expect("panes");
    panes[0].send_keys("printf 'noise\n'").await.expect("keys");
    panes[0].send_key_names(["Enter"]).await.expect("enter");

    let mut window = window;
    retry_until(Duration::from_secs(10), async || {
        window.refresh().await.is_ok() && window.has_activity()
    })
    .await
    .expect("the background window records activity");

    assert!(
        !window.has_bell(),
        "nothing rang a bell, so the two flags differ and a swap would show"
    );

    for (reported, format) in [
        (window.has_activity(), "#{window_activity_flag}"),
        (window.has_bell(), "#{window_bell_flag}"),
    ] {
        let asked = server
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(window.id().to_string())
                    .arg(format),
            )
            .await
            .expect("tmux answers");
        let asked = asked.stdout_lossy().trim() == "1";
        assert_eq!(reported, asked, "{format} agrees with the accessor");
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Pasting a buffer must put its bytes into the pane.
#[tokio::test]
async fn a_buffer_pastes_into_a_pane() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("pasted").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);

    server
        .set_buffer(Some("greeting"), "printf 'pasted-here\n'")
        .await
        .expect("the buffer is set");
    pane.paste_buffer(Some("greeting"))
        .await
        .expect("the buffer pastes");
    pane.send_key_names(["Enter"]).await.expect("enter");

    retry_until(Duration::from_secs(10), async || {
        pane.capture().await.is_ok_and(|lines| {
            lines
                .iter()
                .any(|line| line.as_bytes().windows(11).any(|w| w == b"pasted-here"))
        })
    })
    .await
    .expect("what was pasted runs in the pane");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Searching a window's panes must find one that matches.
///
/// The lenient form returns an empty vector rather than an error, so a search
/// that never matched and a search that failed look alike to a caller. This
/// covers the case where it does match, which is the one that would be silent.
#[cfg(feature = "query")]
#[tokio::test]
async fn searching_a_window_finds_the_pane_that_matches() {
    use libtmux::query::Filterable as _;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("searched").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a session has a window");

    let panes = window.panes().await.expect("panes");
    let wanted = panes[0].id().to_string();

    let fields = libtmux::Pane::filter_fields();
    let found = window
        .search_panes_or_empty(fields.pane_id.eq(wanted.as_str()))
        .await;

    assert_eq!(found.len(), 1, "the pane that matches is returned");
    assert_eq!(found[0].id().to_string(), wanted);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A client's own fields must each name the client, not its neighbour.
///
/// `tty` and `term_name` are both `TmuxText` off the same projection, so one
/// reading the other's field type-checks and returns a plausible string. A
/// control client makes the two differ -- it has no terminal, so `client_tty`
/// is empty while `client_termname` is not -- which is what lets a swap show.
///
/// `suspend` is deliberately absent: it sends SIGTSTP to the client, and the
/// only client here is the control connection this test is holding.
#[cfg(feature = "control-mode")]
#[tokio::test]
async fn a_client_reports_its_own_terminal_and_type() {
    use libtmux::control::ControlMode;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("client-fields").await.expect("session");
    let control = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches");

    retry_until(Duration::from_secs(10), async || {
        server
            .clients()
            .await
            .is_ok_and(|clients| !clients.is_empty())
    })
    .await
    .expect("the attached client is listed");

    let client = server.clients().await.expect("clients").remove(0);
    let name = client.name().to_string_lossy().into_owned();

    assert_ne!(
        client.tty().as_str().expect("client text"),
        client.term_name().as_str().expect("client text"),
        "the two fields differ here, so a swap between them would show"
    );

    for (reported, format) in [
        (client.tty(), "#{client_tty}"),
        (client.term_name(), "#{client_termname}"),
    ] {
        // `-t` rather than `-c`, because `-c` did not take an argument until
        // 3.5a: `display-message`'s option string is `acd:INpt:F:v` on 3.2a,
        // so the client name becomes a positional argument, the command
        // exceeds its one-argument maximum, and tmux answers with a usage
        // error. Its own usage line advertises `[-c target-client]` there
        // anyway, so the help disagrees with the parser.
        //
        // `-t` resolves the client on every supported release, and is what
        // `Client` itself uses. Checked rather than assumed: with the client
        // attached as `screen-256color` and the asking process at `vt100`,
        // both 3.2a and 3.7c answer `screen-256color`, so this reports the
        // target rather than the caller.
        let asked = server
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(&name)
                    .arg(format),
            )
            .await
            .expect("tmux answers for this client");
        assert_eq!(
            reported.as_str().expect("client text"),
            asked.stdout_lossy().trim(),
            "{format} agrees with the accessor"
        );
    }

    assert!(
        !client.is_readonly(),
        "a client that attached without -r may act"
    );
    client.redraw().await.expect("the client redraws");

    let _ = control.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// The remaining dispatch-only commands must reach tmux and be accepted.
///
/// These change something a headless server cannot show back -- a prefix key
/// arriving, a prompt history no client has filled -- so what is covered is
/// that the command is built and accepted, not what it did.
#[tokio::test]
async fn dispatch_only_commands_are_accepted() {
    use libtmux::since;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("dispatch").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);

    pane.send_prefix().await.expect("the prefix key is sent");

    // The prompt history arrived in 3.3, and the crate refuses it below that
    // rather than dispatching something tmux will reject. Both answers are
    // asserted, because a skip would leave the refusal untested on exactly
    // the releases that produce it.
    let cleared = server.clear_prompt_history().await;
    if server
        .capabilities()
        .await
        .expect("capabilities")
        .tmux_version()
        .meets(&since::PROMPT_HISTORY)
    {
        cleared.expect("the prompt history is cleared");
    } else {
        assert!(
            matches!(
                cleared.as_ref().map_err(libtmux::Error::kind),
                Err(libtmux::ErrorKind::UnsupportedVersion),
            ),
            "an older tmux reports the version rather than failing some other way: {cleared:?}",
        );
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Unlinking a link that is already gone must not say the window is gone.
///
/// A link-scoped command names both halves, `session:@id`, because a window
/// linked into several sessions has one id and several links and the id alone
/// could not say which link to drop. tmux answers a target it cannot resolve
/// by echoing it back, and it echoes an identity identically whether the
/// window died or is merely linked somewhere else -- so the answer alone
/// cannot separate them, and reading it as proof of death claims "closed or
/// killed" about a window still running under another link.
///
/// That is what costs a caller something: `is_object_gone` is what they branch
/// on to drop a handle.
#[tokio::test]
async fn unlinking_a_gone_link_does_not_call_a_live_window_gone() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let home = server.new_session("home").await.expect("session");
    let elsewhere = server.new_session("elsewhere").await.expect("session");

    let shared = home
        .new_window(NewWindowOptions::new("shared"))
        .await
        .expect("a window to share");
    let shared_id = shared.id().to_string();
    let index = shared.index();

    shared
        .link_to(&elsewhere, None)
        .await
        .expect("the window is linked into a second session");

    // Two handles to the same link, taken before either is used: `unlink`
    // consumes the handle, and after the first call this session holds no
    // window at that index to take a second from.
    let mut handles = Vec::new();
    for _ in 0..2 {
        handles.push(
            home.windows()
                .await
                .expect("windows")
                .into_iter()
                .find(|window| window.index() == index)
                .expect("the shared window is linked here"),
        );
    }
    let second = handles.pop().expect("two handles");
    let first = handles.pop().expect("two handles");

    // The first removal succeeds: two links, one goes.
    first.unlink().await.expect("the link is removed");

    // The window itself is still running, held by the other session.
    let alive = server
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .any(|window| window.id().to_string() == shared_id);
    assert!(alive, "the window survives losing one of its links");

    // Removing the same link again fails, and what it says matters.
    let again = home
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .find(|window| window.index() == index);
    assert!(
        again.is_none(),
        "the session no longer holds a window at that index"
    );

    let refused = second.unlink().await.expect_err("the link is already gone");

    assert!(
        !refused.is_object_gone(),
        "a missing link is not a missing object; the window is still alive. got {refused:?}"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A `TMUX_PANE` that is set and wrong is not the same as one that is unset.
///
/// Both used to answer "not inside tmux", which sends a caller who *is* inside
/// tmux to check the wrong thing entirely. `Server::from_env_value` already
/// drew this line for `TMUX`, ten lines away, with a comment making the case.
#[tokio::test]
async fn a_malformed_pane_variable_is_not_being_outside_tmux() {
    use libtmux::ServerConfigurationErrorKind as Kind;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    server.new_session("env").await.expect("session");

    let absent = libtmux::Pane::from_env_value(server, None::<std::ffi::OsString>)
        .await
        .expect_err("nothing set the variable");
    assert!(
        matches!(
            &absent,
            libtmux::Error::InvalidServerConfiguration {
                kind: Kind::NotInsideTmux,
                ..
            }
        ),
        "an unset variable means this process was not started by tmux, got {absent:?}"
    );

    for wrong in ["%abc", "@0", "%-1", "#{pane_id}", ""] {
        let malformed = libtmux::Pane::from_env_value(server, Some(wrong))
            .await
            .expect_err("the variable is set and does not name a pane");
        assert!(
            matches!(
                &malformed,
                libtmux::Error::InvalidServerConfiguration {
                    kind: Kind::MalformedTmuxVariable,
                    ..
                }
            ),
            "{wrong:?} is a variable written wrongly, not an absent one, got {malformed:?}"
        );
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A chooser opens in a pane, so it needs no client; a popup needs one.
///
/// Both kinds are dispatched from `Server` and both take `Option<&Client>`,
/// which makes them look like one family that disagrees about clients. They
/// are two families: `choose` and `find_window` put a *pane* into a mode, and
/// `display_popup` and its three siblings draw *on a client* and cannot
/// without one.
///
/// Asserting the return value alone cannot tell them apart -- that was the
/// mistake this test replaces. `Ok` from `choose` was read as an empty promise
/// until somebody looked at the pane, which is in a mode.
#[tokio::test]
async fn a_chooser_opens_in_a_pane_and_a_popup_needs_a_client() {
    use libtmux::Chooser;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("nobody").await.expect("session");
    assert!(
        server.clients().await.expect("clients").is_empty(),
        "nothing is attached"
    );

    let mut pane = session.panes().await.expect("panes").remove(0);
    assert!(!pane.is_in_mode(), "the pane starts in no mode");

    server
        .choose(Chooser::Tree, None)
        .await
        .expect("a chooser needs no client");
    pane.refresh().await.expect("the pane still exists");
    assert!(
        pane.is_in_mode(),
        "the chooser is open in the pane, which is what the Ok meant"
    );

    let refused = server
        .display_popup(None, "true")
        .await
        .expect_err("a popup draws on a client and there is none");
    assert_eq!(refused.kind(), libtmux::ErrorKind::Refused);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// One dead object must answer the same way whichever call asks.
///
/// tmux has two vocabularies for it. `cmd-find.c` resolves a target and says
/// "can't find pane"; `options.c` resolves its own and says "no such pane".
/// Matching only the first meant `is_object_gone` answered `true` from
/// `capture` and `false` from `get_option` about the same dead pane.
///
/// The `@` branch made it worse than inconsistent. A user option that is not
/// set is unknown to tmux, so that failure is the answer `None` -- but the
/// branch swallowed *every* failure for an `@` name, so a pane that had gone
/// away read as a pane whose option was never set.
///
/// The option is set before the pane is killed. Without that, `Ok(None)` is
/// indistinguishable from a legitimate "never set" and the test proves
/// nothing.
#[tokio::test]
async fn a_dead_pane_says_so_however_the_question_is_asked() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("gone").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a session has a window");
    let doomed = window
        .panes()
        .await
        .expect("panes")
        .remove(0)
        .split(SplitOptions::new(SplitDirection::Below))
        .await
        .expect("a second pane to kill");

    // The positive control: the option is really set, on a pane that is really
    // there, so a later `None` cannot mean "never set".
    doomed
        .set_option("@marker", "here")
        .await
        .expect("the user option is set");
    assert_eq!(
        doomed
            .get_option("@marker")
            .await
            .expect("readable")
            .expect("set")
            .as_str()
            .expect("text"),
        "here"
    );

    // `kill` consumes the handle, so the questions afterwards are asked
    // through a second one taken before it.
    let asking = window
        .panes()
        .await
        .expect("panes")
        .into_iter()
        .find(|pane| pane.id() == doomed.id())
        .expect("the doomed pane is listed");
    doomed.kill().await.expect("the pane is killed");
    let doomed = asking;

    // Every route to the same fact agrees.
    let by_capture = doomed.capture().await.expect_err("the pane is gone");
    assert!(
        by_capture.is_object_gone(),
        "capture says so: {by_capture:?}"
    );

    let by_option = doomed
        .get_option("remain-on-exit")
        .await
        .expect_err("the pane is gone");
    assert!(
        by_option.is_object_gone(),
        "a built-in option says so too: {by_option:?}"
    );

    let by_user_option = doomed
        .get_option("@marker")
        .await
        .expect_err("the pane is gone, not the option unset");
    assert!(
        by_user_option.is_object_gone(),
        "a user option must not report a gone pane as an unset option: {by_user_option:?}"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A variable nobody set and a session that has ended are different answers.
///
/// `environment` returned `Ok(None)` for any refusal, so a dead session read as
/// a variable that was never there -- and the three other environment methods
/// on the same handle reported the death correctly, so one struct disagreed
/// with itself.
///
/// tmux says which it is, on the same exit code: "unknown variable: NAME" for
/// the first, "no such session: NAME" for the second.
///
/// The variable is set before the session is killed, so `Ok(None)` afterwards
/// cannot be read as a legitimate "never set".
#[tokio::test]
async fn a_dead_session_is_not_an_unset_variable() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    // A second session keeps the server alive once the first is killed.
    server.new_session("keeper").await.expect("session");
    let doomed = server.new_session("doomed").await.expect("session");

    doomed
        .set_environment("FOO", "bar")
        .await
        .expect("the variable is set");
    assert!(
        doomed.environment("FOO").await.expect("readable").is_some(),
        "the positive control: it really is set"
    );
    assert!(
        doomed
            .environment("NEVER_SET")
            .await
            .expect("readable")
            .is_none(),
        "a name tmux does not hold is still None"
    );

    let asking = server
        .sessions()
        .await
        .expect("sessions")
        .into_iter()
        .find(|session| session.id() == doomed.id())
        .expect("the doomed session is listed");
    doomed.kill().await.expect("the session is killed");

    let refused = asking
        .environment("FOO")
        .await
        .expect_err("the session is gone, not the variable unset");
    assert!(
        refused.is_object_gone(),
        "a gone session must not read as an unset variable: {refused:?}"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn interactive_commands_need_a_client() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    server.new_session("interactive").await.expect("session");

    // A headless server has no terminal, so commands that draw on one fail
    // loudly rather than reporting a success nobody could see.
    assert!(server.display_panes(None).await.is_err());
    assert!(
        server
            .display_menu(
                None,
                "menu",
                [("Item".into(), "i".into(), "kill-pane".into())]
            )
            .await
            .is_err(),
    );
    assert!(
        server
            .command_prompt(None, Some("question"), "kill-pane")
            .await
            .is_err(),
    );

    // The choosers are the exception: tmux accepts one with no client and
    // silently does nothing. This records what tmux does rather than what
    // symmetry would suggest.
    server
        .choose(libtmux::Chooser::Tree, None)
        .await
        .expect("tmux accepts a chooser with no client");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn retry_until_waits_for_tmux_rather_than_sleeping() {
    use libtmux::test::{retry_until, unique_name};
    use std::time::Duration;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // A unique name lets concurrent tests share one server without colliding.
    let name = unique_name("retry");
    let session = server.new_session(name.as_str()).await.expect("session");
    assert_ne!(unique_name("retry"), name);

    // tmux applies a split before the new pane's command has necessarily
    // execed, so wait for the state under test.
    session
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .next()
        .expect("one window")
        .split(SplitOptions::new(SplitDirection::Below).command("sleep 300"))
        .await
        .expect("pane is created");

    retry_until(Duration::from_secs(5), async || {
        session.panes().await.is_ok_and(|panes| panes.len() == 2)
    })
    .await
    .expect("the second pane appears");

    // A condition that never holds reports the deadline instead of hanging.
    let waited = retry_until(Duration::from_millis(50), async || false)
        .await
        .expect_err("a condition that never holds times out");
    assert_eq!(waited.waited(), Duration::from_millis(50));

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_scoped_raw_command_targets_its_own_object() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let first = server.new_session("first").await.expect("first session");
    let second = server.new_session("second").await.expect("second session");

    // Each handle addresses itself, not whatever tmux considers current.
    for (session, expected) in [(&first, "first"), (&second, "second")] {
        let named = session
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg("#{session_name}"),
            )
            .await
            .expect("the command runs");
        assert_eq!(named.stdout_lossy().trim(), expected);
    }

    // The placement is the point. tmux stops reading flags at the first
    // positional, so a target appended after `--` is taken as text: the
    // command succeeds and types it into whichever pane was current. Sending
    // through the handle puts `-t` where tmux still reads it.
    let window = second
        .active_window()
        .await
        .expect("active window")
        .expect("a session has a window");
    let pane = window
        .active_pane()
        .await
        .expect("active pane")
        .expect("a window has a pane");

    pane.cmd(
        Command::new("send-keys")
            .arg("--")
            .arg("echo scoped")
            .arg("Enter"),
    )
    .await
    .expect("keys are sent");

    let seen = retry_until(Duration::from_secs(5), async || {
        pane.capture().await.is_ok_and(|lines| {
            lines
                .iter()
                .any(|line| String::from_utf8_lossy(line.as_bytes()).contains("scoped"))
        })
    })
    .await;
    assert!(seen.is_ok(), "the keys reached this pane, not another");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn formatting_returns_text_and_displaying_returns_nothing() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("split").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("active window")
        .expect("a session has a window");

    // The reading half expands in the object's own context, so each handle
    // answers about itself rather than about whatever tmux considers current.
    assert_eq!(
        session
            .format("#{session_name}")
            .await
            .expect("format")
            .to_string_lossy(),
        "split",
    );
    assert_eq!(
        window
            .format("#{window_id}")
            .await
            .expect("format")
            .to_string_lossy(),
        window.id().to_string(),
    );

    // A format that expands to nothing is empty text rather than an error:
    // tmux expanded it fine, the answer is just empty.
    assert!(
        session
            .format("#{?0,yes,}")
            .await
            .expect("format")
            .as_bytes()
            .is_empty(),
    );

    // The showing half returns nothing, and succeeds with nobody attached: a
    // headless server shows the message to no one, which is not a failure.
    let pane = window
        .active_pane()
        .await
        .expect("active pane")
        .expect("a window has a pane");
    assert_eq!(
        pane.format("#{pane_id}")
            .await
            .expect("format")
            .to_string_lossy(),
        pane.id().to_string(),
    );

    session.display("hello").await.expect("display");
    window.display("hello").await.expect("display");
    pane.display("hello").await.expect("display");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_capability_too_new_for_this_tmux_is_refused_rather_than_ignored() {
    // Follows the compat suite's convention: `LIBTMUX_TEST_TMUX` points this
    // at one release of the version matrix, so the same assertion runs across
    // every supported tmux rather than only whichever is on PATH.
    let mut builder = TestServer::builder();
    if let Some(executable) = std::env::var_os("LIBTMUX_TEST_TMUX") {
        builder = builder.tmux_executable(executable);
    }
    let guard = builder.start().await.expect("tmux starts");
    let server = guard.server();

    let version = server
        .capabilities()
        .await
        .expect("capabilities")
        .tmux_version()
        .clone();
    let supported = version.release().is_none_or(|release| {
        *release >= libtmux::ReleaseVersion::new(3, 3, libtmux::ReleaseSuffix::FINAL)
    });

    let answer = server.prompt_history(libtmux::PromptKind::Command).await;
    if supported {
        // Nothing has answered a prompt on a fresh server, so the history is
        // empty rather than absent.
        assert!(answer.expect("prompt history reads").is_empty());
    } else {
        // tmux below 3.3 has no such command. Left to tmux this would surface
        // as an unknown-command failure or, for a flag, as silence: the
        // command succeeding having ignored what was asked for.
        let error = answer.expect_err("an older tmux refuses the capability");
        assert_eq!(error.kind(), libtmux::ErrorKind::UnsupportedVersion);
        assert!(
            error.to_string().contains("3.3"),
            "the refusal says what it needs: {error}",
        );
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn the_server_access_list_names_its_owner_and_refuses_to_unseat_them() {
    let mut builder = TestServer::builder();
    if let Some(executable) = std::env::var_os("LIBTMUX_TEST_TMUX") {
        builder = builder.tmux_executable(executable);
    }
    let guard = builder.start().await.expect("tmux starts");
    let server = guard.server();

    let supported = server
        .capabilities()
        .await
        .expect("capabilities")
        .tmux_version()
        .release()
        .is_none_or(|release| {
            *release >= libtmux::ReleaseVersion::new(3, 3, libtmux::ReleaseSuffix::FINAL)
        });
    if !supported {
        assert_eq!(
            server
                .access_rules()
                .await
                .expect_err("an older tmux has no access list")
                .kind(),
            libtmux::ErrorKind::UnsupportedVersion,
        );
        guard.shutdown().await.expect("tmux fixture shuts down");
        return;
    }

    // Whoever started the server owns it, and an owner may always act.
    let rules = server.access_rules().await.expect("access list");
    let owner = rules.first().expect("the owner is listed");
    assert_eq!(rules.len(), 1);
    assert_eq!(owner.mode(), libtmux::AccessMode::Write);
    assert!(!owner.user().is_empty());

    // tmux refuses to change the owner's own entry, so a caller cannot lock
    // itself out of the server it just started.
    let user = owner.user().to_owned();
    for attempt in [
        server
            .grant_access(&user, libtmux::AccessMode::ReadOnly)
            .await,
        server.revoke_access(&user).await,
    ] {
        let error = attempt.expect_err("tmux refuses to change the owner");
        assert!(
            error.to_string().contains("owns the server"),
            "the refusal says why: {error}",
        );
    }

    // And the list is unchanged by the refusals.
    assert_eq!(server.access_rules().await.expect("access list"), rules);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Prompt marks come from the shell, so a shell that emits none is the case
/// most callers meet: bash and zsh do not without shell integration, fish
/// does. An unmarked capture is an answer about the shell, not a failure.
#[tokio::test]
async fn real_tmux_compat_capture_line_flags_mark_prompts_when_the_shell_emits_them() {
    use libtmux::{CaptureOptions, since};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("prompts").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);

    let supported = server
        .capabilities()
        .await
        .expect("capabilities")
        .tmux_version()
        .meets(&since::CAPTURE_LINE_FLAGS);

    if !supported {
        // Below 3.7 tmux accepts no `-F`, and saying so beats an empty answer.
        let refused = pane.capture_lines(CaptureOptions::visible()).await;
        assert!(
            matches!(
                refused.as_ref().map_err(libtmux::Error::kind),
                Err(libtmux::ErrorKind::UnsupportedVersion),
            ),
            "an older tmux reports the version rather than returning nothing: {refused:?}",
        );
        guard.shutdown().await.expect("tmux fixture shuts down");
        return;
    }

    // Standing in for shell integration: the sequences a shell would emit
    // around its prompt and before its output.
    pane.send_keys(
        r"printf '\033]133;A\007'; echo THE-PROMPT; printf '\033]133;C\007'; echo the-output",
    )
    .await
    .expect("keys are sent");
    pane.send_key_names(["Enter"]).await.expect("Enter is sent");
    tokio::time::sleep(Duration::from_millis(600)).await;

    let lines = pane
        .capture_lines(CaptureOptions::history())
        .await
        .expect("the capture succeeds");

    let prompt = lines
        .iter()
        .position(|line| line.starts_prompt)
        .expect("the prompt mark is reported");
    let output = lines
        .iter()
        .position(|line| line.starts_output)
        .expect("the output mark is reported");

    assert!(prompt < output, "the prompt precedes its output");
    assert_eq!(
        lines[prompt].text.to_string_lossy().trim(),
        "THE-PROMPT",
        "the mark lands on the line the shell was drawing",
    );
    assert_eq!(lines[output].text.to_string_lossy().trim(), "the-output");

    // The flags are stripped rather than left at the front of the text.
    assert!(
        lines
            .iter()
            .all(|line| !line.text.to_string_lossy().starts_with("- ")),
        "no line keeps tmux's flag column in its text",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A client stopped in place is not a client that went away.
///
/// tmux leaves a suspended client out of `list-clients` -- the listing filters
/// the dead, the exiting and the suspended together -- while still resolving
/// it as a command target. Reading absence from that listing as `ObjectGone`
/// told a caller to discard a handle that works again the moment the client
/// resumes, which is the opposite of what `is_object_gone` is consulted for.
///
/// The client is spawned here rather than taken from `ControlMode`, because
/// suspending a client stops its process and a crate-held connection would
/// then be one this test cannot take down. A child it owns can be killed
/// outright, stopped or not.
#[tokio::test]
async fn a_suspended_client_is_not_reported_gone() {
    use libtmux::since;
    use std::process;

    /// SIGKILL rather than closing stdin: a stopped process never reads its
    /// stdin to notice EOF, so the polite shutdown is the one that hangs.
    struct KillOnDrop(process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            drop(self.0.kill());
            drop(self.0.wait());
        }
    }

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("suspendable").await.expect("session");

    let hides_stopped = server
        .capabilities()
        .await
        .expect("capabilities")
        .tmux_version()
        .meets(&since::CLIENTS_HIDE_STOPPED);

    let child = process::Command::new("tmux")
        .arg("-S")
        .arg(guard.socket_path())
        .arg("-C")
        .arg("attach")
        .arg("-t")
        .arg("suspendable")
        .stdin(process::Stdio::piped())
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .spawn()
        .expect("a control-mode client attaches");
    let _client_process = KillOnDrop(child);

    retry_until(Duration::from_secs(10), async || {
        server
            .clients()
            .await
            .is_ok_and(|clients| !clients.is_empty())
    })
    .await
    .expect("the attached client is listed");

    let client = server.clients().await.expect("clients").remove(0);
    client.suspend().await.expect("the client suspends");

    if !hides_stopped {
        // Before 3.7 the listing screens on the session alone, and suspending
        // never clears that, so the client stays listed and reads back with
        // no miss path to take. Asserted rather than skipped: the fix must
        // not have invented an error on a release that has no problem.
        let same = client
            .refreshed()
            .await
            .expect("the client is still listed");
        assert_eq!(same.name(), client.name());
        assert!(
            !server.clients().await.expect("clients").is_empty(),
            "an older tmux lists a suspended client",
        );
        guard.shutdown().await.expect("tmux fixture shuts down");
        return;
    }

    // Leaving the listing is the condition that used to read as gone, so the
    // test asserts it happened rather than assuming the command took effect.
    retry_until(Duration::from_secs(10), async || {
        server
            .clients()
            .await
            .is_ok_and(|clients| clients.is_empty())
    })
    .await
    .expect("the suspended client leaves the listing");

    let Err(error) = client.refreshed().await else {
        panic!("the suspended client is not in the listing to be found")
    };
    assert!(
        matches!(error, libtmux::Error::ClientSuspended { .. }),
        "a suspended client resolves and says so: {error:?}",
    );
    assert!(
        !error.is_object_gone(),
        "this is what a caller consults before discarding the handle",
    );
    assert!(
        error.is_transient(),
        "the same client handle works once the process resumes",
    );

    // And the reason the misreading was tempting: every count a caller might
    // consult goes quiet at once. The client still carries tmux's `attached`
    // flag, but the session stops reporting it, so nothing in the listing
    // surface distinguishes this from a client that left.
    assert!(
        !session.refreshed().await.expect("session").is_attached(),
        "a suspended client stops counting toward its session",
    );
}

/// A capture can now reach what only `Pane::cmd` could before.
///
/// `capture-pane` trims trailing spaces unless given `-N`, and no typed path
/// passed it, so every capture normalised away whatever a program printed at
/// a line's end. A caller asserting on exact pane content had to drop to the
/// untyped API and read bytes.
#[tokio::test]
async fn a_capture_can_keep_the_spaces_tmux_would_trim() {
    use libtmux::CaptureOptions;

    fn printed(lines: Vec<libtmux::TmuxText>) -> String {
        lines
            .into_iter()
            .map(|line| line.to_string_lossy().into_owned())
            .find(|line| line.starts_with("AB"))
            .expect("the printed line is captured")
    }

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("trailing").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);

    // Three spaces a program printed, which is what tmux strips.
    pane.send_keys("printf 'AB   \\n'")
        .await
        .expect("keys are sent");
    pane.send_key_names(["Enter"]).await.expect("the line runs");

    retry_until(Duration::from_secs(10), async || {
        pane.capture()
            .await
            .is_ok_and(|lines| lines.iter().any(|l| l.to_string_lossy().starts_with("AB")))
    })
    .await
    .expect("the pane prints");

    let trimmed = printed(pane.capture().await.expect("default capture"));
    let exact = printed(
        pane.capture_with(CaptureOptions::visible().trailing_spaces())
            .await
            .expect("capture keeping trailing spaces"),
    );

    assert_eq!(trimmed, "AB", "tmux trims trailing spaces without `-N`");
    assert!(
        exact.starts_with("AB   "),
        "`-N` keeps what the program printed: {exact:?}",
    );
    assert!(
        exact.len() > trimmed.len(),
        "the exact capture is longer: {exact:?} vs {trimmed:?}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// `capture-pane -T` arrived in 3.4, and the crate refuses it below that.
///
/// Asserted on both sides rather than skipped: a capability check that is
/// never exercised on the releases producing the refusal is a branch nothing
/// runs. `just compat` puts 3.2a below the boundary and four releases above.
#[tokio::test]
async fn trimming_blank_cells_is_refused_below_the_release_that_has_it() {
    use libtmux::{CaptureOptions, since};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("blankcells").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);

    let supported = server
        .capabilities()
        .await
        .expect("capabilities")
        .tmux_version()
        .meets(&since::CAPTURE_TRIM_BLANK_CELLS);

    let asked = pane
        .capture_with(CaptureOptions::visible().trim_blank_cells())
        .await;

    if supported {
        asked.expect("3.4 and later accept -T");
    } else {
        assert!(
            matches!(
                asked.as_ref().map_err(libtmux::Error::kind),
                Err(libtmux::ErrorKind::UnsupportedVersion),
            ),
            "an older tmux reports the version rather than dispatching a flag \
             it will reject: {asked:?}",
        );
    }

    // `-N` and `-P` are present at the supported floor, so neither is gated
    // and both must work on every release the lanes build.
    pane.capture_with(CaptureOptions::visible().trailing_spaces())
        .await
        .expect("-N works on every supported release");
    pane.capture_with(CaptureOptions::visible().pending_escape())
        .await
        .expect("-P works on every supported release");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Waiting answers without a feature, and survives the two shapes that made a
/// rebuilt version report success while losing output.
///
/// A screen read misses text that scrolled away before the look, and splits a
/// line wider than the pane into several. Both report success with the wrong
/// content, which is the direction that costs a caller most, so both are
/// asserted rather than assumed.
#[tokio::test]
async fn waiting_survives_scrollback_and_a_line_wider_than_the_pane() {
    use libtmux::PaneWait;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("waits").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);

    // Wider than any sane pane, so tmux wraps it. A screen read would see
    // several lines and never match a needle spanning the wrap.
    let wide = "W".repeat(400);
    pane.send_keys(format!("printf '{wide}\\n'"))
        .await
        .expect("keys are sent");
    pane.send_key_names(["Enter"]).await.expect("it runs");

    assert_eq!(
        pane.wait_for_text(&wide, Duration::from_secs(10))
            .await
            .expect("waiting is not an error"),
        PaneWait::Arrived,
        "a line wider than the pane is matched across its wrap",
    );

    // Push the wide line off the visible screen. A screen read would now
    // report it absent; the scrollback still holds it.
    for _ in 0..60 {
        pane.send_keys("printf 'filler\\n'").await.expect("keys");
        pane.send_key_names(["Enter"]).await.expect("it runs");
    }
    assert_eq!(
        pane.wait_for_text(&wide, Duration::from_secs(10))
            .await
            .expect("waiting is not an error"),
        PaneWait::Arrived,
        "text that scrolled off the screen is still found",
    );

    // Running out of time is an outcome, not an error.
    assert_eq!(
        pane.wait_for_text("nothing prints this", Duration::from_millis(400))
            .await
            .expect("a deadline is not an error"),
        PaneWait::TimedOut,
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A pane whose process ended stops the wait instead of holding the deadline.
#[tokio::test]
async fn waiting_ends_when_the_pane_dies_rather_than_at_the_deadline() {
    use libtmux::{NewWindowOptions, PaneWait};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("dying").await.expect("session");

    // `remain-on-exit` keeps the pane after its command ends, so the wait has
    // a dead pane to observe rather than a missing one.
    server
        .cmd(
            Command::new("set-option")
                .arg("-g")
                .arg("remain-on-exit")
                .arg("on"),
        )
        .await
        .expect("panes remain after exit");
    let window = session
        .new_window(NewWindowOptions::new("shortlived").command("true"))
        .await
        .expect("window");
    let pane = window.panes().await.expect("panes").remove(0);

    let started = std::time::Instant::now();
    let outcome = pane
        .wait_for_text("never printed", Duration::from_secs(20))
        .await
        .expect("waiting is not an error");
    let waited = started.elapsed();

    assert_eq!(outcome, PaneWait::Dead);
    assert!(
        waited < Duration::from_secs(15),
        "a dead pane ends the wait early rather than at the deadline: {waited:?}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Breaking out the only pane relinks its window instead of doing nothing.
///
/// tmux takes the command, and `cmd-break-pane.c` links the window at a free
/// index rather than refusing. The documentation said it did nothing, which
/// left a caller holding a window whose index had silently moved.
#[tokio::test]
async fn breaking_out_the_only_pane_moves_its_window() {
    use libtmux::NewWindowOptions;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("lonely").await.expect("session");
    let window = session
        .new_window(NewWindowOptions::new("lone"))
        .await
        .expect("window");
    let pane = window.panes().await.expect("panes").remove(0);

    assert_eq!(window.panes().await.expect("panes").len(), 1, "one pane");
    let before = window.index();
    let window_id = window.id().clone();

    pane.break_out().await.expect("tmux accepts the command");

    let after = window
        .refreshed()
        .await
        .expect("the window is still there, by id");
    assert_eq!(after.id(), &window_id, "the window keeps its identity");
    assert_ne!(
        after.index(),
        before,
        "the window moved to a free index rather than nothing happening",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A handle whose index moved must not act on whatever took the slot.
///
/// Breaking a lone pane out of a window relinks that window at a free index,
/// which renumbers nothing else but leaves every handle above it holding an
/// index that now belongs to a different live window. tmux answers such a
/// target without complaint, so an index-scoped command reports success on the
/// wrong object.
#[tokio::test]
async fn a_window_acts_on_itself_after_its_index_moved() {
    use libtmux::NewWindowOptions;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("renumber").await.expect("session");

    let mut moved = session
        .new_window(NewWindowOptions::new("moved"))
        .await
        .expect("window");
    let moved_id = moved.id().clone();
    let cached = moved.index();

    // Breaking out its only pane relinks the window at a free index.
    let pane = moved.panes().await.expect("panes").remove(0);
    pane.break_out().await.expect("tmux accepts the command");

    let after = moved.refreshed().await.expect("the window still exists");
    assert_ne!(after.index(), cached, "the window moved");

    // Put a window in the slot the handle still remembers. This is what
    // separates dangerous from merely stale: an empty slot refuses, and an
    // occupied one answers about somebody else.
    let intruder = session
        .new_window(NewWindowOptions::new("intruder").index(cached))
        .await
        .expect("a window takes the vacated slot");
    let intruder_id = intruder.id().clone();
    assert_eq!(intruder.index(), cached, "the slot is occupied again");
    assert_ne!(intruder_id, moved_id, "by a different window");

    // Selecting through the stale handle must reach the handle's own window.
    moved.select().await.expect("select succeeds");
    let active = session
        .active_window()
        .await
        .expect("active window")
        .expect("a session always has one");
    assert_eq!(
        active.id(),
        &moved_id,
        "select acted on the handle's window, not on whatever took its index",
    );
    assert_ne!(
        active.id(),
        &intruder_id,
        "and not on the window occupying its old index",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_name_outside_ascii_survives_the_listing() {
    // tmux replaces every byte outside `0x20..=0x7e` with `_` unless it
    // believes its client speaks UTF-8, and it decides that from the client
    // process's own `TMUX` and locale variables. This crate unsets `TMUX` and
    // inherits whatever locale its caller had, so without saying so directly
    // this name comes back as six underscores and no error.
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let session = server.new_session("日本語").await.expect("session");
    assert_eq!(session.name().as_bytes(), "日本語".as_bytes());

    let listed = server.sessions().await.expect("sessions");
    assert_eq!(listed[0].name().as_bytes(), "日本語".as_bytes());

    guard.shutdown().await.expect("tmux fixture shuts down");
}
