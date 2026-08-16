//! Filtering a gathered hierarchy, where relations have something to relate.

#![cfg(feature = "query")]
#![cfg(feature = "test-support")]
// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use libtmux::query::{Filterable as _, QueryIteratorExt as _};
use libtmux::test::TestServer;
use libtmux::{
    NewSessionOptions, NewWindowOptions, Pane, SessionTree, SplitDirection, SplitOptions,
    WindowTree,
};

/// Build a server with a shape worth asking questions about.
///
/// tmux gives a new session a window, so both sessions name theirs at
/// creation rather than adding one and leaving an unnamed extra behind.
///
/// `build` ends up with windows `editor` (two panes) and `logs`; `idle` with
/// one window, `shell`.
async fn populate(server: &libtmux::Server) {
    let build = server
        .new_session(NewSessionOptions::new("build").window_name("editor"))
        .await
        .expect("session");
    build
        .windows()
        .await
        .expect("windows")
        .remove(0)
        .split(SplitOptions::new(SplitDirection::Below).command("sleep 300"))
        .await
        .expect("pane");
    build
        .new_window(NewWindowOptions::new("logs").command("sleep 300"))
        .await
        .expect("window");

    server
        .new_session(NewSessionOptions::new("idle").window_name("shell"))
        .await
        .expect("session");
}

