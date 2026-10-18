//! The span each dispatch runs in, on both transports.

#![cfg(all(
    feature = "control-mode",
    feature = "test-support",
    feature = "tracing"
))]
// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::os::unix::ffi::OsStringExt as _;
use std::sync::{Arc, Mutex};

use libtmux::control::ControlMode;
use libtmux::test::TestServer;
use libtmux::{Command, ErrorKind, Server};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Subscriber;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;

const SECRET: &str = "sentinel-span-secret";

#[derive(Debug, Default)]
struct Fields(BTreeMap<&'static str, String>);

impl Fields {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
}

impl tracing::field::Visit for Fields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name(), value.to_owned());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        self.0.insert(field.name(), format!("{value:?}"));
    }
}

/// One `tmux_command` span and the events recorded inside it.
#[derive(Debug, Default)]
struct Dispatch {
    fields: Fields,
    events: Vec<Fields>,
}

#[derive(Default)]
struct Recorded {
    dispatches: Vec<Dispatch>,
    /// Events with no `tmux_command` span around them.
    orphans: Vec<Fields>,
}

/// Collects dispatch spans by creation order, not by span ID, which the
/// registry reuses once a span closes.
#[derive(Clone, Default)]
struct Collector(Arc<Mutex<Recorded>>);

struct Slot(usize);

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Collector {
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        if attributes.metadata().name() != "tmux_command" {
            return;
        }
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        let mut recorded = self.0.lock().unwrap();
        recorded.dispatches.push(Dispatch {
            fields,
            events: Vec::new(),
        });
        let slot = Slot(recorded.dispatches.len() - 1);
        context.span(id).unwrap().extensions_mut().insert(slot);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, context: Context<'_, S>) {
        let span = context.span(id).unwrap();
        if let Some(Slot(slot)) = span.extensions().get::<Slot>() {
            values.record(&mut self.0.lock().unwrap().dispatches[*slot].fields);
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, context: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let slot = context
            .event_span(event)
            .and_then(|span| span.extensions().get::<Slot>().map(|slot| slot.0));
        let mut recorded = self.0.lock().unwrap();
        match slot {
            Some(slot) => recorded.dispatches[slot].events.push(fields),
            None => recorded.orphans.push(fields),
        }
    }
}

fn set_token(value: impl Into<OsString>) -> Command {
    Command::new("set-environment")
        .arg("-g")
        .arg("LIBTMUX_SPAN_TOKEN")
        .sensitive_arg(value)
}

/// Dispatch four commands, one per outcome, and return the error's kind.
fn dispatch_each_outcome(server: &Server, cancellation: libtmux::CancellationToken) -> ErrorKind {
    let result = server.cmd(set_token(SECRET)).expect("dispatched");
    assert!(result.success(), "{result:?}");

    let refused = Command::new("set-environment")
        .arg("-t")
        .arg("no-such-session-for-spans")
        .arg("LIBTMUX_SPAN_TOKEN")
        .sensitive_arg(SECRET);
    let result = server.cmd(refused).expect("dispatched");
    assert!(!result.success(), "{result:?}");

    // A NUL cannot reach a process and invalid UTF-8 cannot reach a control
    // line, so each transport refuses this before tmux sees it.
    let mut unsendable = SECRET.as_bytes().to_vec();
    unsendable.extend_from_slice(b"\xff\0");
    let error = server
        .cmd(set_token(OsString::from_vec(unsendable)))
        .expect_err("refused before dispatch");

    let cancelled =
        common::with_deadline(cancellation, std::time::Duration::from_millis(100), || {
            server.cmd(Command::new("wait-for").sensitive_arg(SECRET))
        });
    assert!(
        cancelled.is_err(),
        "the blocked dispatch reaches the caller deadline"
    );

    error.kind()
}

#[test]
fn each_dispatch_runs_in_one_span_on_either_transport() {
    let guard = TestServer::new().expect("tmux starts");
    let (server, process_cancellation) = common::cancellable(guard.server());
    let (control_server, control_cancellation) = common::cancellable(guard.server());
    let session = server.new_session("spans").expect("a session");
    // The version probe is a dispatch of its own; take it before collecting.
    server.capabilities().expect("tmux reports its version");
    control_server
        .capabilities()
        .expect("control handle reports its version");
    let (sender, events) = ControlMode::attach(&control_server, session.id())
        .expect("a control client attaches")
        .split();
    let routed = control_server
        .over_control_mode(&sender)
        .expect("a routed handle");

    let collector = Collector::default();
    let subscriber = tracing_subscriber::registry().with(collector.clone());
    let default = tracing::subscriber::set_default(subscriber);
    let process_kind = dispatch_each_outcome(&server, process_cancellation);
    let control_kind = dispatch_each_outcome(&routed, control_cancellation);
    drop(default);
    events.shutdown().expect("control shuts down");
    guard.shutdown().expect("tmux fixture shuts down");

    let recorded = collector.0.lock().unwrap();
    let observed: Vec<_> = recorded
        .dispatches
        .iter()
        .map(|dispatch| {
            let fields = &dispatch.fields;
            (
                fields.get("transport").unwrap(),
                fields.get("subcommand").unwrap(),
                fields.get("outcome"),
                fields.get("error_kind"),
            )
        })
        .collect();
    let process_kind = format!("{process_kind:?}");
    let control_kind = format!("{control_kind:?}");
    let mut expected = Vec::new();
    for (transport, kind) in [("subprocess", &process_kind), ("control", &control_kind)] {
        expected.extend([
            (transport, "set-environment", Some("success"), None),
            (transport, "set-environment", Some("exit"), None),
            (
                transport,
                "set-environment",
                Some("error"),
                Some(kind.as_str()),
            ),
            (transport, "wait-for", Some("error"), Some("Cancelled")),
        ]);
    }
    assert_eq!(observed, expected, "one span per dispatch");

    for dispatch in &recorded.dispatches {
        let request_id = dispatch.fields.get("request_id").unwrap();
        request_id.parse::<u64>().expect("a numeric request ID");
        for event in &dispatch.events {
            assert_eq!(event.get("request_id"), Some(request_id), "{dispatch:?}");
        }
    }

    // The supervisor that reports a finished process runs on its own task.
    for dispatch in &recorded.dispatches[..2] {
        let messages: Vec<_> = dispatch
            .events
            .iter()
            .filter_map(|event| event.get("message"))
            .collect();
        assert_eq!(
            messages,
            ["tmux command requested", "tmux command finished"],
            "{dispatch:?}",
        );
    }
    assert!(
        recorded.orphans.is_empty(),
        "every dispatch event is inside its span: {:?}",
        recorded.orphans,
    );

    for dispatch in &recorded.dispatches {
        let values = dispatch
            .events
            .iter()
            .chain([&dispatch.fields])
            .flat_map(|fields| fields.0.values());
        for value in values {
            assert!(!value.contains(SECRET), "{dispatch:?}");
        }
    }
}
