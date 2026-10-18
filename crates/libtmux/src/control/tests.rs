#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::too_many_lines
)]

use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::actor::admit_request;
use super::channel::{self, ReplyReceiver, Request, Shared, TryRecvError, lock, reply_channel};
use super::{
    BlockResult, ControlEvents, ControlSender, Delivery, Event, HELD_WHILE_AWAITING, Line,
    PaneOutput, ReplySlot, ReplySlots, decode_watched_pane_id, unescape_output,
};
use crate::{
    Command, ControlModeErrorKind, Error, ErrorKind, PaneId, SessionId, TmuxText, WindowId,
};

fn reply(number: u64) -> BlockResult {
    BlockResult {
        number,
        succeeded: true,
        output: Vec::new(),
        sensitive_input: false,
        failed_lines: Vec::new(),
    }
}

fn request() -> (Request, ReplyReceiver) {
    let (result, answer) = reply_channel();
    (
        Request {
            line: String::new(),
            deadline: None,
            result,
            boundary: None,
        },
        answer,
    )
}

fn refused(mut answer: ReplyReceiver) -> Error {
    answer
        .try_recv()
        .expect("the request is answered")
        .expect_err("the request is refused")
}

#[test]
fn block_refusal_classification_withholds_sensitive_output() {
    assert!(reply(1).refusal_for("display-message").is_none());

    let secret = "sentinel-control-refusal";
    let block = BlockResult {
        number: 2,
        succeeded: false,
        output: vec![TmuxText::from(secret)],
        sensitive_input: true,
        failed_lines: Vec::new(),
    };
    let error = block
        .refusal_for("display-message")
        .expect("an error block is a refusal");
    let diagnostic = format!("{error:?} {error}");
    assert!(matches!(error, Error::CommandFailed { .. }));
    assert!(!diagnostic.contains(secret), "{diagnostic}");
}

#[test]
fn watch_only_rejects_unreadable_pane_ids_without_echoing_them() {
    for line in [
        TmuxText::from("sentinel-not-a-pane-id"),
        TmuxText::from_bytes([0xff]),
    ] {
        let error = decode_watched_pane_id(&line).expect_err("pane id is unreadable");
        let diagnostic = format!("{error:?} {error}");
        assert_eq!(error.kind(), ErrorKind::Decode);
        assert!(!diagnostic.contains("sentinel"), "{diagnostic}");
    }
}

#[test]
fn a_refused_reply_keeps_the_next_reply_aligned() {
    let mut replies = ReplySlots::default();
    let (b_request, b_reply) = request();
    replies.push(b_request.result, None);

    replies.refuse_live();
    let refused = refused(b_reply);
    assert_eq!(
        refused.kind(),
        ErrorKind::Refused,
        "B reports the unread-event cutoff",
    );
    assert!(
        !refused.is_transient(),
        "this command crossed the write boundary before it was refused",
    );
    assert!(!replies.has_live(), "event reading may pause");

    let (c_request, mut c_reply) = request();
    let c_request = admit_request(c_request, HELD_WHILE_AWAITING - 1)
        .expect("C is admitted after the caller drains below the limit");
    replies.push(c_request.result, None);
    assert!(replies.has_live(), "C keeps reply reading unpaused");
    replies.complete(reply(2));
    assert!(
        matches!(c_reply.try_recv(), Err(TryRecvError::Empty)),
        "B's block is discarded rather than answering C",
    );

    replies.complete(reply(3));
    assert_eq!(
        c_reply
            .try_recv()
            .expect("C is answered")
            .expect("C succeeds")
            .number(),
        3,
    );
}

