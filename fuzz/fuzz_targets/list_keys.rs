//! The `list-keys -F` decoder, fed arbitrary bytes.
//!
//! Table names, notes and commands are whatever a configuration bound, so the
//! decoder reads bytes nobody in this process wrote.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    libtmux::__fuzz_parse_key_bindings(data);
});
