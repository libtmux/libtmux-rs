//! Control-mode block framing, fed arbitrary output from tmux.
//!
//! `control_line` classifies one line at a time. This reads a stream the way
//! the connection does, inside and outside `%begin` blocks, and hands each
//! closed block to the reply slots that assemble a chain's answer: no line may
//! escape its block, and no reply may hold another command's blocks.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    libtmux::control::__fuzz_control_blocks(data);
});
