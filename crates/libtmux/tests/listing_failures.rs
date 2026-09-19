//! A listing keeps the reason it failed.
//!
//! There used to be an `_or_empty` twin of each of these, returning an empty
//! vector for "nothing there" and for "the listing failed" alike. Neither
//! consumer crate ever called one, and a reconciler reading "no sessions" from
//! an outage deletes everything, so the twins are gone and the reason is not
//! optional. A caller who would still rather show nothing writes
//! `.unwrap_or_default()`, where it reads as the choice it is.

#![cfg(all(feature = "test-support", feature = "query"))]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use libtmux::test::TestServer;

#[tokio::test]
async fn every_listing_reports_a_server_that_is_gone() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server().clone();
    let session = server.new_session("gone").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a session has a window");

    let panes = {
        use libtmux::query::Filterable as _;
        libtmux::Pane::filter_fields().pane_id.eq("%0")
    };
    let windows = {
        use libtmux::query::Filterable as _;
        libtmux::Window::filter_fields().window_id.eq("@0")
    };

    // Positive control: each listing answers while the server is up, so a
    // later error is the server being gone rather than a method that never
    // worked.
    assert!(!server.sessions().await.expect("sessions").is_empty());
    assert!(!session.windows().await.expect("windows").is_empty());
    assert!(!window.panes().await.expect("panes").is_empty());

    guard.shutdown().await.expect("tmux fixture shuts down");

    // Every listing now fails, and each says so rather than reporting empty.
    // The kind is `Transport` rather than `ServerGone` because the fixture
    // shuts the executor down as well as the daemon, so nothing reaches tmux
    // to be told the server is gone. What matters here is that no listing
    // answers an outage with an empty vector.
    macro_rules! assert_reports_failure {
        ($label:literal, $call:expr) => {
            let error = $call
                .await
                .expect_err(concat!($label, " reports the failure"));
            assert!(
                !matches!(error.kind(), libtmux::ErrorKind::Decode),
                "{}: {error}",
                $label,
            );
        };
    }

    assert_reports_failure!("Server::sessions", server.sessions());
    assert_reports_failure!("Server::windows", server.windows());
    assert_reports_failure!("Server::panes", server.panes());
    assert_reports_failure!("Server::clients", server.clients());
    assert_reports_failure!("Server::attached_sessions", server.attached_sessions());
    assert_reports_failure!("Session::windows", session.windows());
    assert_reports_failure!("Session::panes", session.panes());
    assert_reports_failure!("Session::search_windows", session.search_windows(windows));
    assert_reports_failure!("Window::panes", window.panes());
    assert_reports_failure!("Window::search_panes", window.search_panes(panes));
    assert_reports_failure!("Window::linked_sessions", window.linked_sessions());
}
