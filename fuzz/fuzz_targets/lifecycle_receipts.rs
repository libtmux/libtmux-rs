//! Creation and ownership receipts accept arbitrary bytes and round-trip valid input.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    libtmux::__fuzz_lifecycle_receipts(data);
});
