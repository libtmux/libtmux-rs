//! Complete control notification and line decoding contracts.

// Fixture helpers assert known protocol inputs outside test functions.
#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use libtmux_core::control::{Event, Line, unescape_output};
use libtmux_core::{PaneId, SessionId, TmuxText, WindowId};

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

#[test]
fn block_origin_flags_are_required_and_retained() {
    for flags in [0, 1, 3, u64::MAX] {
        let line = format!("%begin 1700000000 12 {flags}");
        assert_eq!(
            Line::parse(line.as_bytes()),
            Line::BlockStart { number: 12, flags }
        );
    }
    for line in [
        b"%begin 1700000000 12".as_slice(),
        b"%begin 1700000000 12 bad",
    ] {
        assert_eq!(Line::parse(line), Line::Text(TmuxText::from_bytes(line)));
    }
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