#[test]
fn a_chain_keeps_its_blocks_until_the_completion_fence() {
    fn block(number: u64, succeeded: bool, text: &str) -> BlockResult {
        BlockResult {
            output: vec![TmuxText::from(text)],
            failed_lines: vec![!succeeded],
            succeeded,
            ..reply(number)
        }
    }
    let pending =
        |answer: &mut ReplyReceiver| matches!(answer.try_recv(), Err(TryRecvError::Empty));

    let mut replies = ReplySlots::default();
    let (chain, mut chain_answer) = reply_channel();
    let (next, mut next_answer) = reply_channel();
    let (failing, mut failing_answer) = reply_channel();
    let chain_fence = replies.push(chain, None);
    replies.push(next, None);
    let failing_fence = replies.push(failing, None);

    replies
        .consume(block(1, true, "first"), usize::MAX)
        .expect("first block fits");
    assert!(
        pending(&mut chain_answer),
        "one block of two is not the answer"
    );
    replies
        .consume(block(2, true, "second"), usize::MAX)
        .expect("second block fits");
    assert!(
        pending(&mut chain_answer),
        "blocks wait for their completion fence"
    );
    replies
        .consume(chain_fence.block(), usize::MAX)
        .expect("chain completes");
    let chained = chain_answer
        .try_recv()
        .expect("the chain is answered")
        .expect("both commands succeeded");
    assert_eq!(
        chained.output(),
        [TmuxText::from("first"), TmuxText::from("second")]
    );
    assert!(pending(&mut next_answer), "the chain's blocks stay its own");

    replies.complete(reply(3));
    assert_eq!(
        next_answer
            .try_recv()
            .expect("the next caller is answered")
            .expect("it succeeded")
            .number(),
        3,
    );

    replies
        .consume(block(4, true, "printed"), usize::MAX)
        .expect("success fits");
    replies
        .consume(block(5, false, "refused"), usize::MAX)
        .expect("error fits");
    assert!(
        pending(&mut failing_answer),
        "failure still waits for its fence"
    );
    replies
        .consume(failing_fence.block(), usize::MAX)
        .expect("failed chain completes");
    let failed = failing_answer
        .try_recv()
        .expect("a failure ends the chain early")
        .expect("a refusal is a result");
    assert!(!failed.succeeded());
    assert_eq!(
        {
            let (stdout, stderr) = failed.split_by_outcome();
            (
                stdout.cloned().collect::<Vec<_>>(),
                stderr.cloned().collect::<Vec<_>>(),
            )
        },
        (
            vec![TmuxText::from("printed")],
            vec![TmuxText::from("refused")]
        ),
        "what ran before the failure is kept apart from the refusal",
    );
    assert!(
        replies.slots.is_empty(),
        "no slot waits for a skipped command"
    );
}

#[test]
fn reply_deadline_is_the_earliest_pending_deadline() {
    let now = Instant::now();
    let earlier = now + Duration::from_secs(1);
    let later = now + Duration::from_secs(2);
    let mut replies = ReplySlots::default();
    let (first, _first_answer) = reply_channel();
    let (second, _second_answer) = reply_channel();

    replies.push(first, later.into());
    replies.push(second, earlier.into());
    assert_eq!(replies.earliest_deadline(), Some(earlier));
    replies.complete(reply(1));
    assert_eq!(replies.earliest_deadline(), Some(earlier));
    replies.complete(reply(2));
    assert_eq!(replies.earliest_deadline(), None);

    let (first, _first_answer) = reply_channel();
    let (second, _second_answer) = reply_channel();
    replies.push(first, earlier.into());
    replies.push(second, later.into());
    replies.complete(reply(3));
    assert_eq!(replies.earliest_deadline(), Some(later));
}

#[test]
fn retries_at_the_unread_limit_do_not_grow_reply_slots() {
    let mut replies = ReplySlots::default();
    let (in_flight, _answer) = request();
    replies.push(in_flight.result, None);
    replies.refuse_live();
    let slots_at_cutoff = replies.slots.len();

    for _ in 0..64 {
        let (retry, answer) = request();
        assert!(
            admit_request(retry, HELD_WHILE_AWAITING).is_none(),
            "the retry does not cross the write boundary",
        );
        let error = refused(answer);
        assert_eq!(
            error.kind(),
            ErrorKind::Refused,
            "the retry reports the unread-event cutoff",
        );
        assert!(
            !error.is_transient(),
            "the kind also covers live requests that were already written",
        );
    }

    assert_eq!(
        replies.slots.len(),
        slots_at_cutoff,
        "retries refused before writing need no reply tombstones",
    );
}

#[test]
fn block_headers_correlate_by_the_number_tmux_assigns() {
    assert_eq!(
        Line::parse(b"%begin 1786582374 347 0"),
        Line::BlockStart {
            number: 347,
            flags: 0
        }
    );
    assert_eq!(
        Line::parse(b"%end 1786582374 347 0"),
        Line::BlockEnd {
            number: 347,
            succeeded: true,
        },
    );
    assert_eq!(
        Line::parse(b"%error 1786582374 353 1"),
        Line::BlockEnd {
            number: 353,
            succeeded: false,
        },
    );

    // A header without a usable number is text. Guessing one would
    // correlate a result with the wrong command.
    assert!(matches!(Line::parse(b"%begin bad"), Line::Text(_)));
}

fn event(line: &[u8]) -> Event {
    match Line::parse(line) {
        Line::Event(event) => event,
        other => panic!("{other:?} is not an event"),
    }
}

fn a_session() -> SessionId {
    "$0".parse().expect("a session id parses")
}

