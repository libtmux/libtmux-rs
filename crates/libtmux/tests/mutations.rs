//! Integration tests for hierarchy mutations against real tmux.

#![cfg(feature = "test-support")]
// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use libtmux::test::TestServer;
use libtmux::{NewSessionOptions, NewWindowOptions, SplitDirection, SplitOptions, TmuxText};

fn text(value: Option<&TmuxText>) -> Vec<u8> {
    value.expect("tmux reports the value").as_bytes().to_vec()
}

#[tokio::test]
async fn creating_an_object_returns_it_hydrated_in_one_command() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // A bare name is enough for the common case.
    let session = server
        .new_session("work")
        .await
        .expect("session is created");
    assert_eq!(session.name().as_bytes().to_vec(), b"work".to_vec());
    assert_eq!(session.window_count(), 1);
    // The handle came back populated, so no follow-up listing was needed.
    assert!(session.created() > 0);

    let window = session
        .new_window(NewWindowOptions::new("editor").command("sleep 300"))
        .await
        .expect("window is created");
    assert_eq!(window.name().as_bytes().to_vec(), b"editor".to_vec());
    assert_eq!(window.session_id(), session.id());
    assert_eq!(window.pane_count(), 1);

    let pane = window
        .split(SplitOptions::new(SplitDirection::Below).command("sleep 300"))
        .await
        .expect("pane is created");
    assert_eq!(pane.window_id(), window.id());
    assert!(pane.pid() > 0);

    // The server agrees with what the creating commands reported.
    assert_eq!(server.try_sessions().await.expect("sessions").len(), 1);
    assert_eq!(server.try_windows().await.expect("windows").len(), 2);
    assert_eq!(server.try_panes().await.expect("panes").len(), 3);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn creation_options_reach_tmux() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let directory = tempfile::tempdir().expect("temporary directory");

    let session = server
        .new_session(
            NewSessionOptions::new("configured")
                .window_name("first")
                .start_directory(directory.path())
                .size(120, 40)
                .command("sleep 300"),
        )
        .await
        .expect("session is created");

    let windows = session.try_windows().await.expect("windows list");
    assert_eq!(windows[0].name().as_bytes().to_vec(), b"first".to_vec());

    // tmux records the requested size as the session's `default-size`, which
    // is what this option controls and is the same on every supported
    // release. Whether a window with no client attached is then drawn at that
    // size is tmux's own business and is not: 3.2a leaves it at 80x23 where
    // 3.6 uses the default. Asserting the rendered size would be asserting
    // tmux's behaviour rather than the crate's.
    assert_eq!(
        session
            .get_option("default-size")
            .await
            .expect("read")
            .expect("new-session -x -y sets it")
            .as_bytes(),
        b"120x40",
    );

    let panes = session.try_panes().await.expect("panes list");
    assert_eq!(
        text(panes[0].current_path()),
        directory
            .path()
            .canonicalize()
            .expect("canonical path")
            .as_os_str()
            .as_encoded_bytes(),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_new_window_does_not_steal_selection_unless_asked() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let session = server
        .new_session("work")
        .await
        .expect("session is created");
    let first = session
        .active_window()
        .await
        .expect("active window resolves")
        .expect("a session always has an active window");

    session
        .new_window(NewWindowOptions::new("background").command("sleep 300"))
        .await
        .expect("window is created");

    let still_first = session
        .active_window()
        .await
        .expect("active window resolves")
        .expect("a session always has an active window");
    assert_eq!(still_first.id(), first.id(), "creation does not select");

    let selected = session
        .new_window(
            NewWindowOptions::new("foreground")
                .command("sleep 300")
                .select(),
        )
        .await
        .expect("window is created");
    let active = session
        .active_window()
        .await
        .expect("active window resolves")
        .expect("a session always has an active window");
    assert_eq!(active.id(), selected.id(), "select() makes it active");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn renaming_updates_the_handle_that_performed_it() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let mut session = server
        .new_session("before")
        .await
        .expect("session is created");
    session.rename("after").await.expect("rename succeeds");
    assert_eq!(session.name().as_bytes().to_vec(), b"after".to_vec());

    let mut window = session
        .new_window(NewWindowOptions::new("old").command("sleep 300"))
        .await
        .expect("window is created");
    window.rename("new").await.expect("rename succeeds");
    assert_eq!(window.name().as_bytes().to_vec(), b"new".to_vec());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn killing_removes_the_object_and_strands_other_handles() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let session = server
        .new_session("doomed")
        .await
        .expect("session is created");
    server
        .new_session("survivor")
        .await
        .expect("session is created");
    let mut stale = session.clone();

    session.kill().await.expect("kill succeeds");

    assert_eq!(server.try_sessions().await.expect("sessions").len(), 1);
    let error = stale.refresh().await.expect_err("a killed session is gone");
    assert!(matches!(
        error,
        libtmux::Error::ObjectGone {
            kind: libtmux::ObjectKind::Session,
            ..
        },
    ));

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn unlink_removes_one_link_while_kill_removes_the_window() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let origin = server
        .new_session("origin")
        .await
        .expect("session is created");
    server
        .new_session("borrower")
        .await
        .expect("session is created");
    let shared = origin
        .new_window(NewWindowOptions::new("shared").command("sleep 300"))
        .await
        .expect("window is created");

    server
        .cmd(
            libtmux::Command::new("link-window")
                .arg("-s")
                .arg(shared.id().to_string())
                .arg("-t")
                .arg("borrower:9"),
        )
        .await
        .expect("link succeeds");

    let links: Vec<_> = server
        .try_windows()
        .await
        .expect("windows list")
        .into_iter()
        .filter(|window| window.id() == shared.id())
        .collect();
    assert_eq!(links.len(), 2);

    // Unlinking removes one edge; the window survives in the other session.
    links[0].clone().unlink().await.expect("unlink succeeds");
    let remaining: Vec<_> = server
        .try_windows()
        .await
        .expect("windows list")
        .into_iter()
        .filter(|window| window.id() == shared.id())
        .collect();
    assert_eq!(
        remaining.len(),
        1,
        "the window survives in its other session"
    );

    // Killing removes the window itself.
    remaining[0].clone().kill().await.expect("kill succeeds");
    assert!(
        server
            .try_windows()
            .await
            .expect("windows list")
            .iter()
            .all(|window| window.id() != shared.id()),
        "the window is gone everywhere",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn pane_input_and_capture_round_trip_through_a_shell() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let session = server
        .new_session(NewSessionOptions::new("io").command("sh"))
        .await
        .expect("session is created");
    let mut pane = session
        .try_panes()
        .await
        .expect("panes list")
        .into_iter()
        .next()
        .expect("one pane");

    pane.send_keys("printf marker-8fa1\n")
        .await
        .expect("keys are sent");

    // Wait for the shell to produce the output rather than sleeping.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let captured = loop {
        let lines = pane.capture().await.expect("capture succeeds");
        if lines.iter().any(|line| {
            line.as_bytes()
                .windows(11)
                .any(|window| window == b"marker-8fa1")
        }) {
            break lines;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the shell did not echo the marker before the deadline",
        );
        tokio::task::yield_now().await;
    };
    assert!(!captured.is_empty());

    // A sole pane fills its window, so there is nothing for a resize to take
    // space from. Split first, then narrow the original.
    let window = pane
        .window()
        .await
        .expect("parent resolves")
        .expect("the pane's window exists");
    window
        .split(SplitOptions::new(SplitDirection::Right).command("sleep 300"))
        .await
        .expect("pane is created");

    pane.refresh().await.expect("the pane still exists");
    let full_width = pane.width();
    pane.resize(20, 8).await.expect("resize succeeds");

    // The handle reports what tmux did, not what was requested.
    assert_eq!(pane.width(), 20);
    assert!(full_width > 20, "the split left room to shrink into");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn killing_the_server_leaves_nothing_running() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server().clone();

    server
        .new_session("work")
        .await
        .expect("session is created");
    assert!(server.is_alive().await);

    server.kill().await.expect("kill-server succeeds");
    assert!(!server.is_alive().await);
    // Killing an already-dead server is the state the caller asked for.
    server.kill().await.expect("kill-server is idempotent");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn pane_operations_reshape_the_hierarchy() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let session = server.new_session("shaping").await.expect("session");
    let window = session
        .try_windows()
        .await
        .expect("windows")
        .into_iter()
        .next()
        .expect("one window");
    let second = window
        .split(SplitOptions::new(SplitDirection::Below).command("sleep 300"))
        .await
        .expect("pane is created");
    let mut first = window
        .try_panes()
        .await
        .expect("panes")
        .into_iter()
        .next()
        .expect("one pane");

    // A title belongs to the pane, and refreshing shows tmux's own view.
    first.set_title("named").await.expect("title is set");
    assert_eq!(first.title().as_bytes(), b"named",);

    first.clear_history().await.expect("history is cleared");

    // Swapping reports positions from tmux, not from the request.
    let before = first.index();
    first.swap_with(&second).await.expect("panes are swapped");
    assert_ne!(first.index(), before, "the pane moved");

    // Breaking a pane out gives it a window of its own.
    let windows_before = session.try_windows().await.expect("windows").len();
    second.break_out().await.expect("pane is broken out");
    assert_eq!(
        session.try_windows().await.expect("windows").len(),
        windows_before + 1,
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn window_operations_move_and_resize() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("moving").await.expect("session");

    let mut window = session
        .new_window(NewWindowOptions::new("mover").command("sleep 300"))
        .await
        .expect("window is created");

    window.move_to(&session, 20).await.expect("window is moved");
    assert_eq!(window.index(), 20);

    window.resize(100, 30).await.expect("window is resized");
    assert_eq!(window.width(), 100);
    assert_eq!(window.height(), 30);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_session_environment_is_set_read_and_removed() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("environed").await.expect("session");

    assert!(
        session.environment("EDITOR").await.expect("read").is_none(),
        "a variable nobody set is absent rather than empty",
    );

    session
        .set_environment("EDITOR", "hx")
        .await
        .expect("the variable is set");
    assert!(matches!(
        session.environment("EDITOR").await.expect("read"),
        Some(libtmux::EnvironmentEntry::Set(value)) if value.as_bytes() == b"hx",
    ));

    // A session variable reaches processes the session starts from now on,
    // not the ones already running.
    let window = session
        .new_window(NewWindowOptions::new("later").command("sleep 300"))
        .await
        .expect("window");
    let pane = window.try_panes().await.expect("panes").remove(0);
    let seen = server
        .format(Some(&pane), "#{?#{==:#{EDITOR},},unset,set}")
        .await
        .expect("a format expands");
    assert_eq!(seen.as_bytes(), b"set");

    session
        .unset_environment("EDITOR")
        .await
        .expect("the variable is removed");
    assert!(session.environment("EDITOR").await.expect("read").is_none());

    // Hiding is not unsetting: tmux keeps an entry marked so a process
    // started here does not inherit the name, which is a third state that
    // reporting absence would hide.
    session
        .set_environment("PAGER", "less")
        .await
        .expect("the variable is set");
    session
        .hide_environment("PAGER")
        .await
        .expect("the variable is hidden");
    assert_eq!(
        session.environment("PAGER").await.expect("read"),
        Some(libtmux::EnvironmentEntry::Removed),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_window_linked_into_two_sessions_is_one_window() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let source = server.new_session("linking-from").await.expect("session");
    let target = server.new_session("linking-to").await.expect("session");

    let mut window = source
        .new_window(NewWindowOptions::new("shared").command("sleep 300"))
        .await
        .expect("window");

    window
        .link_to(&target, None)
        .await
        .expect("the window is linked");

    // One window, two winlinks: the id is the same on both sides, and the
    // link reports itself as linked rather than as a copy.
    let linked = target
        .try_windows()
        .await
        .expect("windows")
        .into_iter()
        .find(|other| other.id() == window.id())
        .expect("the link exists in the target session");
    assert!(linked.is_linked());

    // Renaming through one link is visible through the other, which a copy
    // would not be.
    window
        .rename("renamed")
        .await
        .expect("the window is renamed");
    assert_eq!(
        linked
            .refreshed()
            .await
            .expect("the link still exists")
            .name()
            .as_bytes(),
        b"renamed",
    );

    // Unlinking removes one winlink, and the window survives in the other.
    linked.unlink().await.expect("the link is removed");
    assert!(
        !target
            .try_windows()
            .await
            .expect("windows")
            .iter()
            .any(|other| other.id() == window.id()),
    );
    assert!(
        source
            .try_windows()
            .await
            .expect("windows")
            .iter()
            .any(|other| other.id() == window.id()),
        "the window itself is still there, only the second link is gone",
    );

    // An index another window already holds is a refusal, not an overwrite.
    let occupied = target
        .try_windows()
        .await
        .expect("windows")
        .first()
        .expect("a window")
        .index();
    assert!(window.link_to(&target, Some(occupied)).await.is_err());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn objects_are_found_by_their_tmux_identity() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("identified").await.expect("session");
    let window = session
        .new_window(NewWindowOptions::new("found").command("sleep 300"))
        .await
        .expect("window");
    let pane = window.try_panes().await.expect("panes").remove(0);

    assert_eq!(
        server
            .session_by_id(session.id())
            .await
            .expect("the lookup runs")
            .expect("the session exists")
            .id(),
        session.id(),
    );
    assert_eq!(
        server
            .window_by_id(window.id())
            .await
            .expect("the lookup runs")
            .expect("the window exists")
            .id(),
        window.id(),
    );
    assert_eq!(
        server
            .pane_by_id(pane.id())
            .await
            .expect("the lookup runs")
            .expect("the pane exists")
            .id(),
        pane.id(),
    );

    // An id tmux does not have is absent rather than an error: it is an
    // ordinary answer to "is this still here".
    let gone = window.id().clone();
    window.kill().await.expect("the window is killed");
    assert!(
        server
            .window_by_id(&gone)
            .await
            .expect("the lookup runs")
            .is_none(),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_failure_says_what_to_do_about_it() {
    use libtmux::{ErrorKind, Server};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("classified").await.expect("session");

    // An object that has gone is the branch a caller writes most: it means
    // look again or create, not that the request was wrong.
    let window = session
        .new_window(NewWindowOptions::new("doomed").command("sleep 300"))
        .await
        .expect("window");
    let mut stale = window.clone();
    window.kill().await.expect("the window is killed");

    let gone = stale
        .rename("gone")
        .await
        .expect_err("the window is killed");
    assert_eq!(gone.kind(), ErrorKind::ObjectGone);
    assert!(gone.is_object_gone());
    assert!(!gone.is_transient(), "looking again will not bring it back");

    // Every object kind reports the same way, through whichever command the
    // caller happened to run.
    let pane = session.try_panes().await.expect("panes").remove(0);
    let stale_pane = pane.clone();
    let gone_session = session.clone();
    session.kill().await.expect("the session is killed");

    for (kind, error) in [
        (
            ErrorKind::ObjectGone,
            stale_pane.capture().await.expect_err("the pane is gone"),
        ),
        (
            ErrorKind::ObjectGone,
            gone_session
                .try_windows()
                .await
                .expect_err("the session is gone"),
        ),
    ] {
        assert_eq!(error.kind(), kind, "{error}");
    }

    // Not everything reports a missing target. tmux expands a format against
    // one to nothing and exits zero, so this records what tmux does rather
    // than what would be tidier.
    assert!(
        server
            .format(Some(&stale_pane), "#{pane_id}")
            .await
            .expect("tmux does not refuse a format against a target that has gone")
            .as_bytes()
            .is_empty(),
    );

    // tmux answering "no" is a different thing: the arguments were wrong.
    let refused = server
        .delete_buffer("never-existed")
        .await
        .expect_err("tmux refuses to delete a buffer it does not have");
    assert_eq!(refused.kind(), ErrorKind::Refused);
    assert!(
        !refused.is_transient(),
        "tmux will answer the same way again"
    );

    // tmux not being runnable at all is neither: nothing about the request
    // will change it.
    let absent = Server::builder()
        .tmux_executable("tmux-that-is-not-installed")
        .socket_path("/tmp/libtmux-rs-unreachable.sock")
        .build()
        .expect("the configuration is valid")
        .try_sessions()
        .await
        .expect_err("there is no such executable");
    assert_eq!(absent.kind(), ErrorKind::Unreachable);
    assert!(!absent.is_transient());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_name_tmux_cannot_address_is_rejected_before_it_is_created() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // What the type prevents, demonstrated: tmux accepts the name, stores it,
    // and then splits it on the separator when asked to find it again. The
    // session exists and cannot be reached or killed by its own name.
    server
        .cmd(
            libtmux::Command::new("new-session")
                .arg("-d")
                .arg("-s")
                .arg("a:b"),
        )
        .await
        .expect("tmux accepts the name");

    let found = server
        .cmd(libtmux::Command::new("has-session").arg("-t").arg("a:b"))
        .await
        .expect("the command runs");
    assert!(
        !found.success(),
        "tmux cannot find the session it just made"
    );
    assert!(
        found.stderr_lossy().contains("can't find window"),
        "it split the name at the colon: {:?}",
        found.stderr_lossy(),
    );

    // The session is nonetheless there, which is what makes this worth
    // refusing rather than letting a caller discover later.
    assert_eq!(server.try_sessions().await.expect("sessions").len(), 1);

    // So the type refuses those names, and keeps everything tmux can address.
    assert!(libtmux::SessionName::new("a:b").is_err());
    assert!(libtmux::SessionName::new("c.d").is_err());
    assert!(libtmux::SessionName::new("").is_err());
    assert_eq!(
        libtmux::SessionName::new("has,comma")
            .expect("a comma is addressable")
            .as_str(),
        "has,comma",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_taken_session_name_is_classified_rather_than_a_bare_refusal() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    server.new_session("taken").await.expect("first session");

    // "pick another name" is the one creation failure a caller routinely
    // handles, so it arrives as its own variant rather than as text to
    // match on. Checking with has-session first would race: another process
    // can take the name between the check and the create.
    let error = server
        .new_session("taken")
        .await
        .map(|_| ())
        .expect_err("tmux refuses a duplicate name");

    assert!(
        matches!(&error, libtmux::Error::SessionExists { name } if name == "taken"),
        "the refusal names what was taken: {error:?}",
    );
    assert_eq!(error.kind(), libtmux::ErrorKind::Refused);

    // The first session is untouched by the refusal.
    assert_eq!(server.try_sessions().await.expect("sessions").len(), 1);

    guard.shutdown().await.expect("tmux fixture shuts down");
}
