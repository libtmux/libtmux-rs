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
