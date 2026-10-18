use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

use super::{AccessMode, NewSessionOptions, Principal, Server, parse_access_rule};
use crate::command::{CommandRequest, CommandResult, ProcessStatus};
use crate::formats::{DecoderKind, FormatDescriptor, FormatPlan, ListProfile};
use crate::internal::executor::{DispatchFuture, Executor, ShutdownFuture};
use crate::{Error, ErrorKind, TmuxVersion};

struct RefusingExecutor {
    calls: AtomicUsize,
}

#[derive(Clone, Copy)]
enum SessionFollowup {
    DispatchError,
    EmptyListing,
}

struct ComposedSessionExecutor {
    calls: AtomicUsize,
    sessions_stdout: Vec<u8>,
    followup: SessionFollowup,
}

impl Executor for ComposedSessionExecutor {
    fn execute(&self, request: CommandRequest) -> DispatchFuture {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let sessions_stdout = self.sessions_stdout.clone();
        let followup = self.followup;
        DispatchFuture::new(move || {
            if call == 3 && matches!(followup, SessionFollowup::DispatchError) {
                return Err(Error::Overloaded {
                    request_id: request.request_id().get(),
                    command: request.summary().clone(),
                    in_flight: 1,
                });
            }

            let stdout = match call {
                0 => b"tmux 3.7b\n".to_vec(),
                1 => sessions_stdout,
                2 | 3 => Vec::new(),
                _ => panic!("one probe, one listing, one mutation, and one refresh"),
            };
            Ok(CommandResult::new(
                request.request_id(),
                request.summary().clone(),
                ProcessStatus::from_exit_status(ExitStatus::from_raw(0)),
                stdout,
                Vec::new(),
            ))
        })
    }

    fn shutdown(&self) -> ShutdownFuture {
        ShutdownFuture::new(|| Ok(()))
    }
}

fn default_format_value(descriptor: &FormatDescriptor) -> &'static [u8] {
    match descriptor.name() {
        "session_id" => b"$1",
        "window_id" => b"@1",
        "pane_id" => b"%1",
        "client_name" => b"client",
        _ => match descriptor.decoder() {
            DecoderKind::Ascii => b"ascii",
            DecoderKind::Text => b"text",
            DecoderKind::Bool
            | DecoderKind::U8
            | DecoderKind::U32
            | DecoderKind::U64
            | DecoderKind::I32
            | DecoderKind::Timestamp
            | DecoderKind::PaneProgress => b"0",
            DecoderKind::SessionId => b"$1",
            DecoderKind::WindowId => b"@1",
            DecoderKind::PaneId => b"%1",
            DecoderKind::PaneProgressState => b"normal",
        },
    }
}

fn session_listing_stdout() -> Vec<u8> {
    let version = TmuxVersion::parse_output(b"tmux 3.7b\n").expect("fixture version");
    let plan = FormatPlan::for_profile(ListProfile::Sessions, &version);
    let mut stdout = Vec::new();
    for descriptor in plan.descriptors_for_test() {
        for byte in default_format_value(descriptor) {
            if matches!(*byte, b'\\' | b'%' | b'=') {
                stdout.push(b'\\');
            }
            stdout.push(*byte);
        }
        stdout.push(b'=');
    }
    stdout.push(b'\n');
    stdout
}

impl Executor for RefusingExecutor {
    fn execute(&self, request: CommandRequest) -> DispatchFuture {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        DispatchFuture::new(move || {
            let (status, stdout, stderr) = if call == 0 {
                (0, b"tmux 3.7b\n".to_vec(), Vec::new())
            } else {
                assert_eq!(call, 1, "one probe and one run-shell dispatch");
                assert_eq!(request.summary().sensitive_argument_count(), 1);
                (1 << 8, Vec::new(), b"sentinel-run-shell-output\n".to_vec())
            };
            Ok(CommandResult::new(
                request.request_id(),
                request.summary().clone(),
                ProcessStatus::from_exit_status(ExitStatus::from_raw(status)),
                stdout,
                stderr,
            ))
        })
    }

    fn shutdown(&self) -> ShutdownFuture {
        ShutdownFuture::new(|| Ok(()))
    }
}

struct BlockingShutdownExecutor {
    closed: AtomicBool,
    shutdown_started: mpsc::Sender<()>,
    release: Arc<(Mutex<bool>, Condvar)>,
}

impl Executor for BlockingShutdownExecutor {
    fn execute(&self, request: CommandRequest) -> DispatchFuture {
        let closed = self.closed.load(Ordering::SeqCst);
        DispatchFuture::new(move || {
            assert!(
                closed,
                "test dispatch occurs only after shutdown closes admission"
            );
            Err(Error::executor_shutdown(
                request.request_id().get(),
                request.summary().clone(),
            ))
        })
    }

    fn shutdown(&self) -> ShutdownFuture {
        self.closed.store(true, Ordering::SeqCst);
        self.shutdown_started
            .send(())
            .expect("shutdown is observed");
        let release = Arc::clone(&self.release);
        ShutdownFuture::new(move || {
            let (value, changed) = &*release;
            let mut released = value.lock().expect("test release mutex");
            while !*released {
                released = changed.wait(released).expect("test release mutex");
            }
            Ok(())
        })
    }
}

