//! The versioned filter-expression wire format, fed arbitrary JSON.
//!
//! An expression can arrive from outside the process -- a config file, a CLI
//! argument, an MCP tool call -- so the deserializer is reachable by anything
//! that can write JSON.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = serde_json::from_str::<libtmux::query::FilterExpr<libtmux::Pane>>(text);
});
