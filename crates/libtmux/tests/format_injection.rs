//! Caller text must reach tmux literally at every sink tmux expands.
//!
//! tmux runs its format machinery over a name, a title and a start directory
//! before it uses them, so `#(command)` in caller text runs a shell. Each test
//! below asks for a value holding one and asserts tmux stored what was asked.
//! The substitution runs `echo`, so an unescaped pass is visible in the stored
//! value rather than only in a side effect: tmux runs `#()` asynchronously, so
//! a marker file checked straight after the call reads absent whether or not
//! the expansion happened, while the stored value is already final.

#![cfg(feature = "test-support")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use libtmux::test::TestServer;
use libtmux::{NewSessionOptions, NewWindowOptions, SplitDirection, SplitOptions};
use tempfile::TempDir;

/// Holds a `#()` substitution, and is a legal tmux name.
const HOSTILE: &str = "evil#(echo pwned)";

#[tokio::test]
async fn a_session_name_reaches_tmux_literally() {
    let guard = TestServer::builder().start().await.expect("tmux starts");

    let session = guard
        .server()
        .new_session(HOSTILE)
        .await
        .expect("the session is created");

    assert_eq!(session.name().to_string_lossy(), HOSTILE);

    guard.shutdown().await.expect("the fixture shuts down");
}

/// Creating and then finding by the same name has to agree, or escaping on the
/// way out would have made the name unreachable on the way back.
#[tokio::test]
async fn a_hostile_name_round_trips_through_lookup() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let created = server
        .new_session(HOSTILE)
        .await
        .expect("the session is created");

    assert!(
        server
            .has_session(HOSTILE)
            .await
            .expect("the listing reads")
    );
    let found = server
        .session(HOSTILE)
        .await
        .expect("the listing reads")
        .expect("the session is found by the name it was given");
    assert_eq!(found.id(), created.id());

    guard.shutdown().await.expect("the fixture shuts down");
}

#[tokio::test]
async fn a_renamed_session_reaches_tmux_literally() {
    let guard = TestServer::builder().start().await.expect("tmux starts");

    let mut session = guard
        .server()
        .new_session("work")
        .await
        .expect("the session is created");
    session.rename(HOSTILE).await.expect("the session renames");

    assert_eq!(session.name().to_string_lossy(), HOSTILE);

    guard.shutdown().await.expect("the fixture shuts down");
}

#[tokio::test]
async fn a_window_name_reaches_tmux_literally() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard
        .server()
        .new_session("work")
        .await
        .expect("the session is created");

    let created = session
        .new_window(HOSTILE)
        .await
        .expect("the window is created");
    assert_eq!(created.name().to_string_lossy(), HOSTILE);

    let mut renamed = session
        .new_window("second")
        .await
        .expect("the window is created");
    renamed.rename(HOSTILE).await.expect("the window renames");
    assert_eq!(renamed.name().to_string_lossy(), HOSTILE);

    guard.shutdown().await.expect("the fixture shuts down");
}

#[tokio::test]
async fn a_first_window_name_reaches_tmux_literally() {
    let guard = TestServer::builder().start().await.expect("tmux starts");

    let session = guard
        .server()
        .new_session(NewSessionOptions::new("work").window_name(HOSTILE))
        .await
        .expect("the session is created");

    let window = session
        .active_window()
        .await
        .expect("the window is read")
        .expect("a session has a window");
    assert_eq!(window.name().to_string_lossy(), HOSTILE);

    guard.shutdown().await.expect("the fixture shuts down");
}

#[tokio::test]
async fn a_pane_title_reaches_tmux_literally() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard
        .server()
        .new_session("work")
        .await
        .expect("the session is created");
    let mut pane = session.panes().await.expect("panes list").remove(0);

    pane.set_title(HOSTILE).await.expect("the title is set");

    assert_eq!(pane.title().to_string_lossy(), HOSTILE);

    guard.shutdown().await.expect("the fixture shuts down");
}

#[tokio::test]
async fn a_start_directory_reaches_tmux_literally() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard
        .server()
        .new_session("work")
        .await
        .expect("the session is created");

    // A directory that really holds a `#`, which is also what an unescaped
    // pass would mangle. tmux reports the path it started in.
    let scratch = TempDir::new().expect("a scratch directory");
    let directory = scratch.path().join("dir#(echo pwned)");
    std::fs::create_dir_all(&directory).expect("the directory is created");

    let window = session
        .new_window(NewWindowOptions::new("cwd").start_directory(&directory))
        .await
        .expect("the window is created");
    let pane = window
        .active_pane()
        .await
        .expect("the pane is read")
        .expect("a window has a pane");

    // tmux reports the directory the pane resolved to. On macOS the
    // temporary root arrives as `/var/...` and resolves to
    // `/private/var/...`, so the expectation has to be canonical too or
    // it is only testing Linux.
    let expected = directory.canonicalize().expect("the directory resolves");

    assert_eq!(
        pane.current_path().map(|path| path.to_string_lossy()),
        Some(expected.to_string_lossy()),
    );

    guard.shutdown().await.expect("the fixture shuts down");
}

#[tokio::test]
async fn a_split_start_directory_reaches_tmux_literally() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard
        .server()
        .new_session("work")
        .await
        .expect("the session is created");
    let pane = session.panes().await.expect("panes list").remove(0);

    let scratch = TempDir::new().expect("a scratch directory");
    let directory = scratch.path().join("split#(echo pwned)");
    std::fs::create_dir_all(&directory).expect("the directory is created");

    let split = pane
        .split(SplitOptions::new(SplitDirection::Below).start_directory(&directory))
        .await
        .expect("the pane splits")
        .refreshed()
        .await
        .expect("the pane is read back");

    // tmux reports the directory the pane resolved to. On macOS the
    // temporary root arrives as `/var/...` and resolves to
    // `/private/var/...`, so the expectation has to be canonical too or
    // it is only testing Linux.
    let expected = directory.canonicalize().expect("the directory resolves");

    assert_eq!(
        split.current_path().map(|path| path.to_string_lossy()),
        Some(expected.to_string_lossy()),
    );

    guard.shutdown().await.expect("the fixture shuts down");
}