#[test]
fn detaching_one_server_shutdown_keeps_all_clones_closed_until_later_completion() {
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let (shutdown_started, started) = mpsc::channel();
    let executor = Arc::new(BlockingShutdownExecutor {
        closed: AtomicBool::new(false),
        shutdown_started,
        release: Arc::clone(&release),
    });
    let server = Server::from_executor_for_test(executor.clone());
    let shutdown_server = server.clone();
    let (finished, completion) = mpsc::channel();
    let shutdown = std::thread::spawn(move || {
        finished
            .send(shutdown_server.shutdown())
            .expect("completion is observed");
    });
    started
        .recv_timeout(Duration::from_secs(5))
        .expect("shutdown starts");
    drop(shutdown);

    assert!(matches!(
        server.cmd(crate::Command::new("display-message")),
        Err(Error::ExecutorShutdown { .. })
    ));
    *release.0.lock().expect("test release mutex") = true;
    release.1.notify_all();
    server
        .clone()
        .shutdown()
        .expect("later clone completes shutdown");
    completion
        .recv_timeout(Duration::from_secs(5))
        .expect("detached shutdown completes")
        .expect("shutdown succeeds");
}

#[test]
fn run_shell_failure_withholds_sensitive_output() {
    let server = Server::from_executor_for_test(Arc::new(RefusingExecutor {
        calls: AtomicUsize::new(0),
    }));

    let error = server
        .run_shell("sentinel-run-shell-command")
        .expect_err("the command is refused");
    let diagnostic = format!("{error:?} {error}");
    for secret in ["sentinel-run-shell-command", "sentinel-run-shell-output"] {
        assert!(!diagnostic.contains(secret), "{diagnostic}");
    }
}

#[test]
fn session_refresh_failure_after_rename_marks_the_completed_effect() {
    let executor = Arc::new(ComposedSessionExecutor {
        calls: AtomicUsize::new(0),
        sessions_stdout: session_listing_stdout(),
        followup: SessionFollowup::DispatchError,
    });
    let server = Server::from_executor_for_test(executor.clone());
    let mut session = server
        .sessions()
        .expect("fixture session listing")
        .pop()
        .expect("one fixture session");

    let error = session
        .rename("renamed")
        .expect_err("refresh dispatch fails after rename succeeds");

    assert_eq!(executor.calls.load(Ordering::SeqCst), 4);
    assert_eq!(error.kind(), ErrorKind::PartialEffect);
    assert!(
        matches!(
            error,
            Error::AfterEffect { operation: "rename-session", source }
                if source.kind() == ErrorKind::Refused && source.is_transient()
        ),
        "the refresh error remains available as the source",
    );
}

#[test]
fn successful_session_step_requires_an_active_window_postcondition() {
    let executor = Arc::new(ComposedSessionExecutor {
        calls: AtomicUsize::new(0),
        sessions_stdout: session_listing_stdout(),
        followup: SessionFollowup::EmptyListing,
    });
    let server = Server::from_executor_for_test(executor.clone());
    let session = server
        .sessions()
        .expect("fixture session listing")
        .pop()
        .expect("one fixture session");

    let error = session
        .next_window()
        .expect_err("an accepted step must leave an active window");

    assert_eq!(executor.calls.load(Ordering::SeqCst), 4);
    assert!(matches!(
        error,
        Error::AfterEffect { operation: "next-window", source }
            if matches!(*source, Error::ObjectGone { .. })
    ));
}

#[test]
fn new_session_options_redact_the_shell_command() {
    let secret = "sentinel-session-command";
    let options = NewSessionOptions::new("work").command(secret);
    assert!(!format!("{options:?}").contains(secret));

    let summary = options.into_command("#{session_id}").summary();
    assert_eq!(summary.sensitive_argument_count(), 1);
    assert!(!summary.to_string().contains(secret));
}

#[test]
fn access_rule_lines_decode_both_the_legacy_and_the_group_acl_grammar() {
    // A release before group ACLs prints no principal marker at all, and
    // every row it can print names a user.
    let legacy_write = parse_access_rule("alice (W)").expect("legacy write row parses");
    assert_eq!(legacy_write.name(), "alice");
    assert_eq!(legacy_write.principal(), Principal::User);
    assert_eq!(legacy_write.mode(), AccessMode::Write);

    let legacy_read = parse_access_rule("bob (R)").expect("legacy read-only row parses");
    assert_eq!(legacy_read.principal(), Principal::User);
    assert_eq!(legacy_read.mode(), AccessMode::ReadOnly);

    // A release with group ACLs marks every row, owner included, so the
    // decoder must not assume the legacy shape just because a row is
    // read-write.
    let user_row = parse_access_rule("carol (U,W)").expect("compound user row parses");
    assert_eq!(user_row.name(), "carol");
    assert_eq!(user_row.principal(), Principal::User);
    assert_eq!(user_row.mode(), AccessMode::Write);

    let group_row = parse_access_rule("admins (G,R)").expect("compound group row parses");
    assert_eq!(group_row.name(), "admins");
    assert_eq!(group_row.principal(), Principal::Group);
    assert_eq!(group_row.mode(), AccessMode::ReadOnly);
}

/// An unrecognized `server-access -l` marker is reported through
/// `Error::UnreadableAccessRule` rather than silently dropped: a
/// listing with only such a row used to come back empty.
#[test]
fn an_unrecognized_access_rule_marker_is_reported_not_dropped() {
    let bad_principal =
        parse_access_rule("mallory (X,W)").expect_err("an unknown principal letter is rejected");
    assert!(
        matches!(&bad_principal, Error::UnreadableAccessRule { marker } if marker == "(X,W)"),
        "the refusal names the marker rather than silently dropping the row: \
             {bad_principal:?}",
    );
    assert_eq!(bad_principal.kind(), ErrorKind::Decode);

    let bad_mode =
        parse_access_rule("mallory (U,X)").expect_err("an unknown mode letter is rejected");
    assert!(matches!(
        &bad_mode,
        Error::UnreadableAccessRule { marker } if marker == "(U,X)"
    ));

    let no_marker =
        parse_access_rule("mallory").expect_err("a line with no trailing marker is rejected");
    assert!(matches!(no_marker, Error::UnreadableAccessRule { .. }));
}