fn a_window() -> WindowId {
    "@2".parse().expect("a window id parses")
}

fn a_pane() -> PaneId {
    "%3".parse().expect("a pane id parses")
}

#[test]
fn session_notifications_are_parsed() {
    assert_eq!(
        event(b"%session-changed $0 work"),
        Event::SessionChanged {
            session: a_session(),
        },
    );
    assert_eq!(
        event(b"%session-renamed $0 renamed"),
        Event::SessionRenamed {
            session: a_session(),
            name: TmuxText::from_bytes(*b"renamed"),
        },
    );
    assert_eq!(
        event(b"%session-window-changed $0 @2"),
        Event::SessionWindowChanged {
            session: a_session(),
            window: a_window(),
        },
    );
    assert_eq!(event(b"%sessions-changed"), Event::SessionsChanged);
}

#[test]
fn window_notifications_are_parsed() {
    assert_eq!(
        event(b"%window-add @2"),
        Event::WindowAdded { window: a_window() },
    );
    assert_eq!(
        event(b"%window-close @2"),
        Event::WindowClosed { window: a_window() },
    );
    assert_eq!(
        event(b"%window-renamed @2 build"),
        Event::WindowRenamed {
            window: a_window(),
            name: TmuxText::from_bytes(*b"build"),
        },
    );
    assert_eq!(
        event(b"%window-pane-changed @2 %3"),
        Event::WindowPaneChanged {
            window: a_window(),
            pane: a_pane(),
        },
    );
    assert_eq!(
        event(b"%unlinked-window-add @2"),
        Event::UnlinkedWindowAdded { window: a_window() },
    );
    assert_eq!(
        event(b"%unlinked-window-close @2"),
        Event::UnlinkedWindowClosed { window: a_window() },
    );
    assert_eq!(
        event(b"%unlinked-window-renamed @2 build"),
        Event::UnlinkedWindowRenamed {
            window: a_window(),
            name: TmuxText::from_bytes(*b"build"),
        },
    );
}

#[test]
fn a_layout_change_is_parsed() {
    assert_eq!(
        event(b"%layout-change @2 bc62,80x24,0,0,0 bc62,80x24,0,0,0 *"),
        Event::LayoutChanged {
            window: a_window(),
            layout: TmuxText::from_bytes(*b"bc62,80x24,0,0,0"),
            visible_layout: TmuxText::from_bytes(*b"bc62,80x24,0,0,0"),
            flags: TmuxText::from_bytes(*b"*"),
        },
    );
}

#[test]
fn output_and_flow_control_notifications_are_parsed() {
    assert_eq!(
        event(b"%output %3 hi"),
        Event::Output {
            pane: a_pane(),
            bytes: b"hi".to_vec(),
        },
    );
    assert_eq!(
        event(b"%extended-output %3 1500 : hi"),
        Event::ExtendedOutput {
            pane: a_pane(),
            age: Duration::from_millis(1500),
            bytes: b"hi".to_vec(),
        },
    );
    assert_eq!(event(b"%pause %3"), Event::Paused { pane: a_pane() });
    assert_eq!(event(b"%continue %3"), Event::Continued { pane: a_pane() });
    assert_eq!(
        event(b"%pane-mode-changed %3"),
        Event::PaneModeChanged { pane: a_pane() },
    );
}

#[test]
fn client_buffer_and_server_notifications_are_parsed() {
    assert_eq!(
        event(b"%client-detached /dev/pts/4"),
        Event::ClientDetached {
            client: TmuxText::from_bytes(*b"/dev/pts/4"),
        },
    );
    assert_eq!(
        event(b"%client-session-changed /dev/pts/4 $0 work"),
        Event::ClientSessionChanged {
            client: TmuxText::from_bytes(*b"/dev/pts/4"),
            session: a_session(),
            name: TmuxText::from_bytes(*b"work"),
        },
    );
    assert_eq!(
        event(b"%paste-buffer-changed buffer0"),
        Event::PasteBufferChanged {
            name: TmuxText::from_bytes(*b"buffer0"),
        },
    );
    assert_eq!(
        event(b"%paste-buffer-deleted buffer0"),
        Event::PasteBufferDeleted {
            name: TmuxText::from_bytes(*b"buffer0"),
        },
    );
    assert_eq!(
        event(b"%config-error /etc/tmux.conf:3: unknown command"),
        Event::ConfigError {
            message: TmuxText::from_bytes(*b"/etc/tmux.conf:3: unknown command"),
        },
    );
    assert_eq!(
        event(b"%message hello"),
        Event::Message {
            message: TmuxText::from_bytes(*b"hello"),
        },
    );
    assert_eq!(event(b"%exit"), Event::Exit { reason: None });
    assert_eq!(
        event(b"%exit too far behind"),
        Event::Exit {
            reason: Some(TmuxText::from_bytes(*b"too far behind")),
        },
    );
}

