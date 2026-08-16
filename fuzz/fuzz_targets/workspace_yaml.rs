//! The tmuxp-style workspace loader, fed arbitrary YAML.
//!
//! A workspace file is hand-written and comes from outside, and the loader
//! walks a nested document deciding what each value means.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = tmux_workspace::Workspace::from_yaml(text);
});
