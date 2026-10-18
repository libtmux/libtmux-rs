use std::borrow::Cow;
use std::error::Error as StdError;
use std::ffi::OsString;
use std::fmt::Display;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::ExitStatusExt;

use static_assertions::{assert_impl_all, assert_not_impl_any};

use super::*;

assert_impl_all!(CommandArg: Send, Sync);
assert_impl_all!(CommandRequest: Send, Sync);
assert_impl_all!(CommandResult: Send, Sync);
assert_impl_all!(RequestId: Send, Sync);
assert_impl_all!(ProcessStatus: Send, Sync);
assert_not_impl_any!(CommandResult: Display);
assert_not_impl_any!(CommandRequest: Clone);

fn argv_bytes(request: &CommandRequest) -> Vec<Vec<u8>> {
    request
        .argv()
        .iter()
        .map(|argument| argument.as_os_str().as_bytes().to_vec())
        .collect()
}

fn exit_status(code: i32) -> ExitStatus {
    ExitStatus::from_raw(code << 8)
}

fn command_result(
    command: Command,
    status: ExitStatus,
    stdout: &[u8],
    stderr: &[u8],
) -> CommandResult {
    let request = CommandRequest::new(RequestId::new(7), command);
    CommandResult::new(
        request.request_id(),
        request.summary().clone(),
        ProcessStatus::from_exit_status(status),
        stdout.to_vec(),
        stderr.to_vec(),
    )
}

#[test]
fn command_results_classify_refusals_without_exposing_sensitive_output() {
    let success = command_result(Command::new("select-pane"), exit_status(0), b"", b"");
    assert!(success.refusal_for("select-pane").is_none());

    let server_gone = command_result(
        Command::new("select-pane"),
        exit_status(1),
        b"",
        b"no server running on /tmp/libtmux-rs-dev/absent\n",
    )
    .refusal_for("select-pane")
    .expect("a nonzero result is a refusal");
    assert!(matches!(server_gone, crate::Error::ServerGone { .. }));

    let object_gone = command_result(
        Command::new("select-pane").arg("-t").arg("%77"),
        exit_status(1),
        b"",
        b"can't find pane: %77\n",
    )
    .refusal_for("select-pane")
    .expect("the target was refused");
    assert_eq!(object_gone.kind(), crate::ErrorKind::ObjectGone);

    let secret = "sentinel-command-result-secret";
    let sensitive = command_result(
        Command::new("send-keys").sensitive_arg(secret),
        exit_status(1),
        b"",
        format!("bad key: {secret}\n").as_bytes(),
    );
    let refusal = sensitive
        .refusal_for("send-keys")
        .expect("the sensitive command was refused");
    let diagnostic = format!("{refusal:?} {refusal}");
    assert!(matches!(refusal, crate::Error::CommandFailed { .. }));
    assert!(!diagnostic.contains(secret), "{diagnostic}");
}

#[test]
fn dispatch_lowering_escapes_only_each_final_semicolon() {
    let cases: &[(&[u8], &[u8])] = &[
        (b";", b"\\;"),
        (b"value;", b"value\\;"),
        (b"a;b", b"a;b"),
        (b"\\;", b"\\\\;"),
        (b"\\\\;", b"\\\\\\;"),
        (b";;", b";\\;"),
        (b"plain", b"plain"),
    ];

    for (logical, physical) in cases {
        let argument = OsString::from_vec(logical.to_vec());
        let request = CommandRequest::new(RequestId::new(11), Command::new("cmd").arg(argument));
        assert_eq!(argv_bytes(&request), [b"cmd".to_vec(), physical.to_vec()]);
    }
}

#[test]
fn dispatch_lowering_applies_to_every_argv_position() {
    let request = CommandRequest::new(
        RequestId::new(12),
        Command::new(";")
            .arg("first;")
            .arg("middle;")
            .arg("last;")
            .arg("after"),
    );

    assert_eq!(
        argv_bytes(&request),
        [
            b"\\;".to_vec(),
            b"first\\;".to_vec(),
            b"middle\\;".to_vec(),
            b"last\\;".to_vec(),
            b"after".to_vec(),
        ],
    );
}

