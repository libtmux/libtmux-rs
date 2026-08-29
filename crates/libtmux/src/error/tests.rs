use std::error::Error as StdError;
use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;

use super::{Error, ErrorKind, SENSITIVE_OUTPUT_WITHHELD, ServerGoneKind};
use crate::Command;
use crate::command::{CommandResult, ProcessStatus, RequestId};

/// The three server-gone wordings a live fixture cannot produce on demand.
///
/// Only "no server running" is reachable from a test, because the other
/// three need the server to die between the client connecting and the
/// command finishing. They are read from tmux's `client.c`, so they are
/// asserted against the classifier rather than against tmux.
#[test]
fn a_server_that_is_not_there_is_not_a_refusal() {
    for (stderr, expected, retryable) in [
        (
            "no server running on /tmp/libtmux-rs-dev/absent",
            ServerGoneKind::NotRunning,
            true,
        ),
        (
            "error connecting to /tmp/libtmux-rs-dev/absent (Connection refused)",
            ServerGoneKind::Unreachable,
            true,
        ),
        ("server exited unexpectedly", ServerGoneKind::Lost, false),
        ("server exited", ServerGoneKind::Stopped, false),
    ] {
        let error = Error::refused("list-sessions", Some(1), stderr.to_owned(), None);
        assert_eq!(error.kind(), ErrorKind::ServerGone, "{stderr}");
        assert!(
            matches!(&error, Error::ServerGone { kind, .. } if *kind == expected),
            "{stderr} should be {expected:?}, got {error:?}",
        );
        assert!(!error.is_object_gone(), "{stderr}");
        assert_eq!(
            error.is_transient(),
            retryable,
            "only a failure before connecting proves replay safe: {stderr}",
        );
    }
}

#[test]
fn only_resource_limited_spawn_failures_invite_unchanged_replay() {
    let spawn = |kind| {
        Error::spawn(
            1,
            Command::new("display-message").summary(),
            std::io::Error::from(kind),
            false,
        )
    };

    for kind in [
        std::io::ErrorKind::Interrupted,
        std::io::ErrorKind::WouldBlock,
        std::io::ErrorKind::OutOfMemory,
        std::io::ErrorKind::ResourceBusy,
        std::io::ErrorKind::ExecutableFileBusy,
    ] {
        assert!(spawn(kind).is_transient(), "{kind:?} may clear on retry");
    }
    for kind in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::InvalidData,
    ] {
        assert!(
            !spawn(kind).is_transient(),
            "{kind:?} needs repair, not replay",
        );
    }
}

#[cfg(feature = "control-mode")]
#[test]
fn control_mode_debug_distinguishes_the_source_kind() {
    let error = Error::control_mode(std::io::Error::from(std::io::ErrorKind::PermissionDenied));

    assert_eq!(
        format!("{error:?}"),
        "ControlMode { kind: Transport, source_kind: Some(PermissionDenied) }",
    );
}

#[test]
fn an_error_after_an_effect_cannot_invite_replay_or_be_relabelled() {
    let error = Error::Overloaded {
        request_id: 7,
        command: Command::new("list-panes").summary(),
        in_flight: 1,
    }
    .after_effect("resize-pane");

    assert_eq!(error.kind(), ErrorKind::PartialEffect);
    assert!(!error.is_object_gone());
    assert!(!error.is_transient());

    let error = error.after_effect("select-pane");
    assert!(
        matches!(
            &error,
            Error::AfterEffect { operation: "resize-pane", source }
                if source.kind() == ErrorKind::Refused && source.is_transient()
        ),
        "the existing, more specific effect is the useful replay boundary: {error:?}",
    );
}

#[test]
fn an_effect_boundary_preserves_a_redacted_source_without_growing() {
    let secret = "sentinel-after-effect-secret";
    let command = Command::new("send-keys").sensitive_arg(secret);
    let result = CommandResult::new(
        RequestId::new(9),
        command.summary(),
        ProcessStatus::from_exit_status(ExitStatus::from_raw(1 << 8)),
        Vec::new(),
        format!("bad key: {secret}\n").into_bytes(),
    );
    let error = result
        .refusal_for("send-keys")
        .expect("the sensitive command was refused")
        .after_effect("send-keys")
        .after_effect("plan");

    let source = StdError::source(&error).expect("the boundary exposes its source");
    let source = source
        .downcast_ref::<Box<Error>>()
        .expect("the source owns a libtmux::Error")
        .as_ref();
    assert_eq!(source.kind(), ErrorKind::Refused);
    assert!(source.source().is_none(), "the source chain has one edge");
    assert!(matches!(
        &error,
        Error::AfterEffect {
            operation: "send-keys",
            ..
        }
    ));

    for diagnostic in [
        error.to_string(),
        format!("{error:?}"),
        source.to_string(),
        format!("{source:?}"),
    ] {
        assert!(!diagnostic.contains(secret), "{diagnostic}");
    }
}