#[test]
fn a_subscription_change_is_parsed_with_and_without_its_optional_fields() {
    assert_eq!(
        event(b"%subscription-changed watched $0 @2 7 %3 : value"),
        Event::SubscriptionChanged {
            name: TmuxText::from_bytes(*b"watched"),
            session: a_session(),
            window: Some(a_window()),
            index: Some(7),
            pane: Some(a_pane()),
            value: TmuxText::from_bytes(*b"value"),
        },
    );
    assert_eq!(
        event(b"%subscription-changed watched $0 - - - : value"),
        Event::SubscriptionChanged {
            name: TmuxText::from_bytes(*b"watched"),
            session: a_session(),
            window: None,
            index: None,
            pane: None,
            value: TmuxText::from_bytes(*b"value"),
        },
    );
}

#[test]
fn an_unmodelled_notification_is_kept() {
    assert_eq!(
        event(b"%invented-later @2 build"),
        Event::Other {
            name: "invented-later".to_owned(),
            rest: TmuxText::from_bytes(*b"@2 build"),
        },
    );
}

#[test]
fn a_block_line_that_looks_like_a_notification_is_output() {
    assert_eq!(
        Line::parse_within_block(b"%0", 12),
        Line::Text(TmuxText::from_bytes(*b"%0")),
    );
    assert_eq!(
        Line::parse_within_block(b"%output %3 hi", 12),
        Line::Text(TmuxText::from_bytes(*b"%output %3 hi")),
    );

    // The block's own terminator is the one line that is still structure.
    assert_eq!(
        Line::parse_within_block(b"%end 1786582374 12 0", 12),
        Line::BlockEnd {
            number: 12,
            succeeded: true,
        },
    );
    // Another block's terminator is not this block's, so it is output.
    assert_eq!(
        Line::parse_within_block(b"%end 1786582374 13 0", 12),
        Line::Text(TmuxText::from_bytes(*b"%end 1786582374 13 0")),
    );
}

#[test]
fn a_malformed_notification_is_text_rather_than_a_guess() {
    let cases: [&[u8]; 5] = [
        b"%window-add nonsense",
        b"%pause nonsense",
        b"%extended-output %3 notanumber : hi",
        b"%session-window-changed $0 nonsense",
        b"%begin bad",
    ];

    for line in cases {
        assert_eq!(
            Line::parse(line),
            Line::Text(TmuxText::from_bytes(line)),
            "{}",
            String::from_utf8_lossy(line),
        );
    }
}

#[test]
fn an_event_says_whether_a_listing_is_now_stale() {
    let stale = |line: &[u8]| match Line::parse(line) {
        Line::Event(event) => event.invalidates_listings(),
        other => panic!("{other:?} is not an event"),
    };

    // Output says nothing about the shape of the server.
    assert!(!stale(b"%output %3 hi"));
    assert!(!stale(b"%extended-output %3 10 : hi"));
    assert!(!stale(b"%pause %3"));

    assert!(stale(b"%window-add @2"));
    assert!(stale(b"%window-close @2"));
    assert!(stale(b"%sessions-changed"));
    assert!(stale(b"%window-pane-changed @2 %3"));
    // An unmodelled notification is precisely the one whose meaning is
    // unknown here, so it counts as invalidating.
    assert!(stale(b"%invented-later whatever"));
}

#[test]
fn a_line_is_bytes_because_tmux_does_not_promise_text() {
    // tmux escapes only what would break the line protocol, so a pane
    // emitting Latin-1 or binary produces a line that is not UTF-8.
    // Reading these as a string would fail the whole connection.
    let line = Line::parse(b"%output %0 \xff\xc3(");
    assert_eq!(
        line,
        Line::Event(Event::Output {
            pane: "%0".parse().expect("a pane id parses"),
            bytes: vec![0xff, 0xc3, b'('],
        }),
    );

    // The same holds for a window name inside a notification. The id is
    // ASCII and parses; the name it carries is whatever tmux stored.
    assert_eq!(
        Line::parse(b"%window-renamed @2 \xff"),
        Line::Event(Event::WindowRenamed {
            window: "@2".parse().expect("a window id parses"),
            name: TmuxText::from_bytes(*b"\xff"),
        }),
    );
}

