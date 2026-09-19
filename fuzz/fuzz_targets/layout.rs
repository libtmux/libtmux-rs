//! Serialized tmux layouts, including checksums and nested cells.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    libtmux::__fuzz_layout(data);
});
