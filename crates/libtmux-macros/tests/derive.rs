//! Compile-time contract tests for the `Filterable` derive.
//!
//! The compile-fail cases pin rustc's own diagnostics beside the derive's, and
//! rustc rewords those between releases. So they run only on the toolchain
//! `rust-toolchain.toml` pins, and `just macros-ui-bless` rewrites them for
//! it. Any other compiler, the MSRV run among them, checks the passing cases
//! alone.

use std::path::Path;
use std::process::Command;

#[test]
fn filterable_ui_contract() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/*.rs");
    match (pinned_release(), running_release()) {
        (Some(pinned), Some(running)) if pinned == running => {
            tests.compile_fail("tests/ui/fail/*.rs");
        }
        (pinned, running) => {
            eprintln!("compile-fail cases skipped: they pin rustc {pinned:?}, this is {running:?}");
        }
    }
}

/// The `channel` `rust-toolchain.toml` names, absent from a packaged crate.
fn pinned_release() -> Option<String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rust-toolchain.toml");
    let text = std::fs::read_to_string(manifest).ok()?;
    text.lines().find_map(|line| {
        let value = line.trim().strip_prefix("channel")?.trim_start();
        Some(value.strip_prefix('=')?.trim().trim_matches('"').to_owned())
    })
}

/// The release `rustc -V` reports here, which is the compiler trybuild runs.
fn running_release() -> Option<String> {
    let output = Command::new("rustc").arg("-V").output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    text.split_whitespace().nth(1).map(ToOwned::to_owned)
}