#[test]
fn output_escaping_round_trips_the_bytes_tmux_sends() {
    assert_eq!(unescape_output(b"plain"), b"plain");
    // tmux escapes a byte below 0x20 as three octal digits.
    assert_eq!(unescape_output(br"a\015b"), b"a\rb");
    assert_eq!(unescape_output(br"\377"), vec![0xff]);
    // A literal backslash arrives doubled.
    assert_eq!(unescape_output(br"a\\b"), b"a\\b");
    // Anything else after a backslash is not an escape tmux produces, so
    // it is kept rather than guessed at.
    assert_eq!(unescape_output(br"a\zb"), b"a\\zb");
}

#[derive(Clone)]
struct Driver {
    shared: Arc<Shared>,
    terminal: Arc<Mutex<Option<Result<(), Error>>>>,
}

impl Driver {
    fn request(&self) -> Request {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let revision = self.shared.revision();
            let request = {
                let mut state = lock(&self.shared.state);
                let request = state.commands.pop_front();
                if request.is_some() {
                    self.shared.notify(&mut state);
                }
                request
            };
            if let Some(request) = request {
                return request;
            }
            assert!(Instant::now() < deadline, "a command arrives");
            self.shared.wait(revision, Some(deadline));
        }
    }
    fn deliver(&self, delivery: Delivery) {
        let mut state = lock(&self.shared.state);
        while state.events.len() >= channel::EVENT_QUEUE && state.receiver_open {
            state = self.shared.changed.wait(state).expect("test queue lock");
        }
        if state.receiver_open {
            state.events.push_back(delivery);
            self.shared.notify(&mut state);
        }
    }
    fn terminal(&self, outcome: Result<(), Error>) {
        *lock(&self.terminal) = Some(outcome);
    }
    fn finish(&self, outcome: Result<(), Error>) {
        self.terminal(outcome);
        let mut state = lock(&self.shared.state);
        state.finished = true;
        self.shared.notify(&mut state);
    }
}

fn control_pair(timeout: Duration) -> (ControlSender, ControlEvents, Driver) {
    let (left, right) = UnixStream::pair().expect("test socket pair");
    let shared = Shared::new(left, right);
    let terminal = Arc::new(Mutex::new(None));
    let worker = Arc::clone(&shared);
    let outcome = Arc::clone(&terminal);
    let connection = thread::spawn(move || {
        let mut state = lock(&worker.state);
        while !state.stop && !state.finished {
            state = worker.changed.wait(state).expect("test queue lock");
        }
        state.accepting = false;
        state.finished = true;
        worker.notify(&mut state);
        drop(state);
        lock(&outcome).take().unwrap_or(Ok(()))
    });
    let sender = ControlSender {
        commands: channel::Sender {
            shared: Arc::clone(&shared),
        },
        timeout,
        pane_off_is_safe: true,
        identity: crate::ServerIdentity::from_socket_path(
            "/tmp/libtmux-rs-test/control-sender".into(),
        ),
        cancellation: crate::CancellationToken::new(),
        next_request_id: Arc::new(AtomicU64::new(1)),
    };
    let events = ControlEvents {
        events: channel::Receiver {
            shared: Arc::clone(&shared),
            connection: Some(connection),
        },
    };
    (sender, events, Driver { shared, terminal })
}

fn output(number: u64, succeeded: bool, lines: &[&str]) -> BlockResult {
    BlockResult {
        number,
        succeeded,
        output: lines.iter().map(|line| TmuxText::from(*line)).collect(),
        sensitive_input: false,
        failed_lines: vec![!succeeded; lines.len()],
    }
}

#[test]
fn bootstrap_cleanup_drains_events_and_preserves_error_priority() {
    for after_effect in [false, true] {
        for recover_terminal in [false, true] {
            let primary = if recover_terminal {
                Error::control_mode_closed()
            } else {
                output(1, false, &["refused"])
                    .refusal_for("refresh-client")
                    .expect("refusal")
            };
            let primary = if after_effect {
                primary.after_effect("watch-only")
            } else {
                primary
            };
            let original = format!("{primary:?}");
            let (_sender, events, driver) = control_pair(Duration::from_secs(1));
            driver.deliver(Delivery::Boundary(super::Boundary(1)));
            driver.terminal(Err(Error::control_mode_frame_too_large("line", 64)));
            let error = events.shutdown_after_error(primary);
            if !recover_terminal {
                assert_eq!(format!("{error:?}"), original);
                continue;
            }
            let cause = match error {
                Error::AfterEffect { source, .. } => *source,
                error => error,
            };
            assert!(matches!(
                cause,
                Error::ControlModeFrameTooLarge {
                    frame: "line",
                    limit: 64
                }
            ));
        }
    }
}

