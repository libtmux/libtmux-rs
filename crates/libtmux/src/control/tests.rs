use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};

use super::{
    BlockResult, ControlEvents, ControlSender, Event, HELD_WHILE_AWAITING, Line, PaneOutput,
    ReplySlot, ReplySlots, Request, admit_request, decode_watched_pane_id, unescape_output,
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
    }
}

fn request() -> (Request, oneshot::Receiver<Result<BlockResult, Error>>) {
    let (result, answer) = oneshot::channel();
    let (commit, _commitment) = oneshot::channel();
    (
        Request {
            line: String::new(),
            deadline: None,
            commit,
            result,
        },
        answer,
    )
}

fn sender(commands: mpsc::Sender<Request>, timeout: Duration) -> ControlSender {
    ControlSender {
        commands,
        timeout,
        pane_off_is_safe: true,
    }
}

fn refused(mut answer: oneshot::Receiver<Result<BlockResult, Error>>) -> Error {
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

#[tokio::test]
async fn watch_only_marks_a_transport_failure_after_its_first_mute() {
    let (commands, mut requests) = mpsc::channel(4);
    let sender = ControlSender {
        commands,
        timeout: Duration::from_secs(1),
        pane_off_is_safe: true,
    };
    let watch = tokio::spawn(async move { sender.watch_only(&[]).await });

    let listing = requests.recv().await.expect("list-panes request");
    assert!(listing.line.starts_with("list-panes "));
    listing
        .result
        .send(Ok(BlockResult {
            number: 1,
            succeeded: true,
            output: vec![TmuxText::from_bytes(*b"%1"), TmuxText::from_bytes(*b"%2")],
            sensitive_input: false,
        }))
        .expect("watch is waiting for the listing");

    let first_mute = requests.recv().await.expect("first mute request");
    assert!(first_mute.line.contains("%1:off"));
    first_mute
        .result
        .send(Ok(reply(2)))
        .expect("watch is waiting for the first mute");

    let second_mute = requests.recv().await.expect("second mute request");
    assert!(second_mute.line.contains("%2:off"));
    second_mute
        .result
        .send(Err(Error::Overloaded {
            request_id: 11,
            command: Command::new("refresh-client").summary(),
            in_flight: 1,
        }))
        .expect("watch is waiting for the second mute");

    let error = watch
        .await
        .expect("watch task does not panic")
        .expect_err("the second mute fails");
    assert!(matches!(
        error,
        Error::AfterEffect { operation: "watch-only", source }
            if source.kind() == ErrorKind::Refused && source.is_transient()
    ));
}

#[tokio::test]
async fn watch_only_refuses_a_failed_listing_before_muting_any_pane() {
    let (commands, mut requests) = mpsc::channel(2);
    let sender = ControlSender {
        commands,
        timeout: Duration::from_secs(1),
        pane_off_is_safe: true,
    };
    let watch = tokio::spawn(async move { sender.watch_only(&[]).await });

    let listing = requests.recv().await.expect("list-panes request");
    listing
        .result
        .send(Ok(BlockResult {
            number: 1,
            succeeded: false,
            output: vec![TmuxText::from_bytes(*b"listing refused")],
            sensitive_input: false,
        }))
        .expect("watch is waiting for the listing");

    let error = watch
        .await
        .expect("watch task does not panic")
        .expect_err("a failed listing is not pane data");
    assert_eq!(error.kind(), ErrorKind::Refused);
    assert!(!matches!(error, Error::AfterEffect { .. }));
    assert!(requests.try_recv().is_err(), "no mute was dispatched");
}

#[tokio::test]
async fn dirty_narrowing_reruns_after_an_in_flight_failure() {
    let (commands, mut requests) = mpsc::channel(4);
    let sender = sender(commands, Duration::from_secs(5));
    let (_events, received) = mpsc::channel(1);
    let (stop, _stopped) = watch::channel(());
    let connection = tokio::spawn(async { Ok::<(), Error>(()) });
    let output = PaneOutput::new(
        "%1".parse().expect("a pane id"),
        ControlEvents {
            events: received,
            stop,
            connection,
        },
        sender,
    );

    output.narrow();
    let first = requests.recv().await.expect("the first list-panes request");
    assert!(first.line.starts_with("list-panes "));
    output.narrow();
    first
        .result
        .send(Err(Error::control_mode_closed()))
        .expect("the first pass is still waiting");

    let second = tokio::time::timeout(Duration::from_secs(1), requests.recv())
        .await
        .expect("the dirty state starts another pass")
        .expect("the sender remains open");
    assert!(second.line.starts_with("list-panes "));
    second
        .result
        .send(Ok(reply(2)))
        .expect("the second pass is still waiting");
}

#[tokio::test]
async fn mute_pane_reports_a_control_error_block() {
    let (commands, mut requests) = mpsc::channel(1);
    let sender = ControlSender {
        commands,
        timeout: Duration::from_secs(1),
        pane_off_is_safe: true,
    };
    let pane: PaneId = "%1".parse().expect("a pane id");
    let mute = tokio::spawn(async move { sender.mute_pane(&pane).await });

    let request = requests.recv().await.expect("mute request");
    request
        .result
        .send(Ok(BlockResult {
            number: 1,
            succeeded: false,
            output: vec![TmuxText::from_bytes(*b"mute refused")],
            sensitive_input: false,
        }))
        .expect("mute is waiting for its block");

    let error = mute
        .await
        .expect("mute task does not panic")
        .expect_err("an error block is not success");
    assert_eq!(error.kind(), ErrorKind::Refused);
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
        matches!(c_reply.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
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

#[tokio::test]
async fn queue_wait_counts_toward_the_command_deadline() {
    let (commands, mut requests) = mpsc::channel(1);
    let sender = sender(commands.clone(), Duration::from_millis(20));
    let (occupant, _answer) = request();
    commands
        .try_send(occupant)
        .expect("the command queue has its one slot filled");

    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        sender.send(Command::new("list-sessions")),
    )
    .await
    .expect("the sender applies its own deadline");
    let error = outcome.expect_err("the full queue exceeds the deadline");
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(
        matches!(
            &error,
            Error::ControlMode {
                kind: ControlModeErrorKind::DispatchTimedOut,
                ..
            }
        ),
        "the command did not reach the write boundary",
    );
    assert!(
        error.is_transient(),
        "the unwritten command is safe to retry"
    );

    let _occupant = requests.recv().await.expect("the first request remains");
    assert!(
        requests.try_recv().is_err(),
        "the expired request never enters the queue",
    );
}

#[tokio::test]
async fn cancellation_before_actor_commit_refuses_the_request() {
    let (commands, mut requests) = mpsc::channel(1);
    let sender = sender(commands, Duration::from_secs(1));
    let sending = tokio::spawn(async move { sender.send(Command::new("list-sessions")).await });

    let request = requests.recv().await.expect("the request is queued");
    sending.abort();
    assert!(
        sending
            .await
            .expect_err("the caller was cancelled")
            .is_cancelled(),
    );
    assert!(
        request.commit().is_none(),
        "the actor cannot commit a cancelled request",
    );
}

#[tokio::test]
async fn deadline_before_actor_commit_refuses_the_request() {
    let timeout = Duration::from_millis(20);
    let (commands, mut requests) = mpsc::channel(1);
    let sender = sender(commands, timeout);
    let sending = tokio::spawn(async move { sender.send(Command::new("list-sessions")).await });

    let request = requests.recv().await.expect("the request is queued");
    let error = sending
        .await
        .expect("the caller task joins")
        .expect_err("the held request reaches its deadline");
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(
        matches!(
            &error,
            Error::ControlMode {
                kind: ControlModeErrorKind::DispatchTimedOut,
                ..
            }
        ),
        "the held command did not reach the write boundary",
    );
    assert!(
        error.is_transient(),
        "the unwritten command is safe to retry"
    );
    assert!(
        request.commit().is_none(),
        "the actor cannot commit the expired request",
    );
}

#[tokio::test]
async fn cancellation_after_commit_keeps_reply_alignment() {
    let (commands, mut requests) = mpsc::channel(1);
    let sender = sender(commands, Duration::from_secs(1));
    let sending = tokio::spawn(async move { sender.send(Command::new("list-sessions")).await });

    let request = requests.recv().await.expect("the request is queued");
    let request = request.commit().expect("the actor commits the request");
    sending.abort();
    assert!(
        sending
            .await
            .expect_err("the caller was cancelled")
            .is_cancelled(),
    );

    let mut replies = ReplySlots::default();
    replies.push(request.result, request.deadline);
    assert!(
        matches!(replies.slots.front(), Some(ReplySlot::Tombstone { .. })),
        "the committed command keeps its reply slot",
    );

    let (next, mut next_answer) = oneshot::channel();
    replies.push(next, None);
    replies.complete(reply(1));
    assert!(
        matches!(
            next_answer.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ),
        "the cancelled command consumes its own block",
    );
    replies.complete(reply(2));
    assert_eq!(
        next_answer
            .try_recv()
            .expect("the next caller is answered")
            .expect("the next command succeeds")
            .number(),
        2,
    );
}

#[test]
fn reply_deadline_is_the_earliest_pending_deadline() {
    let now = tokio::time::Instant::now();
    let earlier = now + Duration::from_secs(1);
    let later = now + Duration::from_secs(2);
    let mut replies = ReplySlots::default();
    let (first, _first_answer) = oneshot::channel();
    let (second, _second_answer) = oneshot::channel();

    replies.push(first, later.into());
    replies.push(second, earlier.into());
    assert_eq!(replies.earliest_deadline(), Some(earlier));
    replies.complete(reply(1));
    assert_eq!(replies.earliest_deadline(), Some(earlier));
    replies.complete(reply(2));
    assert_eq!(replies.earliest_deadline(), None);

    let (first, _first_answer) = oneshot::channel();
    let (second, _second_answer) = oneshot::channel();
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
        Line::BlockStart(347)
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

/// Shared by the notification tests, which between them name every
/// notification tmux writes. The strings are tmux's own format strings
/// from `control-notify.c` and `control.c` with the placeholders filled.
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

/// The one notification tmux builds from a format template, so its
/// trailing field is whatever `#{window_raw_flags}` expanded to.
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

/// tmux writes `-` for a field the subscription does not name, so an
/// absent one is a real answer rather than a parse failure.
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

/// tmux adds notifications between releases, so an unrecognized one is
/// kept rather than dropped.
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

/// tmux queues a notification raised while a block is open, so a line
/// inside one is command output even when it reads as a notification.
/// `list-panes -F '#{pane_id}'` writes `%0` for every row.
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

/// Parsing these leniently would report a pane that does not exist, which
/// is worse than reporting a line nobody claimed. The text keeps the whole
/// line, notification name included, so nothing is lost by not knowing it.
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