#[test]
fn dispatch_lowering_preserves_non_utf8_prefixes() {
    let logical = OsString::from_vec(b"\xffvalue;".to_vec());
    let request = CommandRequest::new(RequestId::new(13), Command::new(logical));

    assert_eq!(argv_bytes(&request), [b"\xffvalue\\;".to_vec()]);
}

#[test]
fn command_summary_remains_logical_after_dispatch_lowering() {
    let request = CommandRequest::new(
        RequestId::new(14),
        Command::new("display-message").arg("value;"),
    );

    assert_eq!(
        request.summary().to_string(),
        r#""display-message" "value;""#
    );
    assert_eq!(
        argv_bytes(&request),
        [b"display-message".to_vec(), b"value\\;".to_vec()]
    );
}

#[test]
fn request_preserves_the_dispatch_id_supplied_by_the_executor_owner() {
    let command = Command::new("display-message").arg("same");
    let cloned = command.clone();
    assert_eq!(command.summary(), cloned.summary());

    let first = CommandRequest::new(RequestId::new(101), command);
    let second = CommandRequest::new(RequestId::new(102), cloned);

    assert_eq!(first.request_id(), RequestId::new(101));
    assert_eq!(second.request_id(), RequestId::new(102));
}

#[test]
fn sensitive_argument_lowering_preserves_bytes_while_diagnostics_redact() {
    let sensitive = OsString::from_vec(b"\xffsentinel-secret;".to_vec());
    let request = CommandRequest::new(
        RequestId::new(103),
        Command::new("set-environment")
            .arg("TOKEN")
            .sensitive_arg(sensitive),
    );

    assert_eq!(
        argv_bytes(&request),
        [
            b"set-environment".to_vec(),
            b"TOKEN".to_vec(),
            b"\xffsentinel-secret\\;".to_vec(),
        ],
    );
    for diagnostic in [
        format!("{request:?}"),
        format!("{:?}", request.summary()),
        request.summary().to_string(),
    ] {
        assert!(!diagnostic.contains("sentinel-secret"));
        assert!(!diagnostic.contains("\\xff"));
    }
}

#[test]
fn process_status_preserves_exit_and_signal_outcomes() {
    let success = ProcessStatus::from_exit_status(exit_status(0));
    let failure = ProcessStatus::from_exit_status(exit_status(7));
    let signal = ProcessStatus::from_exit_status(ExitStatus::from_raw(15));

    assert!(success.success());
    assert_eq!(success.code(), Some(0));
    assert_eq!(success.signal(), None);
    assert!(!failure.success());
    assert_eq!(failure.code(), Some(7));
    assert_eq!(failure.signal(), None);
    assert!(!signal.success());
    assert_eq!(signal.code(), None);
    assert_eq!(signal.signal(), Some(15));
}

