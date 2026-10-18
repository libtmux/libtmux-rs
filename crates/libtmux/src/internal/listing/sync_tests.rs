use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{client_session, create_session, mutation_failure};
use crate::command::{CommandRequest, CommandResult, ProcessStatus, RequestId};
use crate::formats::{FIELD_SEPARATOR, FormatPlan, ListProfile};
use crate::internal::core::Core;
use crate::internal::executor::{DispatchFuture, Executor, ShutdownFuture};
use crate::{Command, Error, ErrorKind};

struct CreationExecutor {
    calls: AtomicUsize,
    stdout: &'static [u8],
}

impl Executor for CreationExecutor {
    fn execute(&self, request: CommandRequest) -> DispatchFuture {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let stdout = if call == 0 {
            b"tmux 3.7b\n".to_vec()
        } else {
            assert_eq!(call, 1, "one probe and one creating command");
            self.stdout.to_vec()
        };
        DispatchFuture::new(move || {
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

/// Records the commands a lookup dispatches, answering the version probe.
struct CapturingExecutor {
    commands: std::sync::Mutex<Vec<String>>,
}

impl Executor for CapturingExecutor {
    fn execute(&self, request: CommandRequest) -> DispatchFuture {
        // `CommandSummary` truncates each argument at `MAX_DIAGNOSTIC_BYTES`
        // for safe display; a sessions template has nine fields and runs
        // past that, so the diagnostic view would hide most separators.
        // `argv()` is the untruncated argv tmux actually receives.
        let summary = format!("{:?}", request.argv());
        let probe = summary.contains("display-message") || summary.contains("list-");
        self.commands
            .lock()
            .expect("the capture lock")
            .push(summary);
        let stdout = if probe {
            Vec::new()
        } else {
            b"tmux 3.7b\n".to_vec()
        };
        DispatchFuture::new(move || {
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

/// Apple's `strftime` rule for a conversion it does not define.
///
/// `stdtime/FreeBSD/strftime.c` reaches `default: break;` and then writes
/// `*pt++ = *format`, which is the conversion character alone: the `%` was
/// consumed to detect the conversion and is never emitted. glibc writes
/// both characters, which is exactly why this cannot be observed on Linux
/// -- and macOS tests do not run on a pull request, so a live test could
/// not catch it either. `%%` is defined, and yields one `%`.
fn darwin_strftime(template: &str) -> String {
    let mut out = String::new();
    let mut rest = template.chars();
    while let Some(character) = rest.next() {
        if character != '%' {
            out.push(character);
            continue;
        }
        match rest.next() {
            // `%%` is the one conversion this template means to use.
            Some('%') => out.push('%'),
            // Undefined: the conversion character survives, the `%` does not.
            Some(undefined) => out.push(undefined),
            None => {}
        }
    }
    out
}

/// A client lookup's template must reach tmux's format parser intact.
///
/// `display-message` expands through `strftime` first -- tmux calls
/// `format_expand_time` where every listing calls `format_expand` -- and a
/// format plan separates its fields with `%`. So the separators are
/// conversions as far as `strftime` is concerned, and on Apple's libc they
/// are deleted, leaving one unparseable field and a `DecodeListing` for
/// every attached client.
#[test]
fn a_client_lookup_template_survives_darwin_strftime() {
    let executor = Arc::new(CapturingExecutor {
        commands: std::sync::Mutex::new(Vec::new()),
    });
    let core = Core::from_executor_for_test(executor.clone());

    // An empty answer is "attached to nothing", which is a clean `None`
    // and leaves the dispatched template to inspect.
    let _ = client_session(&core, std::ffi::OsStr::new("/dev/pts/0"));

    let commands = executor.commands.lock().expect("the capture lock").clone();
    let dispatched = commands
        .iter()
        .find(|command| command.contains("#{"))
        .expect("a lookup dispatches a format template");

    // What tmux time-expands is only a problem for a command that time
    // expands. A lookup that avoids one has nothing to preserve.
    if !dispatched.contains("display-message") {
        return;
    }

    let field_separator = FIELD_SEPARATOR as char;
    let expected = FormatPlan::for_profile(
        ListProfile::Sessions,
        core.capabilities().expect("a version").tmux_version(),
    );
    let wanted = expected.template().matches(field_separator).count();
    assert!(wanted > 0, "a sessions template separates its fields");

    // The strftime pass is skipped whenever the template holds no `%`
    // (`format.c`'s `strchr(fmt, '%') != NULL` guard), on every libc.
    assert!(
        !dispatched.contains('%'),
        "a lookup template must contain no `%`; sent {dispatched}",
    );

    let survived = darwin_strftime(dispatched).matches(field_separator).count();
    assert_eq!(
        survived, wanted,
        "every field separator has to survive strftime; sent {dispatched}",
    );
}

#[test]
fn sensitive_mutation_failure_withholds_tmux_output() {
    let secret = "sentinel-mutation-secret";
    let command = Command::new("set-option")
        .arg("--")
        .arg("mouse")
        .sensitive_arg(secret);
    let result = CommandResult::new(
        RequestId::new(1),
        command.summary(),
        ProcessStatus::from_exit_status(ExitStatus::from_raw(1 << 8)),
        Vec::new(),
        format!("bad value: {secret}\n").into_bytes(),
    );

    let error = mutation_failure("set-option", &result, None);
    assert!(matches!(&error, Error::CommandFailed { .. }));
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains(secret), "{diagnostic}");
}

#[test]
fn successful_creation_marks_decode_and_missing_object_failures() {
    let executor = Arc::new(CreationExecutor {
        calls: AtomicUsize::new(0),
        stdout: b"malformed\n".as_slice(),
    });
    let core = Core::from_executor_for_test(executor.clone());

    let error = create_session(&core, |_format| Command::new("new-session"))
        .expect_err("tmux succeeded but did not describe the created session");

    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
    assert_eq!(error.kind(), ErrorKind::PartialEffect);
    assert!(matches!(
        error,
        Error::AfterEffect {
            operation: "new-session",
            ..
        }
    ));
}

/// Output tmux could not decode is a partial effect: something was made and
/// this cannot say what. No output at all is not, and the difference is not
/// cosmetic -- a caller told an effect may be outstanding cannot safely
/// retry. A creating command that worked always prints the object it made,
/// so nothing printed means nothing made, which is what every tmux does for
/// a socket path under a directory that does not exist.
#[test]
fn creation_that_printed_nothing_is_a_refusal_not_a_partial_effect() {
    let executor = Arc::new(CreationExecutor {
        calls: AtomicUsize::new(0),
        stdout: b"".as_slice(),
    });
    let core = Core::from_executor_for_test(executor.clone());

    let error = create_session(&core, |_format| Command::new("new-session"))
        .expect_err("tmux succeeded but described no session");

    assert_ne!(error.kind(), ErrorKind::PartialEffect);
    assert!(matches!(
        error,
        Error::NoEffect {
            command: "new-session",
            ..
        }
    ));
}
