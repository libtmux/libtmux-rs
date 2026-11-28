use super::*;
use crate::SessionId;

pub(super) const FIRST: CreationNonce = CreationNonce::from_bytes([0x10; 16]);
const SECOND: CreationNonce = CreationNonce::from_bytes([0x20; 16]);

pub(super) fn opened(limits: ControlLimits) -> Tracker {
    let mut tracker = Tracker::new("$0".parse::<SessionId>().unwrap(), limits);
    tracker
        .feed(b"%begin 0 1 0\n%end 0 1 0\n%session-changed $0 fixture\n")
        .unwrap();
    assert!(matches!(
        tracker.next_completion(),
        Some(Completion::Opened)
    ));
    while tracker.next_notice().is_some() {}
    tracker
}

pub(super) fn first_fence() -> &'static [u8] {
    b"%begin 0 8 1\n%end 0 8 1\nunknown command: libtmux-control-fence:10101010101010101010101010101010\n"
}

#[test]
fn opening_block_never_answers_the_first_registered_command() {
    let mut tracker = Tracker::new("$0".parse().unwrap(), ControlLimits::default());
    assert!(tracker.register(1, FIRST, RequestKind::Ordinary).is_err());
    tracker.feed(b"%begin 0 1 0\n%end 0 1 0\n").unwrap();
    assert!(tracker.next_completion().is_none());
    tracker.feed(b"%session-changed $0 fixture\n").unwrap();
    assert!(matches!(
        tracker.next_completion(),
        Some(Completion::Opened)
    ));
    tracker.register(1, FIRST, RequestKind::Ordinary).unwrap();
    tracker
        .feed(b"%begin 0 2 1\nactual reply\n%end 0 2 1\n")
        .unwrap();
    assert!(tracker.next_completion().is_none());
    tracker.feed(first_fence()).unwrap();
    let Some(Completion::Reply { token: 1, reply }) = tracker.next_completion() else {
        panic!("missing first reply")
    };
    assert_eq!(reply.blocks().len(), 1);
    assert_eq!(reply.blocks()[0].output(), b"actual reply\n");
}

#[test]
fn cancelled_requests_keep_their_place_until_the_fence() {
    let mut tracker = opened(ControlLimits {
        max_pending_requests: 1,
        ..ControlLimits::default()
    });
    tracker.register(1, FIRST, RequestKind::Ordinary).unwrap();
    tracker.feed(b"%begin 0 2 1\nprivate-partial").unwrap();
    tracker.cancel(1);
    assert!(matches!(
        tracker.register(2, SECOND, RequestKind::Ordinary),
        Err(ControlFault::Busy { limit: 1 })
    ));
    for bytes in b"-discarded\n%end 0 2 1\n".chunks(3) {
        tracker.feed(bytes).unwrap();
    }
    tracker.feed(first_fence()).unwrap();
    assert!(matches!(
        tracker.next_completion(),
        Some(Completion::Drained { token: 1 })
    ));
    tracker.register(2, SECOND, RequestKind::Ordinary).unwrap();
    tracker.feed(b"%begin 0 9 1\nLATER\n%end 0 9 1\n%begin 0 10 1\n%end 0 10 1\nunknown command: libtmux-control-fence:20202020202020202020202020202020\n").unwrap();
    let Some(Completion::Reply { token: 2, reply }) = tracker.next_completion() else {
        panic!("missing later reply")
    };
    assert_eq!(reply.blocks()[0].output(), b"LATER\n");
    assert!(tracker.next_completion().is_none());
}

#[test]
fn nested_input_blocks_and_hook_blocks_do_not_change_request_ownership() {
    let mut tracker = opened(ControlLimits::default());
    tracker.register(1, FIRST, RequestKind::Ordinary).unwrap();
    tracker.feed(b"%begin 0 2 1\nouter\n%end 0 2 1\n%begin 0 3 0\nprivate-hook\n%end 0 3 0\n%begin 0 4 1\ninner-\xff\n%end 0 4 1\n").unwrap();
    tracker.feed(first_fence()).unwrap();
    let Some(Completion::Reply { token: 1, reply }) = tracker.next_completion() else {
        panic!("missing compound reply")
    };
    assert!(reply.succeeded());
    assert_eq!(
        reply
            .blocks()
            .iter()
            .map(ControlBlock::output)
            .collect::<Vec<_>>(),
        [b"outer\n".as_slice(), b"inner-\xff\n".as_slice()]
    );
}

