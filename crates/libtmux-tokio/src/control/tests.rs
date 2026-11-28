#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use libtmux_core::CreationNonce;
use libtmux_core::control::{Completion, ControlNotice, RequestKind, Tracker};
use tokio::sync::{Semaphore, mpsc, watch};

use super::{BlockResult, ControlEvents, ControlSender, Event, PaneOutput};
use crate::{Command, ControlLimits, Error, PaneId, SessionId, TmuxText, WindowId};

fn tracker() -> Tracker {
    let mut tracker = Tracker::new(
        "$0".parse().unwrap(),
        libtmux_core::control::ControlLimits::default(),
    );
    tracker
        .feed(b"%begin 1 1 0\n%end 1 1 0\n%session-changed $0 fixture\n")
        .unwrap();
    assert!(matches!(
        tracker.next_completion(),
        Some(Completion::Opened)
    ));
    tracker.discard_notices();
    tracker
}

fn reply(wire: &[u8]) -> BlockResult {
    let mut tracker = tracker();
    tracker
        .register(
            1,
            CreationNonce::from_bytes([0xab; 16]),
            RequestKind::Ordinary,
        )
        .unwrap();
    tracker.feed(wire).unwrap();
    tracker.feed(b"%begin 1 99 1\n%end 1 99 1\nunknown command: libtmux-control-fence:abababababababababababababababab\n").unwrap();
    match tracker.next_completion().unwrap() {
        Completion::Reply { reply, .. } => BlockResult::from_shared(reply),
        other => panic!("unexpected completion: {other:?}"),
    }
}

fn sender(commands: mpsc::Sender<super::Request>, timeout: Duration) -> ControlSender {
    let generation = crate::ServerGeneration::from_raw(123, 456);
    let native_server = crate::Server::builder()
        .socket_path("/tmp/libtmux-rs-test/control-unit")
        .build()
        .unwrap()
        .bound_to(generation)
        .unwrap();
    ControlSender {
        generation,
        native_server,
        max_line_bytes: ControlLimits::DEFAULT_LINE_BYTES,
        max_reply_blocks: 256,
        commands,
        admission: Arc::new(Semaphore::new(16)),
        timeout,
        pane_off_is_safe: true,
        version: crate::TmuxVersion::parse_output(b"tmux 3.8\n").unwrap(),
        identity: crate::ServerIdentity::from_socket_path(
            "/tmp/libtmux-rs-test/control-unit".into(),
        ),
    }
}

fn events(
    limits: ControlLimits,
    outcome: Result<(), Error>,
) -> (ControlEvents, super::actor::TestEvents) {
    let (events, wire) = super::actor::event_fixture("$0".parse().unwrap(), limits);
    wire.feed(b"%begin 1 1 0\n%end 1 1 0\n%session-changed $0 fixture\n")
        .unwrap();
    wire.discard();
    let (stop, _stopped) = watch::channel(());
    let connection = tokio::spawn(async move { outcome });
    (
        ControlEvents {
            events,
            stop,
            connection: Some(connection),
        },
        wire,
    )
}

#[test]
fn exact_input_blocks_exclude_hooks_and_debug_withholds_payloads() {
    let block = reply(b"%begin 1 2 1\nfirst\n%end 1 2 1\n%begin 1 3 0\nprivate-hook\n%end 1 3 0\n%begin 1 4 1\nlast\n%end 1 4 1\n");
    assert!(block.succeeded());
    assert_eq!(block.blocks().len(), 2);
    assert_eq!(block.blocks()[0].output(), b"first\n");
    assert_eq!(block.blocks()[1].output(), b"last\n");
    assert_eq!(
        block.output(),
        &[TmuxText::from("first"), TmuxText::from("last")]
    );
    assert!(!format!("{block:?}").contains("first"));
    assert!(!format!("{block:?}").contains("private-hook"));
}

#[test]
fn a_failed_input_block_never_becomes_a_successful_aggregate() {
    let block =
        reply(b"%begin 1 2 1\nrefused\n%error 1 2 1\n%begin 1 3 1\naccepted later\n%end 1 3 1\n");
    assert!(!block.succeeded());
    assert!(!block.blocks()[0].succeeded());
    assert!(block.blocks()[1].succeeded());
}

