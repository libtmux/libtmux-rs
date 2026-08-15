//! Integration tests for logical commands and sanitized summaries.

use std::fmt::{Debug, Display};
use std::os::unix::ffi::OsStringExt;

use libtmux::{Command, CommandSummary};
use static_assertions::{assert_impl_all, assert_not_impl_any};

assert_impl_all!(Command: Clone, Debug, Send, Sync);
assert_impl_all!(CommandSummary: Clone, Debug, Display, Eq, Send, Sync);
assert_not_impl_any!(Command: Display, From<std::ffi::OsString>);

#[test]
fn builders_preserve_logical_public_arguments_in_the_summary() {
    let command = Command::new("display-message").arg("-p").arg("value;");
    let summary = command.summary();

    assert_eq!(summary.to_string(), r#""display-message" "-p" "value;""#);
    assert_eq!(summary.argument_count(), 2);
    assert_eq!(summary.public_argument_count(), 2);
    assert_eq!(summary.sensitive_argument_count(), 0);
}

#[test]
fn sensitive_arguments_have_one_length_independent_marker() {
    let short = Command::new("set-environment")
        .arg("TOKEN")
        .sensitive_arg("x")
        .summary();
    let long = Command::new("set-environment")
        .arg("TOKEN")
        .sensitive_arg("sentinel-secret-with-a-different-length")
        .summary();

    assert_eq!(short, long);
    assert_eq!(short.to_string(), r#""set-environment" "TOKEN" <redacted>"#);
    assert_eq!(short.argument_count(), 2);
    assert_eq!(short.public_argument_count(), 1);
    assert_eq!(short.sensitive_argument_count(), 1);
}

#[test]
fn diagnostics_escape_controls_and_non_utf8_bytes() {
    let argument = std::ffi::OsString::from_vec(b"line\n\xff\t\\\"".to_vec());
    let summary = Command::new("display-message").arg(argument).summary();

    assert_eq!(
        summary.to_string(),
        r#""display-message" "line\n\xff\t\\\"""#,
    );
}

#[test]
fn diagnostics_escape_control_and_non_utf8_subcommand_bytes() {
    let subcommand = std::ffi::OsString::from_vec(b"show\n\xff".to_vec());
    let summary = Command::new(subcommand).arg("ok").summary();

    assert_eq!(summary.to_string(), r#""show\n\xff" "ok""#);
}

#[test]
fn diagnostics_escape_unicode_directionality_bytes_to_ascii() {
    let summary = Command::new("display-message")
        .arg("visible\u{202e}hidden")
        .summary();

    assert_eq!(
        summary.to_string(),
        r#""display-message" "visible\xe2\x80\xaehidden""#,
    );
    assert!(summary.to_string().is_ascii());
}

#[test]
fn public_diagnostic_tokens_are_bounded() {
    let argument_summary = Command::new("display-message")
        .arg("a".repeat(4_096))
        .summary()
        .to_string();
    let subcommand_summary = Command::new("b".repeat(4_096)).summary().to_string();

    assert!(argument_summary.contains("<truncated>"));
    assert!(argument_summary.len() < 256);
    assert!(subcommand_summary.contains("<truncated>"));
    assert!(subcommand_summary.len() < 256);
}

#[test]
fn sensitive_arguments_are_absent_from_all_public_diagnostics() {
    let command = Command::new("set-environment")
        .arg("TOKEN")
        .sensitive_arg("sentinel-secret");
    let summary = command.summary();

    assert!(!format!("{command:?}").contains("sentinel-secret"));
    assert!(!format!("{summary:?}").contains("sentinel-secret"));
    assert!(!summary.to_string().contains("sentinel-secret"));
}
