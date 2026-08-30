//! What the lenient listings do with the failure they discard.

#![cfg(all(feature = "test-support", feature = "tracing", feature = "query"))]

use std::sync::Arc;
use std::sync::Mutex;

use libtmux::test::TestServer;
use tracing::subscriber::Subscriber;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;

/// Collects the `list_command` of every discard the crate records.
#[derive(Clone, Default)]
struct Discards {
    seen: Arc<Mutex<Vec<String>>>,
}

impl Discards {
    fn seen(&self) -> Vec<String> {
        self.seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Discards {
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        struct Fields<'a> {
            discarded: &'a mut bool,
            command: &'a mut Option<String>,
        }

        impl tracing::field::Visit for Fields<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                let rendered = format!("{value:?}");
                match field.name() {
                    "message" if rendered.contains("lenient listing discarded") => {
                        *self.discarded = true;
                    }
                    "list_command" => *self.command = Some(rendered.trim_matches('"').to_owned()),
                    _ => {}
                }
            }

            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "list_command" {
                    *self.command = Some(value.to_owned());
                }
            }
        }

        let mut discarded = false;
        let mut command = None;
        event.record(&mut Fields {
            discarded: &mut discarded,
            command: &mut command,
        });
        // Written as two ifs rather than a let-chain: this crate's floor is
        // 1.85 and let-chains landed in 1.88. `tmux-mcp` uses them because its
        // own floor is 1.88.
        if discarded {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(command.unwrap_or_else(|| "unnamed".to_owned()));
            }
        }
    }
}

/// Every lenient listing must record the failure it throws away.
///
/// The empty vector these return means "nothing there" and "the listing
/// failed" alike, which is the trade a caller chooses them for. What a caller
/// cannot then do is tell the two apart afterwards, so the discard has to
/// reach a log or it reaches nowhere.
///
/// Five of the eleven did. The other six could not: the helper was a private
/// associated function on `Server`, so only that file's listings could call
/// it, and the split followed where the helper sat rather than any decision.
#[tokio::test]
async fn every_lenient_listing_records_what_it_discarded() {
    let discards = Discards::default();
    let subscriber = tracing_subscriber::registry().with(discards.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server().clone();
    let server = &server;
    let session = server.new_session("lenient").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a session has a window");

    // Positive control: every listing answers while the server is up, so a
    // later empty is a discarded failure rather than a method that never
    // worked.
    assert!(!server.sessions_or_empty().await.is_empty());
    assert!(!session.windows_or_empty().await.is_empty());
    assert!(!window.panes_or_empty().await.is_empty());
    assert!(
        discards.seen().is_empty(),
        "a healthy server discards nothing"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");

    // Now every listing fails, and each must say so.
    let matcher = {
        use libtmux::query::Filterable as _;
        libtmux::Pane::filter_fields().pane_id.eq("%0")
    };
    let windows_matcher = {
        use libtmux::query::Filterable as _;
        libtmux::Window::filter_fields().window_id.eq("@0")
    };

    assert!(server.sessions_or_empty().await.is_empty());
    assert!(server.windows_or_empty().await.is_empty());
    assert!(server.panes_or_empty().await.is_empty());
    assert!(server.clients_or_empty().await.is_empty());
    assert!(server.attached_sessions_or_empty().await.is_empty());
    assert!(session.windows_or_empty().await.is_empty());
    assert!(session.panes_or_empty().await.is_empty());
    assert!(
        session
            .search_windows_or_empty(windows_matcher)
            .await
            .is_empty()
    );
    assert!(window.panes_or_empty().await.is_empty());
    assert!(window.search_panes_or_empty(matcher).await.is_empty());
    assert!(window.linked_sessions_or_empty().await.is_empty());

    let seen = discards.seen();
    assert_eq!(
        seen.len(),
        11,
        "all eleven lenient listings record their discard, saw: {seen:?}"
    );
}