#[test]
fn deferred_fence_is_not_a_failed_user_block() {
    let mut tracker = opened(ControlLimits::default());
    tracker.register(1, FIRST, RequestKind::Ordinary).unwrap();
    tracker.feed(b"%begin 0 2 1\nreply\n%end 0 2 1\n%begin 0 8 1\nunknown command: libtmux-control-fence:10101010101010101010101010101010\n%error 0 8 1\n").unwrap();
    let Some(Completion::Reply { token: 1, reply }) = tracker.next_completion() else {
        panic!("missing deferred reply")
    };
    assert!(reply.succeeded());
    assert_eq!(reply.blocks().len(), 1);
}

#[test]
fn terminators_must_match_timestamp_number_and_flags() {
    for end in [b"%end 1 2 1\n".as_slice(), b"%end 0 3 1\n", b"%end 0 2 0\n"] {
        let mut tracker = opened(ControlLimits::default());
        tracker.register(1, FIRST, RequestKind::Ordinary).unwrap();
        tracker.register(2, SECOND, RequestKind::Ordinary).unwrap();
        tracker.feed(b"%begin 0 2 1\npoison\n").unwrap();
        assert_eq!(tracker.feed(end), Err(ControlFault::WrongTerminator));
        assert!(matches!(
            tracker.next_completion(),
            Some(Completion::Lost { token: 1, .. })
        ));
        assert!(matches!(
            tracker.next_completion(),
            Some(Completion::Lost { token: 2, .. })
        ));
        assert!(tracker.register(3, FIRST, RequestKind::Ordinary).is_err());
    }
}

#[test]
fn cancellation_does_not_disable_line_reply_or_block_budgets() {
    let mut tracker = opened(ControlLimits {
        max_line_bytes: 128,
        ..ControlLimits::default()
    });
    tracker.register(1, FIRST, RequestKind::Ordinary).unwrap();
    tracker.cancel(1);
    assert_eq!(
        tracker.feed(&vec![b'x'; 129]),
        Err(ControlFault::LineLimit { limit: 128 })
    );

    let mut tracker = opened(ControlLimits {
        max_reply_bytes: 128,
        ..ControlLimits::default()
    });
    tracker.register(1, FIRST, RequestKind::Ordinary).unwrap();
    tracker.cancel(1);
    tracker.feed(b"%begin 0 2 1\n").unwrap();
    let mut line = vec![b'x'; 80];
    line.push(b'\n');
    tracker.feed(&line).unwrap();
    assert_eq!(
        tracker.feed(&line),
        Err(ControlFault::ReplyLimit { limit: 128 })
    );

    let mut tracker = opened(ControlLimits {
        max_reply_blocks: 2,
        ..ControlLimits::default()
    });
    tracker.register(1, FIRST, RequestKind::Ordinary).unwrap();
    tracker.cancel(1);
    tracker
        .feed(b"%begin 0 2 1\n%end 0 2 1\n%begin 0 3 1\n%end 0 3 1\n")
        .unwrap();
    assert_eq!(
        tracker.feed(b"%begin 0 4 1\n"),
        Err(ControlFault::BlockLimit { limit: 2 })
    );
}

#[test]
fn a_cancelled_nonempty_block_is_not_mistaken_for_the_empty_fence() {
    let mut tracker = opened(ControlLimits::default());
    tracker.register(1, FIRST, RequestKind::Ordinary).unwrap();
    tracker.cancel(1);
    tracker
        .feed(b"%begin 0 2 1\nfirst\n%end 0 2 1\n%begin 0 3 1\nsecond\n%end 0 3 1\n")
        .unwrap();
    assert_eq!(
        tracker.feed(b"unknown command: libtmux-control-fence:10101010101010101010101010101010\n"),
        Err(ControlFault::InvalidFence)
    );
}

