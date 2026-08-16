//! The control-mode line parser, fed arbitrary bytes.
//!
//! This reads from a tmux that keeps running, so a malformed line is not a
//! command that failed: it is bytes the parser has to survive and resynchronise
//! from. Pane output is also not required to be UTF-8, which is why the input
//! here is bytes rather than a string.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    libtmux::control::__fuzz_parse_control_line(data);
});
