//! The `show-environment -s` parser, fed arbitrary bytes.
//!
//! Names and values are whatever the server inherited or a client set, so the
//! parser reads bytes nobody in this process wrote. Beyond not panicking, a
//! listing rendered as tmux renders one must parse back to its variables.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    libtmux::__fuzz_environment_listing(data);
});