/// The order the two server-exit wordings are read in is load-bearing.
///
/// A lost server says `server exited unexpectedly`, which starts with the
/// `server exited` of one that shut down and does not mean it.
#[test]
fn a_lost_server_is_not_read_as_one_that_stopped() {
    let error = Error::refused(
        "new-session",
        Some(1),
        "server exited unexpectedly".to_owned(),
        None,
    );
    assert!(
        matches!(&error, Error::ServerGone { kind, .. } if *kind == ServerGoneKind::Lost),
        "{error:?}",
    );
}

/// A refusal that says nothing about the server stays a refusal, so the
/// classification is not simply calling everything gone.
#[test]
fn a_refusal_that_names_no_server_stays_a_refusal() {
    let error = Error::refused(
        "delete-buffer",
        Some(1),
        "no buffer never-existed".to_owned(),
        None,
    );
    assert_eq!(error.kind(), ErrorKind::Refused, "{error:?}");
}

#[test]
fn withheld_refusal_uses_the_payload_appropriate_variant() {
    let error = Error::refused_withheld("set-option", Some(1));

    assert!(matches!(&error, Error::CommandFailed { .. }), "{error:?}");
    assert_eq!(error.kind(), ErrorKind::Refused);
}

#[test]
fn a_sensitive_mismatched_target_echo_stays_withheld() {
    let secret = "sentinel-sensitive-echo";
    let target = std::ffi::OsStr::new("%9");
    let command = Command::new("send-keys")
        .arg("-t")
        .arg(target)
        .sensitive_arg("sentinel-sensitive-input");
    let result = CommandResult::new(
        RequestId::new(1),
        command.summary(),
        ProcessStatus::from_exit_status(ExitStatus::from_raw(1 << 8)),
        Vec::new(),
        format!("can't find pane: %9 {secret}\n").into_bytes(),
    );

    let error = Error::from_refused_result("send-keys", &result, Some(target));

    assert!(
        matches!(
            &error,
            Error::CommandFailed { stderr, .. } if stderr == SENSITIVE_OUTPUT_WITHHELD
        ),
        "{error:?}",
    );
    let diagnostic = format!("{error:?} {error}");
    for sensitive in [secret, "sentinel-sensitive-input"] {
        assert!(!diagnostic.contains(sensitive), "{diagnostic}");
    }
}

/// What tmux echoes back decides whether an object died or a place is
/// empty, and only one of those lets a caller drop a handle.
///
/// A window or pane coordinate -- an index, or a window name -- is scoped
/// to one session and is not unique on the server, so its absence cannot
/// mean the object is gone. Reporting one as an identity answered
/// `is_object_gone` with `true` for a window that was alive and merely
/// renumbered, which is the one predicate a caller consults before
/// discarding a handle.
///
/// A session is the exception: `-t` takes a session's name, so a bare word
/// there is still an identity. Wording measured on tmux 3.2a to 3.7b.
#[test]
fn a_missing_coordinate_is_not_a_missing_object() {
    for (stderr, gone) in [
        // A sigil means tmux resolved a name that belongs to one object.
        ("can't find window: @99", true),
        ("can't find pane: %99", true),
        ("can't find session: $99", true),
        // A name is how tmux lets a caller target a session.
        ("can't find session: nosuchsession", true),
        // Coordinates: a place within one session, not an object.
        ("can't find window: 9", false),
        ("can't find window: nosuchname", false),
        ("can't find pane: 9", false),
    ] {
        let error = Error::refused(
            "unlink-window",
            Some(1),
            stderr.to_owned(),
            Some(std::ffi::OsStr::new("home:9")),
        );
        assert_eq!(
            error.is_object_gone(),
            gone,
            "{stderr} should report gone={gone}, got {error:?}",
        );
    }
}

/// tmux drops the session half of a coordinate, so the request keeps it.
///
/// `-t home:9` is answered by `can't find window: 9`. Reporting the echo
/// alone would leave a reader holding half a target they cannot act on.
#[test]
fn a_missing_coordinate_reports_the_target_that_was_sent() {
    let error = Error::refused(
        "unlink-window",
        Some(1),
        "can't find window: 9".to_owned(),
        Some(std::ffi::OsStr::new("home:9")),
    );
    assert!(
        matches!(&error, Error::LinkGone { target, .. } if target == "home:9"),
        "{error:?}",
    );
    assert_eq!(error.to_string(), "tmux has no window at home:9");
}