#[test]
fn cancelling_bootstrap_cleanup_still_stops_the_connection() {
    let (sender, events, _driver) = control_pair(Duration::from_secs(1));
    sender.cancellation.cancel();
    let worker = thread::spawn(move || events.shutdown_after_error(Error::control_mode_closed()));
    let error = worker
        .join()
        .expect("cleanup ignores the cancelled operation token");
    assert!(matches!(
        error,
        Error::ControlMode {
            kind: ControlModeErrorKind::Closed,
            ..
        }
    ));
    assert!(sender.is_closed());
}

#[test]
fn cancelling_a_pending_next_preserves_the_terminal_error() {
    let (_sender, mut events, driver) = control_pair(Duration::from_secs(1));
    let stop = driver.shared.stop.clone();
    // Blocking iteration has no partially polled future. A pending call waits
    // for cleanup and owns the one terminal diagnostic.
    let receiver = thread::spawn(move || {
        let error = events
            .next_event()
            .expect("terminal diagnostic")
            .expect_err("timeout");
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert!(events.next_event().is_none());
        events.shutdown().expect("error already delivered");
    });
    driver.terminal(Err(Error::control_mode_timeout()));
    stop.cancel();
    receiver
        .join()
        .expect("the pending iterator finishes after cleanup");
}

#[test]
fn watch_only_marks_a_transport_failure_after_its_first_mute() {
    let (sender, events, driver) = control_pair(Duration::from_secs(1));
    let watch = thread::spawn(move || sender.watch_only(&[]));
    driver
        .request()
        .result
        .send(Ok(output(1, true, &["%1", "%2"])));
    driver.request().result.send(Ok(reply(2)));
    driver
        .request()
        .result
        .send(Err(Error::control_mode_timeout()));
    let error = watch
        .join()
        .expect("watch joins")
        .expect_err("second mute failed");
    assert!(matches!(
        error,
        Error::AfterEffect {
            operation: "watch-only",
            ..
        }
    ));
    events.shutdown().expect("cleanup");
}

#[test]
fn watch_only_refuses_a_failed_listing_before_muting_any_pane() {
    let (sender, events, driver) = control_pair(Duration::from_secs(1));
    let watch = thread::spawn(move || sender.watch_only(&[]));
    driver
        .request()
        .result
        .send(Ok(output(1, false, &["failed listing"])));
    let error = watch
        .join()
        .expect("watch joins")
        .expect_err("listing refused");
    assert!(!matches!(error, Error::AfterEffect { .. }));
    assert!(lock(&driver.shared.state).commands.is_empty());
    events.shutdown().expect("cleanup");
}

#[test]
fn pane_output_shutdown_reports_the_specific_terminal_error() {
    let (sender, events, driver) = control_pair(Duration::from_secs(1));
    let mut pane = PaneOutput::new(a_pane(), events, sender);
    driver.finish(Err(Error::control_mode_frame_too_large("test-frame", 42)));
    assert!(pane.next_chunk().is_none());
    assert!(matches!(
        pane.shutdown(),
        Err(Error::ControlModeFrameTooLarge {
            frame: "test-frame",
            limit: 42
        })
    ));
}

#[test]
fn dirty_narrowing_reruns_after_an_in_flight_failure() {
    let (sender, events, driver) = control_pair(Duration::from_secs(1));
    let mut pane = PaneOutput::new(a_pane(), events, sender);
    pane.narrow();
    let first = driver.request();
    pane.narrow();
    first.result.send(Err(Error::control_mode_timeout()));
    let second = driver.request();
    assert_eq!(second.line, "list-panes -s -F \"#{pane_id}\"");
    second.result.send(Ok(output(2, true, &["%3"])));
    driver.request().result.send(Ok(output(3, true, &["%3"])));
    pane.shutdown().expect("cleanup");
}

#[test]
fn cancelling_a_snapshot_leaves_consumed_output_in_the_callers_sink() {
    let (sender, events, driver) = control_pair(Duration::from_secs(1));
    let cancellation = sender.cancellation.clone();
    let mut pane = PaneOutput::new(a_pane(), events, sender);
    let worker = thread::spawn(move || {
        let request = driver.request();
        driver.deliver(Delivery::Event(Event::Output {
            pane: a_pane(),
            bytes: b"before".to_vec(),
        }));
        driver.deliver(Delivery::Boundary(
            request.boundary.expect("snapshot boundary"),
        ));
        driver.deliver(Delivery::Event(Event::Output {
            pane: a_pane(),
            bytes: b"after".to_vec(),
        }));
        // Cancellation must win before the reply is made available.
        while !request.result.is_closed() {
            thread::yield_now();
        }
        request.result.send(Ok(output(1, true, &["visible"])));
    });
    let mut kept = Vec::new();
    let error = pane
        .snapshot(|chunk| {
            kept.extend_from_slice(chunk);
            cancellation.cancel();
        })
        .expect_err("snapshot cancelled");
    assert_eq!(error.kind(), ErrorKind::Cancelled);
    assert_eq!(kept, b"before");
    assert_eq!(pane.next_chunk().as_deref(), Some(b"after".as_slice()));
    worker.join().expect("driver joins");
    pane.shutdown().expect("cleanup");
}

