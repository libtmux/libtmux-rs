//! Native control framing exercised through the shared bounded tracker.

use libtmux_core::CreationNonce;
use libtmux_core::control::{ControlLimits, RequestKind, Tracker};

/// Feed arbitrary chunks through the same tracker used by both native drivers.
#[doc(hidden)]
pub fn __fuzz_control_blocks(data: &[u8]) {
    let Some((&header, bytes)) = data.split_first() else {
        return;
    };
    let limits = ControlLimits {
        max_line_bytes: 32 * (1 + usize::from(header >> 4)),
        max_reply_bytes: 4096,
        max_reply_blocks: 16,
        max_pending_requests: 8,
        max_events: 8,
        max_event_bytes: 1024,
        ..ControlLimits::default()
    };
    let Ok(session) = "$0".parse() else {
        return;
    };
    let mut tracker = Tracker::new(session, limits);
    let _ = tracker.feed(b"%begin 1 1 0\n%end 1 1 0\n%session-changed $0 fuzz\n");
    while tracker.next_completion().is_some() {}
    for token in 0..u64::from(header & 7) {
        let mut nonce = [0; 16];
        nonce[..8].copy_from_slice(&token.to_le_bytes());
        if tracker
            .register(
                token,
                CreationNonce::from_bytes(nonce),
                RequestKind::Ordinary,
            )
            .is_err()
        {
            break;
        }
        if header & 0x80 != 0 {
            tracker.cancel(token);
        }
    }
    for chunk in bytes.chunks(1 + usize::from(header & 31)) {
        if tracker.feed(chunk).is_err() {
            break;
        }
        while tracker.next_completion().is_some() {}
        if header & 0x40 != 0 {
            while tracker.next_notice().is_some() {}
        }
    }
    let _ = tracker.eof();
}
