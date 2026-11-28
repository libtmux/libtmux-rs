use super::tests::{FIRST, first_fence, opened};
use super::*;

fn challenge() -> (ControlProbe, ControlIdentity) {
    let probe = ControlProbe::new("$0".parse().unwrap(), CreationNonce::from_bytes([0xab; 16]));
    let identity = probe.decode_control_report(b"unknown command: libtmux-control-challenge:abababababababababababababababab:3.8,123,456,$0\n").unwrap();
    (probe, identity)
}

#[test]
fn qualification_requires_the_attached_identity_and_its_complete_fence() {
    let mut tracker = opened(ControlLimits {
        max_events: 1,
        ..ControlLimits::default()
    });
    let (probe, identity) = challenge();
    tracker
        .register_challenge(1, FIRST, probe, identity)
        .unwrap();
    for _ in 0..20 {
        tracker.feed(b"%output %0 ignored\n").unwrap();
    }
    tracker.feed(b"%begin 0 2 1\n%end 0 2 1\nunknown command: libtmux-control-challenge:abababababababababababababababab:3.8,123,456,$0\n").unwrap();
    assert!(!tracker.is_qualified());
    assert!(tracker.next_completion().is_none());
    tracker.feed(first_fence()).unwrap();
    assert!(tracker.is_qualified());
    assert!(matches!(
        tracker.next_completion(),
        Some(Completion::Reply { token: 1, .. })
    ));
}

#[test]
fn attached_generation_changes_and_duplicate_challenges_fail_closed() {
    for report in [
        b"unknown command: libtmux-control-challenge:abababababababababababababababab:3.8,124,456,$0\n".as_slice(),
        b"unknown command: libtmux-control-challenge:abababababababababababababababab:3.8,123,456,$0\nunknown command: libtmux-control-challenge:abababababababababababababababab:3.8,123,456,$0\n",
    ] {
        let mut tracker = opened(ControlLimits::default());
        let (probe, identity) = challenge();
        tracker.register_challenge(1, FIRST, probe, identity).unwrap();
        tracker.feed(b"%begin 0 2 1\n%end 0 2 1\n").unwrap();
        assert_eq!(tracker.feed(report), Err(ControlFault::InvalidChallenge));
        assert!(!tracker.is_qualified());
    }
}

#[test]
fn deferred_challenge_payload_proves_the_same_identity() {
    let mut tracker = opened(ControlLimits::default());
    let (probe, identity) = challenge();
    tracker
        .register_challenge(1, FIRST, probe, identity)
        .unwrap();
    tracker.feed(b"%begin 0 2 1\nunknown command: libtmux-control-challenge:abababababababababababababababab:3.8,123,456,$0\n%error 0 2 1\n").unwrap();
    tracker.feed(first_fence()).unwrap();
    assert!(tracker.is_qualified());
    assert!(matches!(
        tracker.next_completion(),
        Some(Completion::Reply { token: 1, .. })
    ));
}
