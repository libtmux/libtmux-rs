//! Caller text must reach tmux literally at every sink tmux expands.
//!
//! tmux runs its format machinery over a name, a title and a start directory
//! before it uses them, so `#(command)` in caller text runs a shell. Each test
//! below asks for a value holding one and checks the stored text or child cwd.
//! The substitution runs `echo`, so an unescaped pass is visible in the stored
//! value rather than only in a side effect: tmux runs `#()` asynchronously, so
//! a marker file checked straight after the call reads absent whether or not
//! the expansion happened, while the stored value is already final.

#![cfg(feature = "test-support")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::Path;
use std::time::Duration;

use libtmux_tokio::test::{TestServer, retry_until};
use libtmux_tokio::{NewSession, NewWindowOptions, SplitDirection, SplitOptions};
use tempfile::TempDir;

/// Holds a `#()` substitution, and is a legal tmux name.
const HOSTILE: &str = "evil#(echo pwned)";

async fn child_current_path(report: &Path) -> Vec<u8> {
    retry_until(Duration::from_secs(5), async || {
        std::fs::read(report).is_ok_and(|bytes| bytes.ends_with(b"\n"))
    })
    .await
    .expect("the child reports its working directory");
    let mut bytes = std::fs::read(report).expect("the working directory report reads");
    assert_eq!(bytes.pop(), Some(b'\n'));
    bytes
}

#[tokio::test]
async fn a_session_name_reaches_tmux_literally() {
    let guard = TestServer::builder().start().await.expect("tmux starts");

    let session = guard
        .session(HOSTILE)
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

    let created = guard
        .session(HOSTILE)
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

    let mut session = guard.session("work").await.expect("the session is created");
    session.rename(HOSTILE).await.expect("the session renames");

    assert_eq!(session.name().to_string_lossy(), HOSTILE);

    guard.shutdown().await.expect("the fixture shuts down");
}

#[tokio::test]
async fn a_window_name_reaches_tmux_literally() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard.session("work").await.expect("the session is created");

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
        .session(NewSession::new("work").window_name(HOSTILE))
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
    let session = guard.session("work").await.expect("the session is created");
    let mut pane = session.panes().await.expect("panes list").remove(0);

    pane.set_title(HOSTILE).await.expect("the title is set");

    assert_eq!(pane.title().to_string_lossy(), HOSTILE);

    guard.shutdown().await.expect("the fixture shuts down");
}

#[tokio::test]
async fn a_start_directory_reaches_tmux_literally() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard.session("work").await.expect("the session is created");

    let scratch = TempDir::new().expect("a scratch directory");
    let directory = scratch.path().join("dir#(echo pwned)");
    let report = scratch.path().join("cwd");
    std::fs::create_dir_all(&directory).expect("the directory is created");

    session
        .new_window(
            NewWindowOptions::new("cwd")
                .start_directory(&directory)
                .environment("LIBTMUX_CWD_REPORT", &report)
                .command("pwd -P > \"$LIBTMUX_CWD_REPORT\"; exec sleep 300"),
        )
        .await
        .expect("the window is created");

    // The creation row can precede process startup, so the child reports its cwd.
    let expected = directory.canonicalize().expect("the directory resolves");
    assert_eq!(
        child_current_path(&report).await,
        expected.as_os_str().as_encoded_bytes(),
    );

    guard.shutdown().await.expect("the fixture shuts down");
}

#[tokio::test]
async fn a_split_start_directory_reaches_tmux_literally() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard.session("work").await.expect("the session is created");
    let pane = session.panes().await.expect("panes list").remove(0);

    let scratch = TempDir::new().expect("a scratch directory");
    let directory = scratch.path().join("split#(echo pwned)");
    let report = scratch.path().join("cwd");
    std::fs::create_dir_all(&directory).expect("the directory is created");

    pane.split(
        SplitOptions::new(SplitDirection::Below)
            .start_directory(&directory)
            .environment("LIBTMUX_CWD_REPORT", &report)
            .command("pwd -P > \"$LIBTMUX_CWD_REPORT\"; exec sleep 300"),
    )
    .await
    .expect("the pane splits");

    // Canonical paths account for macOS resolving `/var` through `/private/var`.
    let expected = directory.canonicalize().expect("the directory resolves");
    assert_eq!(
        child_current_path(&report).await,
        expected.as_os_str().as_encoded_bytes(),
    );

    guard.shutdown().await.expect("the fixture shuts down");
}
