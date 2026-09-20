//! tmux-mcp's escape-sequence filter, fed arbitrary pane output.
//!
//! The filter is private to tmux-mcp and needs only `std`, so its source is
//! compiled in here rather than exported through a feature of a published
//! crate. A later `use crate::` in it breaks this build, not the filter.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[allow(dead_code, reason = "compiled from tmux-mcp, which uses all of it")]
#[path = "../../crates/tmux-mcp/src/text.rs"]
mod text;

use text::TextFilter;

/// Printable bytes written once tmux is back in its ground state.
const RESYNC_TEXT: &[u8] = b"resync";

fn rendered(chunks: &[&[u8]]) -> Vec<u8> {
    let mut filter = TextFilter::new();
    let mut out = Vec::new();
    for chunk in chunks {
        filter.push(chunk, &mut out);
    }
    out
}

fuzz_target!(|data: &[u8]| {
    let whole = rendered(&[data]);

    // tmux reports output in chunks of its own choosing, so where one ends
    // must not change the text.
    let split = data.first().map_or(0, |&byte| usize::from(byte) % (data.len() + 1));
    let (head, tail) = data.split_at(split);
    assert_eq!(rendered(&[head, tail]), whole, "split at {split}");

    // Text written once tmux is back in `ground` is on its screen, so it must
    // be in the filter's output. CAN then `ESC \` gets there from every state:
    // CAN is data in `dcs_handler` and `dcs_escape`, and `ESC \` ends those.
    assert_resyncs(data, b"\x18\x1b\\");

    // Only `ESC P` reaches device control data. Without a `P`, every state
    // honours `INPUT_STATE_ANYWHERE`, so CAN, SUB or a whole escape sequence
    // -- the next prompt's colour -- ends whatever was open.
    if !data.contains(&b'P') {
        for resync in [&b"\x18"[..], b"\x1a", b"\x1b[m"] {
            assert_resyncs(data, resync);
        }
    }
});

fn assert_resyncs(data: &[u8], resync: &[u8]) {
    let mut input = data.to_vec();
    input.extend_from_slice(resync);
    input.extend_from_slice(RESYNC_TEXT);
    let text = rendered(&[&input]);
    assert!(
        text.ends_with(RESYNC_TEXT),
        "text after {resync:?} was swallowed: {:?}",
        String::from_utf8_lossy(&text)
    );
}
