//! The versioned filter-expression wire format, fed arbitrary JSON.
//!
//! An expression can arrive from outside the process -- a config file, a CLI
//! argument, an MCP tool call -- so the deserializer is reachable by anything
//! that can write JSON. Whatever it accepts must serialize, and read back as
//! the same expression.
#![no_main]

use libfuzzer_sys::fuzz_target;
use libtmux::query::FilterExpr;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(expression) = serde_json::from_str::<FilterExpr<libtmux::Pane>>(text) else {
        return;
    };
    let written = serde_json::to_string(&expression);
    assert!(written.is_ok(), "accepted but not written: {written:?}");
    let Ok(written) = written else {
        return;
    };
    let read = serde_json::from_str::<FilterExpr<libtmux::Pane>>(&written);
    assert!(read.is_ok(), "{written} did not read back: {read:?}");
    assert_eq!(read.ok(), Some(expression), "{written}");
});