#[test]
fn reply_bytes_are_bounded_before_a_partial_record_finishes() {
    let mut tracker = opened(ControlLimits {
        max_reply_bytes: 128,
        ..ControlLimits::default()
    });
    tracker.register(1, FIRST, RequestKind::Ordinary).unwrap();
    tracker.cancel(1);
    tracker.feed(b"%begin 0 2 1\n").unwrap();
    assert_eq!(
        tracker.feed(&vec![b'x'; 129]),
        Err(ControlFault::ReplyLimit { limit: 128 })
    );
}

#[test]
fn a_caller_reply_budget_can_narrow_but_never_enlarge_the_drain() {
    let mut tracker = opened(ControlLimits::default());
    tracker
        .register_with_reply_limit(1, FIRST, RequestKind::Ordinary, 32)
        .unwrap();
    tracker.cancel(1);
    tracker.feed(b"%begin 0 2 1\n").unwrap();
    assert_eq!(
        tracker.feed(&[b'x'; 32]),
        Err(ControlFault::ReplyLimit { limit: 32 })
    );

    let mut tracker = opened(ControlLimits {
        max_reply_bytes: 32,
        ..ControlLimits::default()
    });
    tracker
        .register_with_reply_limit(1, FIRST, RequestKind::Ordinary, usize::MAX)
        .unwrap();
    tracker.feed(b"%begin 0 2 1\n").unwrap();
    assert_eq!(
        tracker.feed(&[b'x'; 32]),
        Err(ControlFault::ReplyLimit { limit: 32 })
    );
}

#[test]
fn subscriber_overflow_never_steals_command_replies_or_pause_provenance() {
    let mut tracker = opened(ControlLimits {
        max_events: 2,
        ..ControlLimits::default()
    });
    tracker
        .register(1, FIRST, RequestKind::Pause("%0".parse().unwrap()))
        .unwrap();
    for _ in 0..20 {
        tracker.feed(b"%output %0 octal-\\377\n").unwrap();
    }
    tracker
        .feed(b"%begin 0 2 1\n%pause %0\n%end 0 2 1\n%pause %0\n")
        .unwrap();
    tracker.feed(first_fence()).unwrap();
    let Some(Completion::Reply { reply, .. }) = tracker.next_completion() else {
        panic!("reply blocked by subscriber")
    };
    assert_eq!(reply.blocks()[0].output(), b"%pause %0\n");
    for _ in 0..2 {
        assert!(
            matches!(tracker.next_notice(), Some(ControlNotice::Event(Event::Output { bytes, .. })) if bytes == b"octal-\xff")
        );
    }
    assert_eq!(
        tracker.next_notice(),
        Some(ControlNotice::Gap {
            dropped: 19,
            included_pause: true
        })
    );
    assert!(tracker.next_notice().is_none());
}

#[test]
fn a_successful_notification_shaped_reply_never_changes_flow_state() {
    let mut tracker = opened(ControlLimits::default());
    tracker.register(1, FIRST, RequestKind::Ordinary).unwrap();
    tracker
        .feed(b"%begin 0 2 1\n%pause %0\n%output %0 fake\n%end 0 2 1\n")
        .unwrap();
    tracker.feed(first_fence()).unwrap();
    assert!(tracker.next_completion().is_some());
    assert!(tracker.next_notice().is_none());
}

#[test]
fn eof_fails_only_outstanding_live_requests() {
    let mut tracker = opened(ControlLimits::default());
    tracker.register(1, FIRST, RequestKind::Ordinary).unwrap();
    tracker.register(2, SECOND, RequestKind::Ordinary).unwrap();
    tracker.cancel(1);
    tracker.feed(b"%begin 0 2 1\npartial").unwrap();
    assert_eq!(tracker.eof(), Err(ControlFault::Disconnected));
    assert!(matches!(
        tracker.next_completion(),
        Some(Completion::Drained { token: 1 })
    ));
    assert!(matches!(
        tracker.next_completion(),
        Some(Completion::Lost {
            token: 2,
            fault: ControlFault::Disconnected
        })
    ));
    assert!(tracker.next_completion().is_none());
}
