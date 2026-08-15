//! Filtering public hierarchy handles with typed expressions.

#![cfg(feature = "query")]
#![cfg(feature = "test-support")]
// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use libtmux::query::{Filterable as _, QueryIteratorExt as _};
use libtmux::test::TestServer;
use libtmux::{Command, Server, TmuxText};

async fn run(server: &Server, command: Command) {
    let result = server.cmd(command).await.expect("setup command executes");
    assert!(result.success(), "setup command succeeds");
}

async fn new_session(server: &Server, name: &str) {
    run(
        server,
        Command::new("new-session")
            .arg("-d")
            .arg("-s")
            .arg(name)
            // `exec` so the pane's process is `sleep` itself. Without it the
            // pane runs `<shell> -c "sleep 300"`, and whether the shell then
            // execs or forks is an optimization POSIX does not require: zsh
            // and bash exec, dash forks and reports `sh`. The assertion below
            // reads the running command, so it says which it wants.
            .arg("exec sleep 300"),
    )
    .await;
}

#[tokio::test]
async fn a_typed_expression_filters_the_handles_a_listing_returned() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    for name in ["build", "build-docs", "test"] {
        new_session(server, name).await;
    }

    let sessions = server.try_sessions().await.expect("sessions list");
    let fields = libtmux::Session::filter_fields();

    // Native iteration remains the floor: a closure needs no expression.
    assert_eq!(
        sessions
            .iter()
            .filter(|session| session.window_count() == 1)
            .count(),
        3,
    );

    // The declarative path filters the very type the listing returned.
    let starts_with_build = fields.session_name.starts_with("build");
    let names = sessions
        .iter()
        .matching(&starts_with_build)
        .map(libtmux::Session::name)
        .map(TmuxText::as_bytes)
        .collect::<Vec<_>>();
    assert_eq!(names, [&b"build"[..], &b"build-docs"[..]]);

    // Composition is ordinary boolean algebra over an inert tree.
    let exactly_build = starts_with_build
        .clone()
        .and(fields.session_name.eq("build"));
    let found = sessions
        .iter()
        .matching(&exactly_build)
        .exactly_one()
        .expect("exactly one session is named build");
    assert_eq!(found.name().as_bytes(), &b"build"[..]);

    // Cardinality distinguishes zero from many without collecting.
    let none = fields.session_name.eq("absent");
    assert!(sessions.iter().matching(&none).exactly_one().is_err());
    assert_eq!(sessions.iter().matching(&none).one_or_none(), Ok(None));
    assert!(
        sessions
            .iter()
            .matching(&starts_with_build)
            .one_or_none()
            .is_err()
    );

    // Negation is available on the same tree.
    assert_eq!(
        sessions
            .iter()
            .matching(&starts_with_build.clone().not())
            .count(),
        1
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn every_hierarchy_handle_is_filterable() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    new_session(server, "work").await;
    run(
        server,
        Command::new("split-window")
            .arg("-d")
            .arg("-t")
            .arg("work")
            // `exec` so the pane's process is `sleep` itself. Without it the
            // pane runs `<shell> -c "sleep 300"`, and whether the shell then
            // execs or forks is an optimization POSIX does not require: zsh
            // and bash exec, dash forks and reports `sh`. The assertion below
            // reads the running command, so it says which it wants.
            .arg("exec sleep 300"),
    )
    .await;

    let windows = server.try_windows().await.expect("windows list");
    let window_fields = libtmux::Window::filter_fields();
    assert_eq!(
        windows
            .iter()
            .matching(&window_fields.window_panes.eq(2_u32))
            .count(),
        1,
    );

    let pane_fields = libtmux::Pane::filter_fields();
    // A pane reports its shell until the requested command execs, so wait for
    // the state under test rather than sleeping a fixed amount.
    let running_sleep = pane_fields.pane_current_command.contains("sleep");
    let panes = poll_until(server, |panes| {
        panes.iter().matching(&running_sleep).count() == 2
    })
    .await;
    let active = panes
        .iter()
        .matching(&pane_fields.pane_active.eq(true))
        .exactly_one()
        .expect("exactly one pane is active");
    assert!(active.is_active());

    // An integer comparison on a text field would not compile, so the only
    // expressible predicates are the ones the field type allows.
    assert_eq!(panes.iter().matching(&running_sleep).count(), 2);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Re-list panes until a predicate holds, or fail at the deadline.
async fn poll_until(
    server: &Server,
    ready: impl Fn(&[libtmux::Pane]) -> bool,
) -> Vec<libtmux::Pane> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let panes = server.try_panes().await.expect("panes list");
        if ready(&panes) {
            return panes;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "panes did not reach the expected state before the deadline",
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn a_filter_expression_is_inert_and_reusable() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    new_session(server, "alpha").await;
    let fields = libtmux::Session::filter_fields();
    let expression = fields.session_name.eq("alpha");

    // The expression holds no connection and no snapshot, so it applies before
    // and after the state it describes changes.
    let before = server.try_sessions().await.expect("sessions list");
    assert_eq!(before.iter().matching(&expression).count(), 1);

    run(
        server,
        Command::new("rename-session")
            .arg("-t")
            .arg("alpha")
            .arg("renamed"),
    )
    .await;

    let after = server.try_sessions().await.expect("sessions list");
    assert_eq!(after.iter().matching(&expression).count(), 0);
    // The original snapshot is unchanged, so the same expression still matches.
    assert_eq!(before.iter().matching(&expression).count(), 1);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn searching_filters_a_listing_and_keeps_the_lenient_loud_pair() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("searched").await.expect("session");
    for name in ["build", "build-docs", "test"] {
        session
            .new_window(libtmux::NewWindowOptions::new(name))
            .await
            .expect("window");
    }

    let fields = libtmux::Window::filter_fields();

    // The expression decides, and it is the same one a listing takes: the
    // search is the listing plus the filter, not a second query language.
    let building = session
        .try_search_windows(&fields.window_name.starts_with("build"))
        .await
        .expect("search");
    assert_eq!(building.len(), 2);

    // A matcher nothing satisfies is an empty answer, not a failure.
    let none = session
        .try_search_windows(&fields.window_name.eq("absent"))
        .await
        .expect("search");
    assert!(none.is_empty());

    // The lenient form agrees while tmux is reachable; it differs only when
    // the listing itself fails, which is what the pair exists to distinguish.
    assert_eq!(
        session
            .search_windows(&fields.window_name.starts_with("build"))
            .await
            .len(),
        building.len(),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}
