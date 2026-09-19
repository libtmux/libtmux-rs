//! The format-row codec every `list-*` result is decoded through, fed
//! arbitrary bytes.
//!
//! Names, paths and titles in those rows are written by users and programs,
//! so the bytes are not this crate's. Beyond not panicking, the same bytes are
//! encoded as tmux prints them and must decode back to themselves.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    libtmux::__fuzz_format_rows(data);
});
