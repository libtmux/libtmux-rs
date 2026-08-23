//! Integration tests for public hierarchy discovery against real tmux.

#![cfg(feature = "test-support")]
// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use libtmux::test::TestServer;
use libtmux::{Client, Command, NewSessionOptions, NewWindowOptions, Pane, Server, Session};
use libtmux::{SplitDirection, SplitOptions, Window};
use static_assertions::assert_impl_all;

// roadmap.md promises public handles are concrete, cheap to clone, and
// `Send + Sync`. Every peripheral type had this pinned and the four headline
// handles did not.
assert_impl_all!(Session: Clone, std::fmt::Debug, Eq, std::hash::Hash, Send, Sync);
assert_impl_all!(Window: Clone, std::fmt::Debug, Eq, std::hash::Hash, Send, Sync);
assert_impl_all!(Pane: Clone, std::fmt::Debug, Eq, std::hash::Hash, Send, Sync);
assert_impl_all!(Client: Clone, std::fmt::Debug, Eq, std::hash::Hash, Send, Sync);

/// Run a setup command and require it to succeed.
async fn run(server: &Server, command: Command) {
    let result = server.cmd(command).await.expect("setup command executes");
    assert!(
        result.success(),
        "setup command succeeds: {:?}",
        result.stderr_lossy(),
    );
}

/// Create a detached session running a long-lived placeholder process.
async fn new_session(server: &Server, name: &str) {
    run(
        server,
        Command::new("new-session")
            .arg("-d")
            .arg("-s")
            .arg(name)
            .arg("sleep 300"),
    )
    .await;
}

#[tokio::test]
async fn empty_server_lists_nothing_across_the_hierarchy() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    assert!(server.sessions_or_empty().await.is_empty());
    assert!(server.windows_or_empty().await.is_empty());
    assert!(server.panes_or_empty().await.is_empty());

    // An empty listing is an ordinary result, not a decoding failure.
    assert!(
        server
            .sessions()
            .await
            .expect("loud form succeeds")
            .is_empty()
    );
    assert!(
        server
            .windows()
            .await
            .expect("loud form succeeds")
            .is_empty()
    );
    assert!(server.panes().await.expect("loud form succeeds").is_empty());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn listings_preserve_tmux_order_and_report_snapshot_values() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    for name in ["alpha", "beta", "gamma"] {
        new_session(server, name).await;
    }

    let sessions = server.sessions().await.expect("sessions list");
    let names = sessions
        .iter()
        .map(|session| session.name().as_bytes().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [b"alpha".to_vec(), b"beta".to_vec(), b"gamma".to_vec()]
    );

    for session in &sessions {
        assert_eq!(session.window_count(), 1);
        assert!(
            !session.is_attached(),
            "a detached session reports no clients"
        );
        assert_eq!(session.attached_client_count(), 0);
        assert!(session.created() > 0);
        assert_eq!(
            session.last_attached(),
            None,
            "a session that was never attached has no last-attach time",
        );
    }

    // Distinct sessions are distinct handles; a clone of one is equal to it.
    assert_ne!(sessions[0], sessions[1]);
    assert_eq!(sessions[0], sessions[0].clone());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_linked_window_appears_once_per_session_that_links_it() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    new_session(server, "origin").await;
    new_session(server, "borrower").await;

    let origin = session_id(server, "origin").await;
    let source = server
        .windows()
        .await
        .expect("windows list")
        .into_iter()
        .find(|window| window.session_id().to_string() == origin)
        .expect("the origin session has a window");

    run(
        server,
        Command::new("link-window")
            .arg("-s")
            .arg(source.id().to_string())
            .arg("-t")
            .arg("borrower:9"),
    )
    .await;

    let windows = server.windows().await.expect("windows list");
    let links = windows
        .iter()
        .filter(|window| window.id() == source.id())
        .collect::<Vec<_>>();

    assert_eq!(
        links.len(),
        2,
        "a window linked into two sessions yields one row per link",
    );

    // Same underlying window...
    assert_eq!(links[0].id(), links[1].id());
    // ...but different places in the hierarchy, so not equal handles.
    assert_ne!(links[0], links[1]);
    assert_ne!(links[0].session_id(), links[1].session_id());
    assert!(
        links.iter().all(|window| window.is_linked()),
        "both edges report the window as linked",
    );

    // Panes under a linked window repeat for the same reason.
    let panes = server.panes().await.expect("panes list");
    let pane_rows = panes
        .iter()
        .filter(|pane| pane.window_id() == source.id())
        .collect::<Vec<_>>();
    assert_eq!(pane_rows.len(), 2, "the pane appears once per window link");
    // Unlike windows, pane identity ignores the discovery edge.
    assert_eq!(pane_rows[0], pane_rows[1]);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Resolve a session id by name through the public listing.