#[test]
fn command_results_preserve_exact_output_bytes_and_trailing_blank_lines() {
    let result = command_result(
        Command::new("display-message"),
        exit_status(0),
        b"first\n\n",
        b"warning\n\n",
    );

    assert_eq!(result.request_id(), 7);
    assert_eq!(result.command().to_string(), r#""display-message""#);
    assert_eq!(result.stdout(), b"first\n\n");
    assert_eq!(result.stderr(), b"warning\n\n");
    let (stdout, stderr) = result.into_streams();
    assert_eq!(stdout, b"first\n\n");
    assert_eq!(stderr, b"warning\n\n");
}

#[test]
fn command_results_offer_borrowed_strict_and_named_lossy_views() {
    let valid = command_result(Command::new("show-messages"), exit_status(0), b"ok\n", b"");
    assert_eq!(valid.stdout_utf8().expect("fixture is UTF-8"), "ok\n");
    assert!(matches!(valid.stdout_lossy(), Cow::Borrowed("ok\n")));

    let invalid = command_result(
        Command::new("show-messages"),
        exit_status(0),
        b"before\xffafter",
        b"error\xfe",
    );
    assert_eq!(
        invalid
            .stdout_utf8()
            .expect_err("fixture is not UTF-8")
            .valid_up_to(),
        6,
    );
    assert_eq!(
        invalid
            .stderr_utf8()
            .expect_err("fixture is not UTF-8")
            .valid_up_to(),
        5,
    );
    assert!(matches!(invalid.stdout_lossy(), Cow::Owned(_)));
    assert!(matches!(invalid.stderr_lossy(), Cow::Owned(_)));
}

#[test]
fn borrowed_utf8_errors_never_own_or_debug_rejected_output() {
    let result = command_result(
        Command::new("show-messages"),
        exit_status(0),
        b"sentinel-secret\xff",
        b"",
    );
    let error = result
        .stdout_utf8()
        .expect_err("fixture contains an invalid UTF-8 byte");

    assert!(!format!("{error:?}").contains("sentinel-secret"));
    assert!(!error.to_string().contains("sentinel-secret"));
    assert!(StdError::source(&error).is_none());
}

#[test]
fn nonzero_has_session_output_is_not_mirrored_or_promoted_to_an_error() {
    let result = command_result(
        Command::new("has-session").arg("-t").arg("missing"),
        exit_status(1),
        b"",
        b"can't find session: missing\n",
    );

    assert!(!result.success());
    assert_eq!(result.exit_code(), Some(1));
    assert_eq!(result.stdout(), b"");
    assert_eq!(result.stderr(), b"can't find session: missing\n");
}

#[test]
fn sensitive_arguments_and_output_are_absent_from_private_debug_surfaces() {
    let sensitive = CommandArg::sensitive(OsString::from("sentinel-secret"));
    let command = Command::new("set-environment")
        .arg("TOKEN")
        .sensitive_arg("sentinel-secret");
    let summary = command.summary();
    let result = command_result(
        command.clone(),
        exit_status(7),
        b"sentinel-secret stdout",
        b"sentinel-secret stderr",
    );

    for diagnostic in [
        format!("{sensitive:?}"),
        format!("{command:?}"),
        format!("{summary:?}"),
        summary.to_string(),
        format!("{result:?}"),
    ] {
        assert!(!diagnostic.contains("sentinel-secret"));
    }
    assert!(
        result
            .stdout()
            .windows(15)
            .any(|bytes| bytes == b"sentinel-secret")
    );
    assert!(
        result
            .stderr()
            .windows(15)
            .any(|bytes| bytes == b"sentinel-secret")
    );
    let debug = format!("{result:?}");
    assert!(debug.contains("stdout_len"));
    assert!(debug.contains("stderr_len"));
    assert!(debug.contains("<redacted>"));
}

fn chain_request(chain: CommandChain) -> CommandRequest {
    CommandRequest::chain_with_global_argv(RequestId::new(200), &[], chain)
}

#[test]
fn a_chain_separates_its_members_with_a_bare_semicolon_argv_token() {
    let request = chain_request(
        CommandChain::new(Command::new("select-pane").arg("-m"))
            .then(Command::new("send-keys").arg("-t").arg("{marked}")),
    );

    assert_eq!(
        argv_bytes(&request),
        [
            b"select-pane".to_vec(),
            b"-m".to_vec(),
            b";".to_vec(),
            b"send-keys".to_vec(),
            b"-t".to_vec(),
            b"{marked}".to_vec(),
        ],
    );
}

#[test]
fn a_caller_supplied_semicolon_never_becomes_a_chain_separator() {
    // tmux reads a bare `;` element as a boundary and `\;` as a literal.
    // Every caller token is lowered, so the only bare `;` in the argv is
    // the one the chain itself authored.
    let request = chain_request(
        CommandChain::new(Command::new("display-message").arg(";").arg("kill-server;"))
            .then(Command::new("list-sessions")),
    );

    assert_eq!(
        argv_bytes(&request),
        [
            b"display-message".to_vec(),
            b"\\;".to_vec(),
            b"kill-server\\;".to_vec(),
            b";".to_vec(),
            b"list-sessions".to_vec(),
        ],
    );
    let bare_separators = argv_bytes(&request)
        .into_iter()
        .filter(|token| token == b";")
        .count();
    assert_eq!(bare_separators, 1);
}

#[test]
fn a_single_command_chain_matches_that_command_dispatched_alone() {
    let alone = CommandRequest::new(RequestId::new(201), Command::new("list-panes").arg("-a"));
    let chained = chain_request(CommandChain::new(Command::new("list-panes").arg("-a")));

    assert_eq!(argv_bytes(&alone), argv_bytes(&chained));
    assert_eq!(alone.summary().to_string(), chained.summary().to_string());
}

#[test]
fn global_argv_precedes_the_whole_chain_exactly_once() {
    let global = [OsString::from("-S"), OsString::from("/tmp/socket")];
    let request = CommandRequest::chain_with_global_argv(
        RequestId::new(202),
        &global,
        CommandChain::new(Command::new("a")).then(Command::new("b")),
    );

    assert_eq!(
        argv_bytes(&request),
        [
            b"-S".to_vec(),
            b"/tmp/socket".to_vec(),
            b"a".to_vec(),
            b";".to_vec(),
            b"b".to_vec(),
        ],
    );
    assert_eq!(request.logical_subcommand_index(), 2);
}

#[test]
fn a_chain_summary_distinguishes_a_separator_from_a_literal_semicolon() {
    let summary = CommandChain::new(Command::new("display-message").arg(";"))
        .then(Command::new("list-sessions"))
        .summary();

    // The boundary renders bare; the argument renders quoted.
    assert_eq!(
        summary.to_string(),
        r#""display-message" ";" ; "list-sessions""#
    );
    // A separator is structure, so it counts as neither kind of argument.
    assert_eq!(summary.public_argument_count(), 2);
    assert_eq!(summary.sensitive_argument_count(), 0);
    assert_eq!(summary.argument_count(), 2);
}

#[test]
fn chained_sensitive_arguments_dispatch_exactly_and_stay_redacted() {
    let chain = CommandChain::new(
        Command::new("set-environment")
            .arg("TOKEN")
            .sensitive_arg("sentinel-secret"),
    )
    .then(Command::new("list-sessions"));
    let request = chain_request(chain.clone());

    assert_eq!(
        argv_bytes(&request),
        [
            b"set-environment".to_vec(),
            b"TOKEN".to_vec(),
            b"sentinel-secret".to_vec(),
            b";".to_vec(),
            b"list-sessions".to_vec(),
        ],
    );
    assert_eq!(chain.summary().sensitive_argument_count(), 1);
    for diagnostic in [
        format!("{chain:?}"),
        format!("{request:?}"),
        chain.summary().to_string(),
    ] {
        assert!(!diagnostic.contains("sentinel-secret"));
    }
}

#[test]
fn command_arg_public_debug_is_ascii_escaped_and_bounded() {
    let escaped = CommandArg::public(OsString::from_vec(b"line\n\xff".to_vec()));
    let bounded = CommandArg::public(OsString::from_vec(vec![b'a'; 128]));
    let escaped_debug = format!("{escaped:?}");
    let bounded_debug = format!("{bounded:?}");

    assert!(escaped_debug.is_ascii());
    assert!(escaped_debug.contains("\\n"));
    assert!(escaped_debug.contains("\\xff"));
    assert!(bounded_debug.contains("<truncated>"));
    assert!(bounded_debug.len() < 256);
}

/// tmux answers `parse error: syntax error` and runs nothing, so the cases
/// come from `cmd-parse.y`: a word starting with `%` is a condition unless
/// it is all `%` and digits.
#[cfg(feature = "control-mode")]
#[test]
fn a_percent_token_is_quoted_unless_tmux_reads_it_as_a_pane_id() {
    let line = |command: Command| command.control_mode_line().expect("renders");

    // All digits after the percent: a pane id, and tmux wants it bare.
    assert_eq!(
        line(Command::new("list-panes").arg("-t").arg("%1")),
        "list-panes -t %1",
    );

    // Anything else beginning with a percent opens a condition, so it has
    // to be quoted or tmux refuses the whole line.
    assert_eq!(
        line(Command::new("refresh-client").arg("-A").arg("%1:off")),
        r#"refresh-client -A "%1:off""#,
    );
    assert_eq!(
        line(Command::new("display-message").arg("%if")),
        r#"display-message "%if""#,
    );

    // A percent that does not start the word was never a condition.
    assert_eq!(
        line(Command::new("display-message").arg("x%1:off")),
        "display-message x%1:off",
    );
}
