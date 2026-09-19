//! What the hierarchy costs, counted rather than asserted in prose.

#![cfg(all(feature = "test-support", feature = "tracing"))]
// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use libtmux::test::TestServer;
use libtmux::{NewWindowOptions, SplitDirection, SplitOptions};
use tracing::subscriber::Subscriber;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;

/// Counts the tmux commands the crate issues.
///
/// The crate reports one event per command it runs, so this is what tmux
/// actually saw -- not a model of it that could drift.
#[derive(Clone, Default)]
struct CommandCounter {
    count: Arc<AtomicUsize>,
}

impl CommandCounter {
    fn commands(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }

    fn reset(&self) {
        self.count.store(0, Ordering::Relaxed);
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for CommandCounter {
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        struct Message<'a>(&'a mut bool);

        impl tracing::field::Visit for Message<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" && format!("{value:?}").contains("requested") {
                    *self.0 = true;
                }
            }
        }

        let mut requested = false;
        event.record(&mut Message(&mut requested));
        if requested {
            self.count.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Add `sessions` sessions of `windows` windows, each holding two panes.
async fn populate(server: &libtmux::Server, prefix: &str, sessions: usize, windows: usize) {
    for session in 0..sessions {
        let session = server
            .new_session(format!("{prefix}-{session}").as_str())
            .await
            .expect("session");

        for window in 0..windows {
            let created = session
                .new_window(
                    NewWindowOptions::new(format!("window-{window}").as_str()).command("sleep 300"),
                )
                .await
                .expect("window");
            created
                .split(SplitOptions::new(SplitDirection::Below).command("sleep 300"))
                .await
                .expect("pane");
        }
    }
}

#[tokio::test]
async fn the_hierarchy_costs_the_same_however_large_it_is() {
    let counter = CommandCounter::default();
    let subscriber = tracing_subscriber::registry().with(counter.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // One session, one window: the smallest hierarchy there is.
    populate(server, "small", 1, 1).await;
    counter.reset();
    let small = server.hierarchy().await.expect("hierarchy");
    let small_commands = counter.commands();

    // Sixteen times the objects.
    populate(server, "large", 3, 5).await;
    counter.reset();
    let large = server.hierarchy().await.expect("hierarchy");
    let large_commands = counter.commands();

    assert!(large.len() > small.len(), "the second hierarchy is larger");
    assert!(
        large.iter().map(|tree| tree.windows.len()).sum::<usize>() > 10,
        "and larger by enough for a per-object cost to show",
    );

    // Three listings, whatever the server holds. A walk down the tree would
    // cost one command per session and one per window, so this is the
    // difference between a constant and a hierarchy-sized bill.
    assert_eq!(small_commands, 3, "sessions, windows, and panes");
    assert_eq!(
        large_commands, small_commands,
        "the cost does not grow with the hierarchy",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn walking_down_costs_a_command_for_every_step() {
    let counter = CommandCounter::default();
    let subscriber = tracing_subscriber::registry().with(counter.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    populate(server, "walked", 2, 3).await;

    // The same information, gathered the obvious way. This is not a strawman:
    // it is what the traversal API does, and it is the right shape when a
    // caller wants one branch rather than the whole tree.
    counter.reset();
    let sessions = server.sessions().await.expect("sessions");
    let mut windows = 0;
    for session in &sessions {
        for window in session.windows().await.expect("windows") {
            windows += 1;
            let _ = window.panes().await.expect("panes");
        }
    }
    let walked = counter.commands();

    counter.reset();
    server.hierarchy().await.expect("hierarchy");
    let gathered = counter.commands();

    assert_eq!(
        walked,
        1 + sessions.len() + windows,
        "one listing per session and per window, plus the first",
    );
    assert_eq!(gathered, 3);
    assert!(
        walked > gathered * 3,
        "walking cost {walked} commands where the hierarchy cost {gathered}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// What it costs to ask a client what it is attached to.
///
/// Three accessors, one command each. Each used to cost two: a
/// `display-message` for the id, then a listing filtered down to that one id.
/// tmux fills a client's session, that session's current window and that
/// window's active pane into the same format tree, so the whole snapshot comes
/// back in the first round trip and the second was never needed.
#[cfg(feature = "control-mode")]
#[tokio::test]
async fn asking_a_client_what_it_is_attached_to_costs_one_command_each() {
    let counter = CommandCounter::default();
    let subscriber = tracing_subscriber::registry().with(counter.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("attached").await.expect("session");

    // A control-mode connection is a client, so the server has one to ask.
    let control = libtmux::control::ControlMode::attach(server, session.id())
        .await
        .expect("a control client");
    // Also warms the version probe, which is a command of its own the first
    // time anything asks for it.
    let client = server
        .clients()
        .await
        .expect("clients")
        .into_iter()
        .next()
        .expect("one client");

    counter.reset();
    let attached = client.attached_session().await.expect("a session");
    let session_commands = counter.commands();

    counter.reset();
    let window = client.attached_window().await.expect("a window");
    let window_commands = counter.commands();

    counter.reset();
    let pane = client.attached_pane().await.expect("a pane");
    let pane_commands = counter.commands();

    // The answer is the point. A cheaper call that returns the wrong object is
    // not cheaper, so the count is asserted next to what came back.
    assert_eq!(attached.expect("a session").id(), session.id());
    assert!(window.is_some(), "the session has a current window");
    assert!(pane.is_some(), "that window has an active pane");

    assert_eq!(
        session_commands, 1,
        "one display-message, no second listing"
    );
    assert_eq!(window_commands, 1, "the same for the window");
    assert_eq!(pane_commands, 1, "and for the pane");

    control.shutdown().await.expect("control shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// What it costs to read a field a pane listing already carries.
///
/// These four arrive with every pane listing, so reading them sends nothing,
/// and each agrees with what `display-message` reports for the same pane: an
/// empty expansion where the read says `Absent`.
#[cfg(feature = "query")]
#[tokio::test]
async fn reading_a_listed_pane_field_costs_no_command() {
    use libtmux::query::Filterable as _;
    use libtmux::{Availability, Command, NewSessionOptions, Pane, PaneWait};

    let counter = CommandCounter::default();
    let subscriber = tracing_subscriber::registry().with(counter.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    // History to scroll back through, then a cursor off the origin, then a
    // reader that prints nothing more, so the cursor holds still.
    let session = server
        .new_session(NewSessionOptions::new("read").command("seq 1 200; printf abc; exec cat"))
        .await
        .expect("session");
    let mut pane = session.panes().await.expect("panes").remove(0);
    let arrived = pane
        .wait_for_text("abc", std::time::Duration::from_secs(10))
        .await
        .expect("wait");
    assert_eq!(arrived, PaneWait::Arrived);
    let fields = Pane::filter_fields();

    for scrolled in [None, Some(5)] {
        if let Some(lines) = scrolled {
            pane.copy_mode().await.expect("copy mode");
            pane.cmd(
                Command::new("send-keys")
                    .arg("-X")
                    .arg("-N")
                    .arg(lines.to_string())
                    .arg("scroll-up"),
            )
            .await
            .expect("scroll");
        }
        pane.refresh().await.expect("listing");

        counter.reset();
        let cursor_x = pane.get(fields.cursor_x);
        let cursor_y = pane.get(fields.cursor_y);
        let pane_mode = pane.get(fields.pane_mode);
        let scroll_position = pane.get(fields.scroll_position);
        assert_eq!(counter.commands(), 0, "a read sends tmux nothing");

        assert_eq!(cursor_x, Availability::Available(3), "after `abc`");
        if let Some(lines) = scrolled {
            assert!(pane_mode.is_available());
            assert_eq!(scroll_position, Availability::Available(lines));
        } else {
            assert_eq!(pane_mode, Availability::Absent);
            assert_eq!(scroll_position, Availability::Absent);
        }

        let reads = [
            ("#{cursor_x}", cursor_x.available().map(|x| x.to_string())),
            ("#{cursor_y}", cursor_y.available().map(|y| y.to_string())),
            (
                "#{pane_mode}",
                pane_mode
                    .available()
                    .map(|mode| mode.to_string_lossy().into_owned()),
            ),
            (
                "#{scroll_position}",
                scroll_position.available().map(|lines| lines.to_string()),
            ),
        ];
        for (format, read) in reads {
            let asked = pane.format(format).await.expect("display-message");
            assert_eq!(
                asked.to_string_lossy(),
                read.unwrap_or_default(),
                "{format} with scroll {scrolled:?}",
            );
        }
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A replacing hook write is one invocation, so no drop can land between the
/// clear and the entries and leave the hook empty.
#[tokio::test]
async fn replacing_hooks_clears_and_writes_in_one_command() {
    use libtmux::{IndexedHooks, ReplaceMode, TmuxText};

    let counter = CommandCounter::default();
    let subscriber = tracing_subscriber::registry().with(counter.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard
        .server()
        .new_session("hooks")
        .await
        .expect("the session is created");
    let mut entries = std::collections::BTreeMap::new();
    entries.insert(0, TmuxText::from(b"display-message one".to_vec()));
    entries.insert(1, TmuxText::from(b"display-message two".to_vec()));
    let written = IndexedHooks::from(entries);
    // The scope check reads before writing; count only the write.
    session
        .set_hooks("alert-bell", &written, ReplaceMode::Replace)
        .await
        .expect("the hooks are written");

    counter.reset();
    session
        .set_hooks("alert-bell", &written, ReplaceMode::Replace)
        .await
        .expect("the hooks are written");

    assert_eq!(
        counter.commands(),
        1,
        "the clear and both entries travel together",
    );
    let read = session
        .hook("alert-bell")
        .await
        .expect("the hook reads")
        .expect("the hook is set");
    assert_eq!(read.len(), 2);
}