#[tokio::test]
async fn feature_and_blocking_policies_refuse_before_admission() {
    let (commands, mut queued) = mpsc::channel(1);
    let sender = sender(commands, Duration::from_secs(1));
    for command in [
        Command::new("wait-for").arg("channel"),
        Command::new("run-shell").arg("sleep 1"),
        Command::new("capture-pane").arg("-t").arg("%1").arg("-p"),
        Command::new("list-panes")
            .arg("-F")
            .arg("#{pane_id}")
            .requires_tmux(
                "future",
                crate::ReleaseVersion::new(99, 0, crate::ReleaseSuffix::FINAL),
            ),
    ] {
        assert!(sender.send(command).await.is_err());
        assert!(matches!(
            queued.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }
}

#[tokio::test]
async fn queue_wait_and_admission_count_toward_the_original_deadline() {
    let (commands, mut queued) = mpsc::channel(1);
    let sender = sender(commands, Duration::from_millis(20));
    let held = Arc::clone(&sender.admission)
        .acquire_many_owned(16)
        .await
        .unwrap();
    let error = sender
        .send(Command::new("list-panes").arg("-F").arg("#{pane_id}"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::ControlMode {
            kind: crate::ControlModeErrorKind::DispatchTimedOut,
            ..
        }
    ));
    assert!(matches!(
        queued.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    drop(held);
}

#[tokio::test]
async fn cancelling_an_event_wait_keeps_the_next_notice_and_terminal_error() {
    let (mut events, wire) = events(ControlLimits::default(), Err(Error::control_mode_timeout()));
    tokio::select! { biased; _ = events.next_notice() => panic!("no notice yet"), () = std::future::ready(()) => {} }
    wire.feed(b"%output %3 bytes\n").unwrap();
    assert!(
        matches!(events.next_notice().await, Some(Ok(ControlNotice::Event(Event::Output { bytes, .. }))) if bytes == b"bytes")
    );
    wire.finish();
    assert!(matches!(
        events.next_notice().await,
        Some(Err(Error::ControlMode {
            kind: crate::ControlModeErrorKind::TimedOut,
            ..
        }))
    ));
    assert!(events.next_notice().await.is_none());
}

#[tokio::test]
async fn subscriber_overflow_is_a_gap_and_preserves_pause_provenance() {
    let (mut events, wire) = events(
        ControlLimits::default().max_events(1).max_event_bytes(64),
        Ok(()),
    );
    wire.feed(b"%output %3 retained\n%pause %3\n%output %3 lost\n")
        .unwrap();
    assert!(matches!(
        events.next_notice().await,
        Some(Ok(ControlNotice::Event(Event::Output { .. })))
    ));
    assert!(matches!(
        events.next_notice().await,
        Some(Ok(ControlNotice::Gap {
            dropped: 2,
            included_pause: true
        }))
    ));
    wire.feed(b"%output %3 later\n").unwrap();
    assert!(
        matches!(events.next_event().await, Some(Ok(Event::Output { bytes, .. })) if bytes == b"later")
    );
    wire.finish();
    assert!(events.next_event().await.is_none());
}

#[tokio::test]
async fn pane_output_yields_a_gap_and_then_the_original_terminal_cause() {
    let (events, wire) = events(
        ControlLimits::default(),
        Err(Error::control_mode_frame_too_large("line", 64)),
    );
    let (commands, _queued) = mpsc::channel(1);
    let mut output = PaneOutput::new(
        "%3".parse().unwrap(),
        events,
        sender(commands, Duration::from_secs(1)),
    );
    wire.feed(b"%output %3 first\n%pause %3\n%continue %3\n%output %3 second\n")
        .unwrap();
    assert_eq!(output.next_chunk().await.unwrap().unwrap(), b"first");
    assert!(matches!(
        output.next_chunk().await,
        Some(Err(Error::ControlGap {
            included_pause: true,
            ..
        }))
    ));
    assert_eq!(output.next_chunk().await.unwrap().unwrap(), b"second");
    wire.finish();
    assert!(matches!(
        output.next_chunk().await,
        Some(Err(Error::ControlModeFrameTooLarge {
            frame: "line",
            limit: 64
        }))
    ));
    assert!(output.next_chunk().await.is_none());
    output.shutdown().await.unwrap();
}

#[tokio::test]
async fn unrelated_notices_yield_before_the_next_pane_chunk() {
    use futures_core::Stream as _;
    use std::pin::Pin;
    use std::task::Poll;

    let (events, wire) = events(ControlLimits::default(), Ok(()));
    let (commands, _queued) = mpsc::channel(1);
    let mut output = PaneOutput::new(
        "%3".parse().unwrap(),
        events,
        sender(commands, Duration::from_secs(1)),
    );
    wire.feed(
        b"%output %4 other\n"
            .repeat(super::PANE_NOTICE_BATCH + 1)
            .as_slice(),
    )
    .unwrap();
    wire.feed(b"%output %3 wanted\n").unwrap();
    let first =
        std::future::poll_fn(|context| Poll::Ready(Pin::new(&mut output).poll_next(context))).await;
    assert!(
        first.is_pending(),
        "one poll cannot consume an endless unrelated stream"
    );
    assert_eq!(output.next_chunk().await.unwrap().unwrap(), b"wanted");
    output.shutdown().await.unwrap();
}

#[tokio::test]
async fn snapshot_consumes_a_bounded_batch_even_when_output_keeps_arriving() {
    let (events, wire) = events(ControlLimits::default().max_events(1), Ok(()));
    let (commands, _queued) = mpsc::channel(1);
    let mut output = PaneOutput::new(
        "%3".parse().unwrap(),
        events,
        sender(commands, Duration::from_secs(1)),
    );
    wire.feed(b"%output %3 first\n").unwrap();
    let mut delivered = 0;
    let error = output
        .snapshot(|_| {
            delivered += 1;
            assert!(
                delivered <= 1,
                "new arrivals cannot keep snapshot from capturing"
            );
            wire.feed(b"%output %3 later\n").unwrap();
        })
        .await
        .unwrap_err();
    assert_eq!(delivered, 1);
    assert!(matches!(
        error,
        Error::ControlMode {
            kind: crate::ControlModeErrorKind::UnrepresentableCommand,
            ..
        }
    ));
    assert_eq!(output.next_chunk().await.unwrap().unwrap(), b"later");
    output.shutdown().await.unwrap();
}

fn event(line: &[u8]) -> Event {
    let mut tracker = tracker();
    let mut bytes = line.to_vec();
    bytes.push(b'\n');
    tracker.feed(&bytes).unwrap();
    match tracker.next_notice().unwrap() {
        ControlNotice::Event(event) => event,
        other => panic!("{other:?}"),
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

#[test]
fn malformed_notifications_remain_unframed_notices_and_bad_headers_close() {
    for line in [
        b"%window-add nonsense".as_slice(),
        b"%pause nonsense",
        b"%extended-output %3 notanumber : hi",
        b"%session-window-changed $0 nonsense",
    ] {
        assert_eq!(
            event(line),
            Event::Other {
                name: "unframed".into(),
                rest: TmuxText::from_bytes(line)
            }
        );
    }
    assert!(tracker().feed(b"%begin bad\n").is_err());
}

#[test]
fn event_byte_fidelity_and_escaping_use_the_shared_production_decoder() {
    assert_eq!(
        event(b"%output %3 \xff\xc3("),
        Event::Output {
            pane: a_pane(),
            bytes: vec![0xff, 0xc3, b'(']
        }
    );
    assert_eq!(
        event(b"%window-renamed @2 \xff"),
        Event::WindowRenamed {
            window: a_window(),
            name: TmuxText::from_bytes([0xff])
        }
    );
    for (encoded, bytes) in [
        (b"plain".as_slice(), b"plain".as_slice()),
        (br"a\015b", b"a\rb"),
        (br"\377", &[0xff]),
        (br"a\\b", br"a\b"),
        (br"a\zb", br"a\zb"),
    ] {
        let mut line = b"%output %3 ".to_vec();
        line.extend_from_slice(encoded);
        assert_eq!(
            event(&line),
            Event::Output {
                pane: a_pane(),
                bytes: bytes.to_vec()
            }
        );
    }
}

#[test]
fn unfinished_lines_stop_at_the_shared_budget_without_a_newline() {
    let mut tracker = Tracker::new(
        a_session(),
        libtmux_core::control::ControlLimits {
            max_line_bytes: 64,
            ..Default::default()
        },
    );
    assert!(matches!(
        tracker.feed(&[b'x'; 65]),
        Err(libtmux_core::control::ControlFault::LineLimit { limit: 64 })
    ));
}