#[test]
fn a_snapshot_streams_a_flood_into_caller_owned_storage() {
    let (sender, events, driver) = control_pair(Duration::from_secs(5));
    let mut pane = PaneOutput::new(a_pane(), events, sender);
    let count = HELD_WHILE_AWAITING * 2;
    let worker = thread::spawn(move || {
        let request = driver.request();
        for _ in 0..count {
            driver.deliver(Delivery::Event(Event::Output {
                pane: a_pane(),
                bytes: b"chunk".to_vec(),
            }));
        }
        driver.deliver(Delivery::Boundary(
            request.boundary.expect("snapshot boundary"),
        ));
        request.result.send(Ok(output(1, true, &["visible"])));
        driver.finish(Ok(()));
    });
    let mut bytes = 0;
    assert_eq!(
        pane.snapshot(|chunk| bytes += chunk.len())
            .expect("capture"),
        [TmuxText::from("visible")]
    );
    assert_eq!(bytes, count * 5);
    worker.join().expect("driver joins");
    assert!(pane.next_chunk().is_none());
    pane.shutdown().expect("cleanup");
}

#[test]
fn a_snapshot_rejected_before_writing_does_not_wait_for_a_boundary() {
    let (sender, events, driver) = control_pair(Duration::from_secs(1));
    let mut pane = PaneOutput::new(a_pane(), events, sender);
    let worker = thread::spawn(move || {
        driver
            .request()
            .result
            .send(Err(Error::control_mode_unread()));
    });
    assert_eq!(
        pane.snapshot(|_| {}).expect_err("snapshot refused").kind(),
        ErrorKind::Refused
    );
    worker.join().expect("driver joins");
    pane.shutdown().expect("cleanup");
}

#[test]
fn mute_pane_reports_a_control_error_block() {
    let (sender, events, driver) = control_pair(Duration::from_secs(1));
    let worker = thread::spawn(move || sender.mute_pane(&a_pane()));
    driver
        .request()
        .result
        .send(Ok(output(1, false, &["refused"])));
    assert_eq!(
        worker
            .join()
            .expect("mute joins")
            .expect_err("mute refused")
            .kind(),
        ErrorKind::Refused
    );
    events.shutdown().expect("cleanup");
}

#[test]
fn queue_wait_counts_toward_the_command_deadline() {
    let (sender, events, driver) = control_pair(Duration::from_millis(20));
    let mut kept = Vec::new();
    for _ in 0..channel::COMMAND_QUEUE {
        let (request, answer) = request();
        lock(&driver.shared.state).commands.push_back(request);
        kept.push(answer);
    }
    let error = sender
        .send(Command::new("list-sessions"))
        .expect_err("queue full");
    assert!(matches!(
        error,
        Error::ControlMode {
            kind: ControlModeErrorKind::DispatchTimedOut,
            ..
        }
    ));
    assert!(error.is_transient());
    assert_eq!(
        lock(&driver.shared.state).commands.len(),
        channel::COMMAND_QUEUE
    );
    events.shutdown().expect("cleanup");
}

#[test]
fn cancellation_before_actor_commit_refuses_the_request() {
    let (sender, events, driver) = control_pair(Duration::from_secs(1));
    let cancellation = sender.cancellation.clone();
    let sending = thread::spawn(move || sender.send(Command::new("list-sessions")));
    let request = driver.request();
    cancellation.cancel();
    assert_eq!(
        sending
            .join()
            .expect("sender joins")
            .expect_err("cancelled")
            .kind(),
        ErrorKind::Cancelled
    );
    assert!(request.commit().is_none());
    events.shutdown().expect("cleanup");
}

