use super::*;
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};

fn argv(command: &Command) -> Vec<OsString> {
    let mut argv = Vec::new();
    command.extend_argv(&mut argv);
    argv
}

#[test]
fn command_lowering_escapes_each_final_semicolon_without_changing_logical_text() {
    for (logical, physical) in [
        (";", "\\;"),
        ("value;", "value\\;"),
        ("a;b", "a;b"),
        ("\\;", "\\\\;"),
        (";;", ";\\;"),
        ("plain", "plain"),
    ] {
        let command = Command::new("display-message").arg(logical);
        assert_eq!(
            argv(&command),
            ["display-message", physical].map(OsString::from)
        );
        assert_eq!(command.logical_words().last(), Some(OsStr::new(logical)));
    }
    let command = Command::new(";").arg("first;").arg("after");
    assert_eq!(
        argv(&command),
        ["\\;", "first\\;", "after"].map(OsString::from)
    );
    assert_eq!(command.summary().to_string(), r#"";" "first;" "after""#);
}

#[cfg(unix)]
#[test]
fn sensitive_arguments_keep_bytes_in_argv_and_disappear_from_every_diagnostic() {
    let command = Command::new("set-environment")
        .arg("TOKEN")
        .sensitive_arg(OsString::from_vec(b"\xffsentinel-secret;".to_vec()));
    let arguments = argv(&command);
    assert_eq!(arguments[2].as_bytes(), b"\xffsentinel-secret\\;");
    assert_eq!(command.summary().sensitive_argument_count(), 1);
    assert_eq!(command.summary().public_argument_count(), 1);
    assert!(command.control_mode_line().is_none());
    let sensitive = CommandArg::sensitive(OsString::from("sentinel-secret"));
    let chain = CommandChain::new(command.clone()).then(Command::new("list-sessions"));
    for diagnostic in [
        format!("{sensitive:?}"),
        format!("{command:?}"),
        format!("{chain:?}"),
        command.summary().to_string(),
        format!("{:?}", command.summary()),
    ] {
        assert!(!diagnostic.contains("sentinel-secret"));
        assert!(!diagnostic.contains("\\xff"));
    }
}

#[test]
fn a_chain_keeps_authored_boundaries_apart_from_literal_semicolons() {
    let chain = CommandChain::new(Command::new("display-message").arg(";").arg("value;"))
        .then(Command::new("list-sessions"));
    let summary = chain.summary();
    assert_eq!(
        summary.to_string(),
        r#""display-message" ";" "value;" ; "list-sessions""#
    );
    assert_eq!(summary.public_argument_count(), 3);
    assert_eq!(
        chain.control_mode_line(),
        Some(r#"display-message ";" "value;" ; list-sessions"#.into())
    );
    let global = [
        OsString::from("-S"),
        OsString::from("/tmp/libtmux-rs-dev/example"),
    ];
    let (arguments, logical_index) = chain.into_argv(&global);
    assert_eq!(logical_index, 2);
    assert_eq!(
        arguments,
        [
            "-S",
            "/tmp/libtmux-rs-dev/example",
            "display-message",
            "\\;",
            "value\\;",
            ";",
            "list-sessions",
        ]
        .map(OsString::from)
    );
    assert_eq!(arguments.iter().filter(|word| *word == ";").count(), 1);
}

#[test]
fn command_metadata_keeps_the_strongest_requirement_and_refused_intervals() {
    use crate::{ReleaseSuffix, ReleaseVersion};

    let lower = ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL);
    let higher = ReleaseVersion::new(3, 5, ReleaseSuffix::FINAL);
    let command = Command::new("set-buffer")
        .requires_tmux("first", higher)
        .requires_tmux("lower", lower)
        .refusing_interval("regression", lower, higher);
    assert_eq!(command.version_requirement(), Some(("first", higher)));
    assert_eq!(
        command.refused_intervals(),
        &[("regression", lower, higher)]
    );
    let chain = CommandChain::new(command).then(Command::new("list-sessions"));
    assert_eq!(
        chain
            .commands()
            .filter_map(Command::version_requirement)
            .collect::<Vec<_>>(),
        vec![("first", higher)]
    );
}

#[test]
fn new_session_commands_preserve_format_intent_and_sensitive_operands() {
    let operation = crate::NewSession::new(crate::TmuxArg::format("#{session_name}"))
        .environment("TOKEN", "sentinel-secret")
        .command("private-command;");
    let command = Command::new_session(&operation, "#{session_id}");
    assert_eq!(argv(&command)[6], "#{session_name}");
    assert_eq!(command.summary().sensitive_argument_count(), 2);
    assert!(!format!("{command:?}").contains("sentinel-secret"));
    assert!(!format!("{command:?}").contains("private-command"));
}

#[cfg(unix)]
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

#[test]
fn control_records_refuse_embedded_delimiters_and_bound_quoting_before_rendering() {
    for value in ["line\nnext", "line\rnext", "nul\0tail"] {
        assert!(
            Command::new("display-message")
                .arg(value)
                .control_mode_line()
                .is_none()
        );
        assert!(Command::new(value).control_mode_line().is_none());
    }
    let command = Command::new("display-message").arg("a\"b");
    let line = command.control_mode_line().unwrap();
    assert_eq!(
        command.control_mode_line_bounded(line.len()).as_deref(),
        Some(line.as_str())
    );
    assert!(command.control_mode_line_bounded(line.len() - 1).is_none());
    assert!(
        Command::new("display-message")
            .arg("x".repeat(4096))
            .control_mode_line_bounded(64)
            .is_none()
    );
    let chain = CommandChain::new(Command::new("one")).then(Command::new("two").arg("space here"));
    let line = chain.control_mode_line().unwrap();
    assert_eq!(
        chain.control_mode_line_bounded(line.len()).as_deref(),
        Some(line.as_str())
    );
    assert!(chain.control_mode_line_bounded(line.len() - 1).is_none());
}