#[tokio::test]
async fn a_session_is_selected_by_what_its_windows_hold() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    populate(server).await;

    let branches = server.hierarchy().await.expect("hierarchy");
    let sessions = SessionTree::filter_fields();
    let windows = WindowTree::filter_fields();

    // The question a Session handle cannot ask, because it does not hold its
    // windows: which sessions contain a window named this.
    let named = sessions
        .windows
        .any(windows.window.window_name.eq("editor"));
    let matched: Vec<_> = branches
        .iter()
        .matching(&named)
        .map(|branch| branch.session.name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(matched, ["build"]);

    // Two levels down, and combined with the session's own fields, which sit
    // alongside the relation rather than behind it.
    // A second pane exists, which is pane index 1. The v1 grammar has
    // equality and set membership, not ordering, so this asks for the index
    // rather than for a count.
    let split_window = sessions.windows.any(
        windows
            .panes
            .any(Pane::filter_fields().pane_index.eq(1_u32)),
    );
    let busy = sessions
        .session
        .session_name
        .starts_with("bui")
        .and(split_window);
    assert_eq!(
        branches.iter().matching(&busy).count(),
        1,
        "one session has a window with more than one pane",
    );

    // A session with no window like that matches none of it.
    let quiet = sessions
        .windows
        .none(windows.window.window_name.eq("editor"));
    let quiet: Vec<_> = branches
        .iter()
        .matching(&quiet)
        .map(|branch| branch.session.name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(quiet, ["idle"]);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn every_window_must_satisfy_all_and_an_empty_relation_does_so_vacuously() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    populate(server).await;

    let branches = server.hierarchy().await.expect("hierarchy");
    let sessions = SessionTree::filter_fields();
    let windows = WindowTree::filter_fields();

    // The vacuous-empty rule is exercised where a relation can actually be
    // empty, in tests/filter_derive.rs: tmux never gives a window no panes.
    //
    // `all` over a session whose windows are not all named this.
    let uniform = sessions.windows.all(windows.window.window_name.eq("shell"));
    let matched: Vec<_> = branches
        .iter()
        .matching(&uniform)
        .map(|branch| branch.session.name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(matched, ["idle"], "only the session with one such window");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[cfg(feature = "serde")]
#[tokio::test]
async fn a_relation_survives_the_portable_envelope() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    populate(server).await;

    let sessions = SessionTree::filter_fields();
    let windows = WindowTree::filter_fields();
    let expression = sessions
        .windows
        .any(windows.window.window_name.eq("editor"));

    // The tree types carry their own target names, so an expression over them
    // is as portable as one over a session. That is what lets a caller send a
    // question about a session's contents rather than only about a session.
    let wire = serde_json::to_value(&expression).expect("the expression serializes");
    assert_eq!(wire["target"], "session_tree");
    assert_eq!(wire["expr"]["op"], "relation");
    assert_eq!(wire["expr"]["quantifier"], "any");
    assert_eq!(wire["expr"]["field"], "windows");
    // The related expression rides bare. Its target is decided by the schema
    // rather than repeated on the wire, so it cannot disagree with itself.
    assert_eq!(wire["expr"]["expr"]["field"], "window_name");

    let decoded: libtmux::query::FilterExpr<SessionTree> =
        serde_json::from_value(wire).expect("the expression round-trips");

    let branches = server.hierarchy().await.expect("hierarchy");
    assert_eq!(
        branches
            .iter()
            .matching(&decoded)
            .map(|branch| branch.session.name().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        ["build"],
    );

    // A related expression naming a field the related type does not have is
    // rejected on the way in, not silently evaluated as false.
    let mismatched = serde_json::json!({
        "version": 1,
        "target": "session_tree",
        "expr": {
            "op": "relation",
            "quantifier": "any",
            "field": "windows",
            "expr": {"op": "eq", "field": "pane_index", "value": 0},
        },
    });
    assert!(
        serde_json::from_value::<libtmux::query::FilterExpr<SessionTree>>(mismatched).is_err(),
        "pane_index is not a window field",
    );

    // And a relation this type does not have is rejected the same way.
    let unknown = serde_json::json!({
        "version": 1,
        "target": "session_tree",
        "expr": {
            "op": "relation",
            "quantifier": "any",
            "field": "clients",
            "expr": {"op": "eq", "field": "window_name", "value": "x"},
        },
    });
    assert!(serde_json::from_value::<libtmux::query::FilterExpr<SessionTree>>(unknown).is_err(),);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_numeric_field_compares_by_order_as_well_as_by_value() {
    use libtmux::Session;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let busy = server
        .new_session(NewSessionOptions::new("busy"))
        .await
        .expect("session");
    for name in ["one", "two", "three"] {
        busy.new_window(NewWindowOptions::new(name).command("sleep 300"))
            .await
            .expect("window");
    }
    server
        .new_session(NewSessionOptions::new("quiet"))
        .await
        .expect("session");

    let sessions = server.sessions().await.expect("sessions");
    let fields = Session::filter_fields();
    let named = |expression: &libtmux::query::FilterExpr<Session>| {
        let mut names: Vec<_> = sessions
            .iter()
            .matching(expression)
            .map(|session| session.name().to_string_lossy().into_owned())
            .collect();
        names.sort_unstable();
        names
    };

    // busy holds four windows, quiet holds one.
    assert_eq!(named(&fields.session_windows.gt(1_u32)), ["busy"]);
    assert_eq!(named(&fields.session_windows.gte(4_u32)), ["busy"]);
    assert_eq!(named(&fields.session_windows.lt(4_u32)), ["quiet"]);
    assert_eq!(named(&fields.session_windows.lte(1_u32)), ["quiet"]);
    assert_eq!(
        named(&fields.session_windows.gte(1_u32)),
        ["busy", "quiet"],
        "a bound is inclusive where the name says so",
    );
    assert!(named(&fields.session_windows.gt(9_u32)).is_empty());

    // Ordering is numeric, not lexicographic: ten windows beat a bound of
    // nine, where a string comparison would put "10" below "9".
    let ten = server
        .new_session(NewSessionOptions::new("ten"))
        .await
        .expect("session");
    for index in 0..9 {
        ten.new_window(NewWindowOptions::new(format!("w{index}").as_str()).command("sleep 300"))
            .await
            .expect("window");
    }

    let sessions = server.sessions().await.expect("sessions");
    let over_nine: Vec<_> = sessions
        .iter()
        .matching(&fields.session_windows.gt(9_u32))
        .map(|session| session.name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(over_nine, ["ten"]);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[cfg(feature = "serde")]
#[test]
fn ordering_travels_the_wire_and_is_refused_where_it_has_no_meaning() {
    use libtmux::Session;

    let expression = Session::filter_fields().session_windows.gt(2_u32);
    let wire = serde_json::to_value(&expression).expect("the expression serializes");
    assert_eq!(wire["expr"]["op"], "gt");
    // Integers ride as text, which is how this grammar avoids arguing with
    // JSON about the width of a number.
    assert_eq!(wire["expr"]["value"], "2");

    let decoded: libtmux::query::FilterExpr<Session> =
        serde_json::from_value(wire).expect("the expression round-trips");
    assert_eq!(decoded, expression);

    // Text has no ordering, and asking for it is refused rather than
    // evaluating to false.
    let on_text = serde_json::json!({
        "version": 1,
        "target": "session",
        "expr": {"op": "gt", "field": "session_name", "value": "a"},
    });
    assert!(serde_json::from_value::<libtmux::query::FilterExpr<Session>>(on_text).is_err());

    // Ordering compares the decoded integer, not the string it rode in as.
    // Lexicographically "10" is below "9", so this distinguishes the two.
    let above_nine = serde_json::json!({
        "version": 1,
        "target": "session",
        "expr": {"op": "gt", "field": "session_windows", "value": "9"},
    });
    let above_nine: libtmux::query::FilterExpr<Session> =
        serde_json::from_value(above_nine).expect("the expression decodes");
    assert_eq!(
        above_nine,
        Session::filter_fields().session_windows.gt(9_u32),
        "the wire form and the authored form are the same expression",
    );

    // A bound is one value, not a set.
    let two_bounds = serde_json::json!({
        "version": 1,
        "target": "session",
        "expr": {"op": "gt", "field": "session_windows", "value": ["1", "2"]},
    });
    assert!(serde_json::from_value::<libtmux::query::FilterExpr<Session>>(two_bounds).is_err());
}