async fn session_id(server: &Server, name: &str) -> String {
    server
        .sessions()
        .await
        .expect("sessions list")
        .into_iter()
        .find(|session| session.name().as_bytes() == name.as_bytes())
        .expect("the named session exists")
        .id()
        .to_string()
}

#[tokio::test]
async fn panes_report_their_window_and_process_details() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    new_session(server, "work").await;
    run(
        server,
        Command::new("split-window")
            .arg("-t")
            .arg("work")
            .arg("-d")
            .arg("sleep 300"),
    )
    .await;

    let panes = server.panes().await.expect("panes list");
    assert_eq!(panes.len(), 2, "the split produced a second pane");

    let windows = server.windows().await.expect("windows list");
    assert_eq!(windows.len(), 1, "both panes share one window");
    assert_eq!(windows[0].pane_count(), 2);

    for pane in &panes {
        assert_eq!(pane.window_id(), windows[0].id());
        assert!(pane.pid() > 0);
        assert!(pane.width() > 0);
        assert!(pane.height() > 0);
        assert!(!pane.is_dead(), "a running pane is not dead");
        assert!(!pane.is_in_mode(), "a fresh pane has no mode open");
    }

    assert_eq!(
        panes.iter().filter(|pane| pane.is_active()).count(),
        1,
        "exactly one pane is active",
    );
    assert_ne!(panes[0], panes[1]);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn traversal_scopes_each_level_to_its_parent() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    new_session(server, "one").await;
    new_session(server, "two").await;
    run(
        server,
        Command::new("new-window")
            .arg("-d")
            .arg("-t")
            .arg("one")
            .arg("sleep 300"),
    )
    .await;
    run(
        server,
        Command::new("split-window")
            .arg("-d")
            .arg("-t")
            .arg("one")
            .arg("sleep 300"),
    )
    .await;

    let sessions = server.sessions().await.expect("sessions list");
    let one = sessions
        .iter()
        .find(|session| session.name().as_bytes() == b"one")
        .expect("session one exists");
    let two = sessions
        .iter()
        .find(|session| session.name().as_bytes() == b"two")
        .expect("session two exists");

    // Session scoping: the server sees three windows, each session sees its own.
    assert_eq!(server.windows().await.expect("windows").len(), 3);
    assert_eq!(one.windows().await.expect("windows").len(), 2);
    assert_eq!(two.windows().await.expect("windows").len(), 1);

    let one_windows = one.windows().await.expect("windows");
    assert!(
        one_windows
            .iter()
            .all(|window| window.session_id() == one.id()),
        "a session lists only its own links",
    );

    // Exactly one window is active per session, and it is reachable directly.
    let active = one
        .active_window()
        .await
        .expect("active window resolves")
        .expect("a session always has an active window");
    assert!(active.is_active());
    assert_eq!(
        one_windows
            .iter()
            .filter(|window| window.is_active())
            .count(),
        1,
    );

    // Window scoping: the split landed in session one's active window.
    let split_window = one_windows
        .iter()
        .find(|window| window.pane_count() == 2)
        .expect("one window holds the split");
    let panes = split_window.panes().await.expect("panes list");
    assert_eq!(panes.len(), 2);
    assert!(
        panes
            .iter()
            .all(|pane| pane.window_id() == split_window.id())
    );

    let active_pane = split_window
        .active_pane()
        .await
        .expect("active pane resolves")
        .expect("a window always has an active pane");
    assert!(active_pane.is_active());

    // Session-scoped panes span every window in that session.
    assert_eq!(one.panes().await.expect("panes").len(), 3);
    assert_eq!(two.panes().await.expect("panes").len(), 1);

    // Upward traversal re-reads tmux and lands back where we started.
    let parent = split_window
        .session()
        .await
        .expect("parent resolves")
        .expect("the parent session still exists");
    assert_eq!(&parent, one);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn upward_traversal_reflects_a_rename_rather_than_the_snapshot() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    new_session(server, "before").await;
    let window = server
        .windows()
        .await
        .expect("windows list")
        .into_iter()
        .next()
        .expect("one window");

    run(
        server,
        Command::new("rename-session")
            .arg("-t")
            .arg("before")
            .arg("after"),
    )
    .await;

    // The handle still holds its original snapshot...
    let parent = window
        .session()
        .await
        .expect("parent resolves")
        .expect("the parent session still exists");

    // ...but resolving the parent re-reads tmux, so the new name is visible.
    assert_eq!(parent.name().as_bytes().to_vec(), b"after".to_vec());
    // Identity is unchanged by a rename, so the handles still match.
    assert_eq!(parent.id(), window.session_id());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn refreshing_one_handle_leaves_its_clones_alone() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    new_session(server, "before").await;
    let mut session = server
        .sessions()
        .await
        .expect("sessions list")
        .into_iter()
        .next()
        .expect("one session");
    let stale = session.clone();

    run(
        server,
        Command::new("rename-session")
            .arg("-t")
            .arg("before")
            .arg("after"),
    )
    .await;

    // Neither handle changes until one is asked to.
    assert_eq!(session.name().as_bytes().to_vec(), b"before".to_vec());
    assert_eq!(stale.name().as_bytes().to_vec(), b"before".to_vec());

    session.refresh().await.expect("the session still exists");

    assert_eq!(session.name().as_bytes().to_vec(), b"after".to_vec());
    assert_eq!(
        stale.name().as_bytes().to_vec(),
        b"before".to_vec(),
        "each handle owns its snapshot, so a clone is unaffected",
    );
    // Identity survives a rename, so the handles remain equal.
    assert_eq!(session, stale);

    // The non-mutating form leaves the receiver as it was.
    let refreshed = stale.refreshed().await.expect("the session still exists");
    assert_eq!(refreshed.name().as_bytes().to_vec(), b"after".to_vec());
    assert_eq!(stale.name().as_bytes().to_vec(), b"before".to_vec());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn refresh_reports_an_object_that_tmux_no_longer_has() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    new_session(server, "keep").await;
    new_session(server, "drop").await;

    let mut doomed = server
        .sessions()
        .await
        .expect("sessions list")
        .into_iter()
        .find(|session| session.name().as_bytes() == b"drop")
        .expect("the doomed session exists");

    run(server, Command::new("kill-session").arg("-t").arg("drop")).await;

    let error = doomed
        .refresh()
        .await
        .expect_err("a killed session cannot refresh");
    assert!(
        matches!(
            error,
            libtmux::Error::ObjectGone {
                kind: libtmux::ObjectKind::Session,
                ..
            },
        ),
        "a vanished object is distinct from a connection failure, got {error:?}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn refresh_follows_a_window_that_moved_index() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    new_session(server, "work").await;
    let mut window = server
        .windows()
        .await
        .expect("windows list")
        .into_iter()
        .next()
        .expect("one window");
    let original = window.index();

    run(
        server,
        Command::new("move-window")
            .arg("-s")
            .arg(window.id().to_string())
            .arg("-t")
            .arg("work:12"),
    )
    .await;

    window.refresh().await.expect("the window still exists");

    assert_eq!(window.index(), 12);
    assert_ne!(window.index(), original);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn liveness_and_session_lookup_answer_over_raw_bytes() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    assert!(server.is_alive().await, "a started server answers");
    assert!(!server.has_session("absent").await.expect("lookup succeeds"));

    new_session(server, "present").await;
    assert!(
        server
            .has_session("present")
            .await
            .expect("lookup succeeds")
    );
    assert!(
        !server.has_session("presen").await.expect("lookup succeeds"),
        "matching is exact rather than a prefix",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_dead_server_yields_empty_leniently_and_an_error_loudly() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server().clone();
    guard.shutdown().await.expect("tmux fixture shuts down");

    // The lenient contract hides the cause behind an empty listing.
    assert!(server.sessions_or_empty().await.is_empty());
    assert!(server.windows_or_empty().await.is_empty());
    assert!(server.panes_or_empty().await.is_empty());

    // The loud form keeps it. This is the whole reason both forms exist: the
    // executor is gone, which is a caller mistake rather than an empty server.
    let error = server
        .sessions()
        .await
        .expect_err("a shut-down executor cannot list");
    assert!(
        matches!(error, libtmux::Error::ExecutorShutdown { .. }),
        "the loud form preserves the real cause, got {error:?}",
    );

    server
        .shutdown()
        .await
        .expect("executor shutdown is idempotent");
}

#[tokio::test]
async fn an_absent_daemon_is_empty_to_one_form_and_a_reason_to_the_other() {
    // A server that was never started is the ordinary "nothing running" case,
    // and the pair of listing forms is what lets one caller shrug at it while
    // another must not.
    let directory = tempfile::tempdir().expect("temporary directory");
    let server = Server::builder()
        .socket_path(directory.path().join("absent.sock"))
        .build()
        .expect("an inert server handle is built");

    assert!(
        server.sessions_or_empty().await.is_empty(),
        "the lenient form suits a status line, which has nothing to say",
    );

    let error = server
        .sessions()
        .await
        .expect_err("the loud form keeps the reason");
    assert!(
        !error.is_object_gone(),
        "nothing was removed; the daemon was never there: {error}",
    );

    // Which question was being asked is what these primitives are for.
    assert!(!server.is_alive().await);
    assert!(server.check_alive().await.is_err());

    server.shutdown().await.expect("executor shuts down");
}

#[tokio::test]
async fn from_env_reads_only_the_socket_path_out_of_the_tmux_triple() {
    // tmux exports `<socket_path>,<server_pid>,<session_id>`. The pid and the
    // session id are frozen when a pane spawns, so only the path is used.
    let server = Server::from_env_value(Some("/tmp/libtmux-rs-test/from-env.sock,4242,$7"))
        .expect("a well-formed TMUX value resolves");
    assert_eq!(
        server.socket_path(),
        std::path::Path::new("/tmp/libtmux-rs-test/from-env.sock"),
    );

    for rejected in ["", ",4242,$7", "no-commas-at-all"] {
        assert!(
            Server::from_env_value(Some(rejected)).is_err(),
            "{rejected:?} is not tmux's triple",
        );
    }

    assert!(
        Server::from_env_value(None::<String>).is_err(),
        "a process outside tmux has no TMUX value",
    );
}

#[tokio::test]
async fn attached_sessions_selects_only_sessions_with_clients() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    new_session(server, "solo").await;

    // Nothing is attached in a headless fixture, so the filtered listing is
    // empty while the unfiltered one is not.
    assert_eq!(server.sessions().await.expect("sessions").len(), 1);
    assert!(
        server
            .attached_sessions()
            .await
            .expect("attached sessions")
            .is_empty(),
    );
    assert!(server.attached_sessions_or_empty().await.is_empty());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn lookups_find_one_object_by_name_id_or_index() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let alpha = server.new_session("alpha").await.expect("session");
    server.new_session("beta").await.expect("session");
    alpha
        .new_window(NewWindowOptions::new("editor").command("sleep 300"))
        .await
        .expect("window");

    // By name, byte-exact.
    let found = server
        .session("beta")
        .await
        .expect("lookup")
        .expect("beta exists");
    assert_eq!(found.name().as_bytes().to_vec(), b"beta".to_vec());
    assert!(server.session("gamma").await.expect("lookup").is_none());
    assert!(
        server.session("bet").await.expect("lookup").is_none(),
        "matching is exact rather than a prefix",
    );

    // By id.
    let by_id = server
        .session_by_id(alpha.id())
        .await
        .expect("lookup")
        .expect("alpha exists");
    assert_eq!(by_id, alpha);

    // Scoped to the parent, by name and by index.
    let editor = alpha
        .window("editor")
        .await
        .expect("lookup")
        .expect("editor exists");
    assert_eq!(editor.name().as_bytes().to_vec(), b"editor".to_vec());
    let at_index = alpha
        .window_at(editor.index())
        .await
        .expect("lookup")
        .expect("the index is occupied");
    assert_eq!(at_index, editor);
    assert!(alpha.window_at(999).await.expect("lookup").is_none());

    // A window in another session is not found through this one.
    assert!(
        alpha
            .window("nothing-here")
            .await
            .expect("lookup")
            .is_none()
    );

    let pane = editor
        .pane_at(0)
        .await
        .expect("lookup")
        .expect("a window always has a pane at its first index");
    assert_eq!(pane.window_id(), editor.id());
    assert_eq!(
        server
            .pane_by_id(pane.id())
            .await
            .expect("lookup")
            .expect("the pane exists"),
        pane,
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_name_that_would_corrupt_a_tmux_filter_is_still_found() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // Looking up by building a tmux `-f` predicate around the name would let
    // these change the predicate's meaning. Comparing bytes here does not.
    //
    // `#` is absent from this list because tmux expands a format in a session
    // name as it is created, so `has#{format}` is stored as `has` and never
    // reaches a lookup. Braces and commas do survive.
    for name in ["has}brace", "has,comma"] {
        server.new_session(name).await.expect("session");
        let found = server
            .session(name)
            .await
            .expect("lookup")
            .unwrap_or_else(|| panic!("{name} is found"));
        assert_eq!(found.name().as_bytes().to_vec(), name.as_bytes().to_vec());
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A field tmux left empty must not fail the listing that carries it.
///
/// tmux allowed empty window and session names in 3.7a; below that
/// `check_name` refuses one, so no server can hold one. An empty start
/// directory needs no such gate and reaches `session_path` on every supported
/// release. Both were declared `Required` in the format catalog, which turns
/// an empty field into a decode error, and a decode error fails the whole
/// listing rather than the row that produced it: one such session took the
/// answer away from every caller of every session listing, including the
/// lookups that would have found it to kill it.
#[tokio::test]
async fn real_tmux_compat_an_empty_field_does_not_fail_the_listing() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let keep = server.new_session("keep").await.expect("a named session");
    let empty_names = server
        .capabilities()
        .await
        .expect("capabilities")
        .tmux_version()
        .meets(&libtmux::since::EMPTY_OBJECT_NAMES);

    let mut expected = 1;
    if empty_names {
        let unnamed = server.new_session("").await.expect("an empty name");
        assert!(unnamed.name().as_bytes().is_empty());
        expected += 1;
    } else {
        let refused = server.new_session("").await;
        assert!(
            refused.is_err(),
            "below {} tmux refuses the name rather than storing it: {refused:?}",
            libtmux::since::EMPTY_OBJECT_NAMES,
        );
    }

    let rootless = server
        .new_session(NewSessionOptions::new("rootless").start_directory(""))
        .await
        .expect("an empty start directory");
    assert!(rootless.path().as_bytes().is_empty());
    expected += 1;

    assert_eq!(
        server.sessions().await.expect("every row decodes").len(),
        expected,
    );
    assert_eq!(
        server.hierarchy().await.expect("the whole tree").len(),
        expected,
    );
    assert!(server.has_session("keep").await.expect("liveness"));
    assert_eq!(
        keep.refreshed()
            .await
            .expect("a healthy handle refreshes")
            .name()
            .as_bytes(),
        b"keep",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn one_hierarchy_fetch_matches_walking_down() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    for name in ["alpha", "beta"] {
        let session = server.new_session(name).await.expect("session");
        session
            .new_window(NewWindowOptions::new("second").command("sleep 300"))
            .await
            .expect("window");
    }
    let first = server
        .session("alpha")
        .await
        .expect("lookup")
        .expect("alpha exists");
    first
        .window_at(first.windows().await.expect("windows")[0].index())
        .await
        .expect("lookup")
        .expect("the window exists")
        .split(SplitOptions::new(SplitDirection::Below).command("sleep 300"))
        .await
        .expect("pane");

    // Walking down and fetching at once must describe the same server.
    let tree = server.hierarchy().await.expect("hierarchy");
    let walked: Vec<_> = {
        let mut walked = Vec::new();
        for session in server.sessions().await.expect("sessions") {
            let mut windows = Vec::new();
            for window in session.windows().await.expect("windows") {
                let panes = window.panes().await.expect("panes");
                windows.push((window, panes));
            }
            walked.push((session, windows));
        }
        walked
    };

    assert_eq!(tree.len(), walked.len());
    for (branch, (session, windows)) in tree.iter().zip(&walked) {
        assert_eq!(&branch.session, session);
        assert_eq!(branch.windows.len(), windows.len());
        for (built, (window, panes)) in branch.windows.iter().zip(windows) {
            assert_eq!(&built.window, window);
            assert_eq!(built.panes.len(), panes.len());
            assert_eq!(built.panes.first(), panes.first());
        }
    }

    // The shape is what the test built: two sessions, two windows each, and a
    // split in the first session's first window.
    assert_eq!(tree.len(), 2);
    assert_eq!(tree[0].windows.len(), 2);
    assert_eq!(tree[0].windows[0].panes.len(), 2);
    assert_eq!(tree[1].windows[0].panes.len(), 1);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_linked_window_appears_under_every_session_that_links_it() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let origin = server.new_session("origin").await.expect("session");
    server.new_session("borrower").await.expect("session");
    let shared = origin
        .new_window(NewWindowOptions::new("shared").command("sleep 300"))
        .await
        .expect("window");

    run(
        server,
        Command::new("link-window")
            .arg("-s")
            .arg(shared.id().to_string())
            .arg("-t")
            .arg("borrower:9"),
    )
    .await;

    // Stitching is by winlink, so a linked window is under both sessions with
    // the same panes rather than being assigned to one of them.
    let tree = server.hierarchy().await.expect("hierarchy");
    let holders: Vec<_> = tree
        .iter()
        .filter(|branch| {
            branch
                .windows
                .iter()
                .any(|built| built.window.id() == shared.id())
        })
        .collect();
    assert_eq!(holders.len(), 2);
    for holder in holders {
        let built = holder
            .windows
            .iter()
            .find(|built| built.window.id() == shared.id())
            .expect("the linked window is present");
        assert_eq!(built.panes.len(), 1);
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_process_inside_tmux_can_find_where_it_is() {
    use libtmux::{Pane, Session, Window};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("located").await.expect("session");
    let window = session
        .new_window(NewWindowOptions::new("second").command("sleep 300"))
        .await
        .expect("window");
    let pane = window.panes().await.expect("panes").remove(0);

    // tmux exports TMUX_PANE to every process it starts. Reading it back is
    // how a program answers "where am I" without being told.
    let reported = server
        .format(Some(&pane), "#{pane_id}")
        .await
        .expect("a format expands");
    let value = std::ffi::OsString::from(reported.to_string_lossy().into_owned());

    assert_eq!(
        Pane::from_env_value(server, Some(&value))
            .await
            .expect("the lookup runs")
            .expect("the pane exists")
            .id(),
        pane.id(),
    );
    assert_eq!(
        Window::from_env_value(server, Some(&value))
            .await
            .expect("the lookup runs")
            .expect("the window exists")
            .id(),
        window.id(),
    );
    assert_eq!(
        Session::from_env_value(server, Some(&value))
            .await
            .expect("the lookup runs")
            .expect("the session exists")
            .id(),
        session.id(),
    );

    // A pane that has gone is absent rather than an error: the value can
    // outlive what it names.
    let gone = pane.id().clone();
    window.kill().await.expect("the window is killed");
    assert!(
        Pane::from_env_value(server, Some(gone.as_ref()))
            .await
            .expect("the lookup runs")
            .is_none(),
    );

    // Not being inside tmux at all is a different thing, and says so.
    assert!(
        Pane::from_env_value(server, None::<&str>).await.is_err(),
        "an absent TMUX_PANE is a configuration error, not an empty result",
    );
    assert!(
        Pane::from_env_value(server, Some("not-a-pane-id"))
            .await
            .is_err(),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn linked_sessions_survive_a_session_name_that_looks_like_a_list() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let first = server.new_session("first").await.expect("first session");
    // tmux reports `#{window_linked_sessions_list}` as comma-separated names,
    // so this name makes that field read exactly like two more sessions.
    // Reading the winlink rows instead is what survives it.
    let awkward = server
        .new_session("has,comma")
        .await
        .expect("a session whose name holds a comma");
    let window = first
        .active_window()
        .await
        .expect("active window")
        .expect("a session has a window");

    window
        .link_to(&awkward, None)
        .await
        .expect("the window links into both");

    let linked = window.linked_sessions().await.expect("linked sessions");
    let mut names: Vec<String> = linked
        .iter()
        .map(|session| String::from_utf8_lossy(session.name().as_bytes()).into_owned())
        .collect();
    names.sort();

    assert_eq!(
        names,
        ["first", "has,comma"],
        "two sessions, not the three the name list would suggest",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn navigation_reports_nowhere_to_go_as_absence_not_failure() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("moving").await.expect("session");

    // tmux reports "no next window" as a command failure, but a session
    // holding one window has no next window and never will. Treating that as
    // an error would make a caller read stderr to tell it from tmux breaking.
    assert!(session.next_window().await.expect("no failure").is_none());
    assert!(
        session
            .previous_window()
            .await
            .expect("no failure")
            .is_none()
    );
    assert!(session.last_window().await.expect("no failure").is_none());

    let second = session
        .new_window(NewWindowOptions::new("second"))
        .await
        .expect("second window");

    let moved = session
        .next_window()
        .await
        .expect("no failure")
        .expect("there is somewhere to go now");
    assert_eq!(
        moved.id(),
        second.id(),
        "the move reports what became active"
    );

    // Back where it started, which is what `last-window` means.
    let back = session
        .last_window()
        .await
        .expect("no failure")
        .expect("a previous window exists");
    assert_ne!(back.id(), second.id());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_point_lookup_picks_the_link_the_window_is_current_through() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // The same window, linked into two sessions, current in only one of them.
    // Activity belongs to the link rather than to the window.
    let alpha = server.new_session("alpha").await.expect("alpha");
    let target = alpha
        .active_window()
        .await
        .expect("active window")
        .expect("a session has a window");
    let target_id = target.id().clone();
    alpha
        .new_window(NewWindowOptions::new("other"))
        .await
        .expect("a second window in alpha");

    let beta = server.new_session("beta").await.expect("beta");
    target.link_to(&beta, Some(5)).await.expect("link");

    // Make it current in beta and not in alpha. tmux lists alpha's row first,
    // because it lists by session name, so "the first row" would be the
    // inactive link and would depend on what the sessions are called.
    server
        .cmd(Command::new("select-window").arg("-t").arg("alpha:1"))
        .await
        .expect("alpha selects its other window");
    server
        .cmd(Command::new("select-window").arg("-t").arg("beta:5"))
        .await
        .expect("beta selects the linked window");

    let found = server
        .window_by_id(&target_id)
        .await
        .expect("lookup")
        .expect("the window exists");

    assert_eq!(
        found.index(),
        5,
        "the current link won, not the first listed"
    );
    assert_eq!(
        found.session_id(),
        beta.id(),
        "so the handle is bound to the session the window is current in",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Every public type prints, and the ones holding paths do not print them.
///
/// `Debug` on a public type is a Rust API guideline, and the redaction is this
/// crate's own rule: a caller debugging a connection should not put a socket
/// path into a log by printing the thing that holds it.
#[test]
fn public_types_are_printable_without_disclosing_paths() {
    use libtmux::{DispatchLimits, OutputLimits, Server};

    let builder = Server::builder()
        .socket_path("/tmp/libtmux-rs-test/debug-redaction.sock")
        .output_limits(OutputLimits::default())
        .dispatch_limits(DispatchLimits::default());

    let printed = format!("{builder:?}");
    assert!(
        printed.contains("ServerBuilder"),
        "it names itself: {printed}"
    );
    assert!(
        !printed.contains("debug-redaction"),
        "the socket path is redacted: {printed}",
    );

    // The generated field handles print their own name rather than a page of
    // zero-sized markers. They only exist with `query`, and so does their
    // `Debug`, so the assertion is gated the same way.
    #[cfg(feature = "query")]
    {
        use libtmux::Pane;
        use libtmux::query::Filterable as _;

        let fields = format!("{:?}", Pane::filter_fields());
        assert!(fields.starts_with("PaneFields"), "got {fields}");
    }
}

#[tokio::test]
async fn handle_equality_and_hashing_separate_two_servers() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn digest<T: Hash>(value: &T) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    let first = TestServer::builder().start().await.expect("tmux starts");
    let second = TestServer::builder().start().await.expect("tmux starts");

    // The same name on two servers, which is what makes the ids collide: each
    // server issues them from zero, so both sessions are `$0`.
    new_session(first.server(), "work").await;
    new_session(second.server(), "work").await;

    let left = first.server().sessions().await.expect("one session");
    let right = second.server().sessions().await.expect("one session");
    let (left, right) = (&left[0], &right[0]);
    assert_eq!(
        left.id(),
        right.id(),
        "the ids collide, or this proves nothing"
    );

    assert_ne!(left, right, "equality has to separate the two servers");
    assert_ne!(digest(left), digest(right), "and so does hashing");

    // Two handles to one object stay equal, and hash alike, so either can key
    // a map.
    let again = first.server().sessions().await.expect("one session");
    assert_eq!(left, &again[0]);
    assert_eq!(digest(left), digest(&again[0]));

    let windows = first.server().windows().await.expect("one window");
    let other_windows = second.server().windows().await.expect("one window");
    assert_ne!(&windows[0], &other_windows[0]);
    assert_ne!(digest(&windows[0]), digest(&other_windows[0]));

    let panes = first.server().panes().await.expect("one pane");
    let other_panes = second.server().panes().await.expect("one pane");
    assert_ne!(&panes[0], &other_panes[0]);
    assert_ne!(digest(&panes[0]), digest(&other_panes[0]));

    first.shutdown().await.expect("first server stops");
    second.shutdown().await.expect("second server stops");
}