#[test]
fn deadline_before_actor_commit_refuses_the_request() {
    let (sender, events, driver) = control_pair(Duration::from_millis(20));
    let sending = thread::spawn(move || sender.send(Command::new("list-sessions")));
    let request = driver.request();
    let error = sending
        .join()
        .expect("sender joins")
        .expect_err("deadline elapsed");
    assert!(matches!(
        error,
        Error::ControlMode {
            kind: ControlModeErrorKind::DispatchTimedOut,
            ..
        }
    ));
    assert!(error.is_transient());
    assert!(request.commit().is_none());
    events.shutdown().expect("cleanup");
}

#[test]
fn cancellation_remains_visible_after_the_actor_drops_the_request() {
    let (sender, events, driver) = control_pair(Duration::from_secs(1));
    let (mut submission, _) = sender
        .begin_ordered(Command::new("list-sessions"), None)
        .expect("command is representable");
    assert!(submission.poll().is_none());
    sender.cancellation.cancel();
    assert!(driver.request().commit().is_none());
    assert_eq!(
        submission
            .poll()
            .expect("cancelled result")
            .expect_err("cancelled")
            .kind(),
        ErrorKind::Cancelled
    );
    assert!(!sender.is_closed());
    events.shutdown().expect("cleanup");
}

#[test]
fn cancellation_after_commit_keeps_reply_alignment() {
    let (sender, events, driver) = control_pair(Duration::from_secs(1));
    let cancellation = sender.cancellation.clone();
    let sending = thread::spawn(move || sender.send(Command::new("list-sessions")));
    let request = driver.request().commit().expect("committed");
    cancellation.cancel();
    assert_eq!(
        sending
            .join()
            .expect("sender joins")
            .expect_err("cancelled")
            .kind(),
        ErrorKind::Cancelled
    );
    let mut slots = ReplySlots::default();
    let cancelled_fence = slots.push(request.result, None);
    assert!(matches!(
        slots.slots.front(),
        Some(ReplySlot::Tombstone { .. })
    ));
    let (result, mut next) = reply_channel();
    slots.push(result, None);
    slots.consume(reply(1), usize::MAX).expect("parent block");
    slots.consume(reply(2), usize::MAX).expect("nested block");
    assert!(matches!(next.try_recv(), Err(TryRecvError::Empty)));
    slots
        .consume(cancelled_fence.block(), usize::MAX)
        .expect("cancelled fence");
    assert!(matches!(next.try_recv(), Err(TryRecvError::Empty)));
    slots.complete(reply(3));
    assert_eq!(
        next.try_recv()
            .expect("next reply")
            .expect("success")
            .number(),
        3
    );
    events.shutdown().expect("cleanup");
}

#[test]
fn a_line_that_never_ends_stops_at_its_budget() {
    let mut stream = std::io::BufReader::new(std::io::repeat(b'a'));
    let error = super::protocol::read_raw(&mut stream, 4096).expect_err("endless line refused");
    assert!(matches!(
        error,
        Error::ControlModeFrameTooLarge {
            frame: "line",
            limit: 4096
        }
    ));
}

#[test]
fn event_timeout_keeps_the_next_event_and_connection_error() {
    let (_sender, mut events, driver) = control_pair(Duration::from_secs(1));
    let error = events
        .next_event_timeout(Duration::from_millis(10))
        .expect_err("quiet stream times out");
    assert!(error.is_transient());
    assert_eq!(error.kind(), ErrorKind::Timeout);
    driver.deliver(Delivery::Event(Event::SessionsChanged));
    assert_eq!(
        events
            .next_event_timeout(Duration::ZERO)
            .expect("queued event is immediate"),
        Some(Event::SessionsChanged)
    );
    driver.finish(Err(Error::control_mode_frame_too_large("line", 42)));
    assert!(matches!(
        events.next_event_timeout(Duration::from_secs(1)),
        Err(Error::ControlModeFrameTooLarge {
            frame: "line",
            limit: 42
        })
    ));
    assert!(
        events
            .next_event_timeout(Duration::ZERO)
            .expect("error delivered once")
            .is_none()
    );
    events.shutdown().expect("cleanup delivered");
}

#[test]
fn output_timeout_preserves_the_next_chunk() {
    let (sender, events, driver) = control_pair(Duration::from_secs(1));
    let mut output = PaneOutput::new(a_pane(), events, sender);
    let error = output
        .next_chunk_timeout(Duration::from_millis(10))
        .expect_err("quiet output times out");
    assert!(error.is_transient());
    driver.deliver(Delivery::Event(Event::Output {
        pane: a_pane(),
        bytes: b"kept".to_vec(),
    }));
    assert_eq!(
        output
            .next_chunk_timeout(Duration::ZERO)
            .expect("queued output is immediate")
            .as_deref(),
        Some(b"kept".as_slice())
    );
    output.shutdown().expect("cleanup");
}
