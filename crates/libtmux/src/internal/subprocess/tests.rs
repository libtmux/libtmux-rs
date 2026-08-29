#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used
)]

use std::error::Error as StdError;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::process;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{Pid, WaitOptions, test_kill_process, waitpid};

use super::{ReaderFailure, SubprocessExecutor, TestHooks, validate_request};
use crate::command::{CommandRequest, RequestId};
use crate::internal::executor::Executor;
use crate::internal::process::LaunchContext;
use crate::{Command, DispatchLimits, Error};

const CHILD_ENV: &str = "LIBTMUX_RS_TEST_CHILD";
const CHILD_TEST: &str = "internal::subprocess::tests::child_helper";
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a poll loop waits before looking again.
///
/// Sleeping rather than yielding matters: these loops wait on a separate
/// process, and a spinning task holds a worker thread against the thing
/// it is waiting for. With two worker threads and a loaded machine, that
/// is enough to miss the deadline it is measuring.
const POLL_INTERVAL: Duration = Duration::from_millis(1);

#[cfg(feature = "tracing")]
const TRACE_CHILD_ENV: &str = "LIBTMUX_RS_TRACING_TEST_CHILD";
#[cfg(feature = "tracing")]
const TRACE_EARLY_TEST: &str =
    "internal::subprocess::tests::tracing_early_failures_emit_one_sanitized_terminal_event";
#[cfg(feature = "tracing")]
const TRACE_SUPERVISOR_TEST: &str =
    "internal::subprocess::tests::tracing_errors_and_sources_omit_sensitive_argv_and_raw_output";

#[cfg(feature = "tracing")]
async fn tracing_test_is_isolated_child(test_name: &str) -> bool {
    if std::env::var_os(TRACE_CHILD_ENV).as_deref() == Some(OsStr::new(test_name)) {
        return true;
    }

    let output = tokio::process::Command::new(
        std::env::current_exe().expect("test executable is available"),
    )
    .arg("--exact")
    .arg(test_name)
    .arg("--nocapture")
    .arg("--")
    .env(TRACE_CHILD_ENV, test_name)
    .output()
    .await
    .expect("isolated tracing test starts");
    assert!(
        output.status.success(),
        "isolated tracing test failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    false
}

#[test]
fn nul_validation_distinguishes_global_subcommand_and_argument_positions() {
    let cases = [
        (
            vec![OsString::from_vec(b"-S\0tail".to_vec())],
            Command::new("display-message"),
            "tmux global argument",
        ),
        (
            vec![OsString::from("-S"), OsString::from("/tmp/socket")],
            Command::new(OsString::from_vec(b"command\0tail".to_vec())),
            "tmux subcommand",
        ),
        (
            vec![OsString::from("-S"), OsString::from("/tmp/socket")],
            Command::new("display-message").arg(OsString::from_vec(b"argument\0tail".to_vec())),
            "tmux argument",
        ),
    ];

    for (global_argv, command, expected) in cases {
        let request = CommandRequest::with_global_argv(RequestId::new(31), &global_argv, command);
        let error = validate_request(&LaunchContext::new("tmux"), &request)
            .expect_err("fixture contains NUL");
        assert!(matches!(
            error,
            Error::InvalidCommandInput { input, .. } if input == expected
        ));
    }
}

#[test]
#[allow(
    clippy::zombie_processes,
    reason = "the process-group test deliberately kills parent and descendant together"
)]
fn child_helper() {
    let Some(mode) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let arguments = helper_arguments();

    match mode.as_bytes() {
        b"streams" => {
            std::io::stdout()
                .write_all(&vec![0xff; 128 * 1024])
                .expect("stdout is writable");
            std::io::stderr()
                .write_all(&vec![0xfe; 128 * 1024])
                .expect("stderr is writable");
        }
        b"nonzero" => {
            std::io::stdout()
                .write_all(b"nonzero-stdout\n\n")
                .expect("stdout is writable");
            process::exit(7);
        }
        b"stdin-eof" => {
            let mut input = Vec::new();
            std::io::stdin()
                .read_to_end(&mut input)
                .expect("stdin is readable");
            writeln!(std::io::stdout(), "stdin={}", input.len()).expect("stdout is writable");
        }
        b"echo-last" => {
            std::io::stdout()
                .write_all(arguments.last().expect("payload argument").as_bytes())
                .expect("stdout is writable");
        }
        b"block" => {
            write_pid_file(arguments.first().expect("PID path"), None);
            loop {
                std::thread::park();
            }
        }
        b"secret-block" => {
            write_pid_file(arguments.first().expect("PID path"), None);
            std::io::stdout()
                .write_all(b"sentinel-output-secret")
                .expect("stdout is writable");
            loop {
                std::thread::park();
            }
        }
        b"secret-success" => {
            std::io::stdout()
                .write_all(b"sentinel-success-output")
                .expect("stdout is writable");
        }
        b"descendant" => {
            let pid_path = arguments.first().expect("PID path");
            let child = process::Command::new(std::env::current_exe().expect("test executable"))
                .arg("--exact")
                .arg(CHILD_TEST)
                .arg("--nocapture")
                .arg("--")
                .env(CHILD_ENV, "grandchild")
                .spawn()
                .expect("grandchild starts");
            write_pid_file(pid_path, Some(child.id()));
            loop {
                std::thread::park();
            }
        }
        b"descendant-parent-exits" => {
            let pid_path = arguments.first().expect("PID path");
            let child = process::Command::new(std::env::current_exe().expect("test executable"))
                .arg("--exact")
                .arg(CHILD_TEST)
                .arg("--nocapture")
                .arg("--")
                .env(CHILD_ENV, "grandchild")
                .spawn()
                .expect("grandchild starts");
            write_pid_file(pid_path, Some(child.id()));
        }
        b"grandchild" => loop {
            std::thread::park();
        },
        other => panic!("unknown child helper mode: {other:?}"),
    }
}

fn helper_arguments() -> Vec<OsString> {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let marker = arguments
        .iter()
        .rposition(|argument| argument == OsStr::new("--"))
        .expect("helper command includes an argument separator");
    arguments.into_iter().skip(marker + 1).collect()
}

fn write_pid_file(path: &OsStr, descendant: Option<u32>) {
    let mut value = process::id().to_string();
    if let Some(pid) = descendant {
        value.push('\n');
        value.push_str(&pid.to_string());
    }
    let path = Path::new(path);
    let mut staging_name = path
        .file_name()
        .expect("PID path has a file name")
        .to_os_string();
    staging_name.push(format!(".{}.tmp", process::id()));
    let staging = path.with_file_name(staging_name);
    let mut file = fs::File::create(&staging).expect("staged PID file is creatable");
    file.write_all(value.as_bytes())
        .expect("staged PID file is writable");
    file.sync_all().expect("staged PID contents are durable");
    drop(file);
    fs::rename(staging, path).expect("PID file is published atomically");
}

fn helper_command(arguments: impl IntoIterator<Item = OsString>) -> Command {
    arguments.into_iter().fold(
        Command::new("--exact")
            .arg(CHILD_TEST)
            .arg("--nocapture")
            .arg("--"),
        Command::arg,
    )
}

fn request(id: u64, arguments: impl IntoIterator<Item = OsString>) -> CommandRequest {
    CommandRequest::new(RequestId::new(id), helper_command(arguments))
}

fn request_with_command(id: u64, command: Command) -> CommandRequest {
    CommandRequest::new(RequestId::new(id), command)
}

fn executor(mode: &str, timeout: Duration) -> SubprocessExecutor {
    SubprocessExecutor::new(std::env::current_exe().expect("test executable"), timeout)
        .with_environment(CHILD_ENV, mode)
}

async fn read_pids(path: &Path, count: usize) -> Vec<u32> {
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if let Ok(contents) = fs::read_to_string(path) {
                let pids = contents
                    .lines()
                    .filter_map(|line| line.parse::<u32>().ok())
                    .collect::<Vec<_>>();
                if pids.len() == count {
                    return pids;
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("child publishes PIDs before the test deadline")
}

fn pid(value: u32) -> Pid {
    Pid::from_raw(i32::try_from(value).expect("test PID fits i32")).expect("test PID is nonzero")
}

async fn assert_process_gone(value: u32) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if matches!(test_kill_process(pid(value)), Err(Errno::SRCH)) {
                return;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("process disappears before the test deadline");
}

fn assert_process_reaped(value: u32) {
    assert!(
        matches!(test_kill_process(pid(value)), Err(Errno::SRCH)),
        "process {value} still exists after terminal cleanup"
    );
}

fn assert_error_redacted(error: &Error, secrets: &[&str]) {
    let mut diagnostics = vec![error.to_string(), format!("{error:?}")];
    let mut source = StdError::source(error);
    while let Some(current) = source {
        diagnostics.push(current.to_string());
        diagnostics.push(format!("{current:?}"));
        source = current.source();
    }
    for diagnostic in diagnostics {
        for secret in secrets {
            assert!(
                !diagnostic.contains(secret),
                "leaked secret in {diagnostic:?}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drains_stdout_and_stderr_concurrently_as_exact_bytes() {
    let executor = executor("streams", TEST_TIMEOUT);
    let result = executor
        .execute(request(1, []))
        .await
        .expect("helper exits successfully");

    assert!(
        result
            .stdout()
            .split(|byte| *byte != 0xff)
            .any(|run| run.len() == 128 * 1024)
    );
    assert!(
        result
            .stderr()
            .split(|byte| *byte != 0xfe)
            .any(|run| run.len() == 128 * 1024)
    );
    assert!(result.success());
    executor.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test]
async fn nonzero_exit_and_stdout_are_returned_as_data() {
    let executor = executor("nonzero", TEST_TIMEOUT);
    let result = executor
        .execute(request(2, []))
        .await
        .expect("nonzero status remains result data");

    assert_eq!(result.exit_code(), Some(7));
    assert!(result.stdout().ends_with(b"nonzero-stdout\n\n"));
    executor.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test]
async fn child_stdin_is_null_and_reaches_eof() {
    let executor = executor("stdin-eof", TEST_TIMEOUT);
    let result = executor
        .execute(request(3, []))
        .await
        .expect("helper reads EOF and exits");

    assert!(
        result
            .stdout()
            .windows(b"stdin=0\n".len())
            .any(|bytes| bytes == b"stdin=0\n")
    );
    executor.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deadline_kills_awaits_and_unregisters_the_child() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let pid_path = directory.path().join("deadline.pid");
    // The deadline has to lose to the child's startup, not race it. The
    // child publishes its PID and the test reads it back, so a deadline
    // that expires first kills the child before it ever writes, and the
    // read then waits out its own five seconds for a file nobody will
    // write. At 100ms that happened on CI, where re-executing this binary
    // takes longer than it does here. The length is not what is under
    // test; that the deadline kills, awaits, and unregisters is.
    let executor = executor("block", Duration::from_secs(2));
    let dispatch =
        tokio::spawn(executor.execute(request(4, [pid_path.as_os_str().to_os_string()])));
    let child_pid = read_pids(&pid_path, 1).await[0];
    let error = dispatch
        .await
        .expect("dispatch task remains healthy")
        .expect_err("blocking helper reaches deadline");

    assert!(matches!(error, Error::Timeout { .. }));
    assert_eq!(executor.active_request_count(), 0);
    assert_process_reaped(child_pid);
    executor.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_deadline_bounds_waiting_for_capacity() {
    const DEADLINE: Duration = Duration::from_millis(200);

    let directory = tempfile::tempdir().expect("temporary directory");
    for (case, acquire_timeout) in [None, Some(DEADLINE * 10)].into_iter().enumerate() {
        let holder_path = directory.path().join(format!("holder-{case}.pid"));
        let waiting_path = directory.path().join(format!("waiting-{case}.pid"));
        let reached = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let hooks = TestHooks {
            after_reservation_reached: Some(Arc::clone(&reached)),
            after_reservation_release: Some(Arc::clone(&release)),
            ..TestHooks::default()
        };
        let limits = DispatchLimits::default()
            .max_in_flight(1)
            .acquire_timeout(acquire_timeout);
        let executor = executor("block", DEADLINE)
            .with_dispatch_limits(limits)
            .with_test_hooks(hooks);
        let base = 26 + u64::try_from(case).expect("small case index") * 2;
        let holder =
            tokio::spawn(executor.execute(request(base, [holder_path.as_os_str().to_os_string()])));

        tokio::task::spawn_blocking(move || reached.wait())
            .await
            .expect("barrier task succeeds");
        let mut waiting = tokio::spawn(
            executor.execute(request(base + 1, [waiting_path.as_os_str().to_os_string()])),
        );
        let outcome = tokio::time::timeout(DEADLINE * 3, &mut waiting).await;
        if outcome.is_err() {
            waiting.abort();
            let _ = (&mut waiting).await;
        }
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .expect("barrier task succeeds");
        let holder_error = holder
            .await
            .expect("holder task remains healthy")
            .expect_err("holder exhausts its deadline before spawn");

        let error = outcome
            .expect("the dispatch deadline includes capacity waiting")
            .expect("waiting task remains healthy")
            .expect_err("capacity remains occupied until the deadline");
        assert!(matches!(error, Error::Overloaded { .. }), "{error:?}");
        assert!(matches!(holder_error, Error::Timeout { .. }));
        executor.shutdown().await.expect("shutdown succeeds");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_dispatch_future_cancels_and_reaps() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let pid_path = directory.path().join("drop.pid");
    let executor = executor("block", TEST_TIMEOUT);
    let mut dispatch =
        Box::pin(executor.execute(request(5, [pid_path.as_os_str().to_os_string()])));

    tokio::select! {
        _ = read_pids(&pid_path, 1) => {}
        result = &mut dispatch => panic!("helper terminated before cancellation: {result:?}"),
    }
    let child_pid = read_pids(&pid_path, 1).await[0];
    drop(dispatch);
    executor
        .shutdown()
        .await
        .expect("shutdown waits for cleanup");

    assert_eq!(executor.active_request_count(), 0);
    assert_process_reaped(child_pid);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborting_the_awaiting_task_cancels_and_reaps() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let pid_path = directory.path().join("abort.pid");
    let executor = executor("block", TEST_TIMEOUT);
    let dispatch =
        tokio::spawn(executor.execute(request(6, [pid_path.as_os_str().to_os_string()])));
    let child_pid = read_pids(&pid_path, 1).await[0];

    dispatch.abort();
    assert!(dispatch.await.expect_err("task was aborted").is_cancelled());
    executor
        .shutdown()
        .await
        .expect("shutdown waits for cleanup");

    assert_eq!(executor.active_request_count(), 0);
    assert_process_reaped(child_pid);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_kills_same_group_descendants_holding_pipes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let pid_path = directory.path().join("descendant.pid");
    let executor = executor("descendant", TEST_TIMEOUT);
    let dispatch =
        tokio::spawn(executor.execute(request(7, [pid_path.as_os_str().to_os_string()])));
    let pids = read_pids(&pid_path, 2).await;

    dispatch.abort();
    let _ = dispatch.await;
    executor
        .shutdown()
        .await
        .expect("shutdown waits for cleanup");

    assert_eq!(executor.active_request_count(), 0);
    assert_process_reaped(pids[0]);
    assert_process_gone(pids[1]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exited_leader_anchors_group_while_descendant_holds_pipes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let pid_path = directory.path().join("exited-leader.pid");
    // Same race as `deadline_kills_awaits_and_unregisters_the_child`, and
    // worse: two PIDs have to be published before the deadline expires.
    let executor = executor("descendant-parent-exits", Duration::from_secs(2));
    let dispatch =
        tokio::spawn(executor.execute(request(25, [pid_path.as_os_str().to_os_string()])));
    let pids = read_pids(&pid_path, 2).await;
    let error = dispatch
        .await
        .expect("dispatch task remains healthy")
        .expect_err("inherited pipes remain open until the overall deadline");

    assert!(matches!(error, Error::Timeout { .. }));
    assert_eq!(executor.active_request_count(), 0);
    assert_process_reaped(pids[0]);
    assert_process_gone(pids[1]).await;
    executor.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_cancels_all_children_rejects_new_work_and_is_idempotent() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let first_path = directory.path().join("first.pid");
    let second_path = directory.path().join("second.pid");
    let rejected_path = directory.path().join("rejected.pid");
    let executor = executor("block", TEST_TIMEOUT);
    let first = tokio::spawn(executor.execute(request(8, [first_path.as_os_str().to_os_string()])));
    let second =
        tokio::spawn(executor.execute(request(9, [second_path.as_os_str().to_os_string()])));
    let first_pid = read_pids(&first_path, 1).await[0];
    let second_pid = read_pids(&second_path, 1).await[0];

    let first_shutdown_executor = executor.clone();
    let second_shutdown_executor = executor.clone();
    let first_shutdown = tokio::spawn(async move { first_shutdown_executor.shutdown().await });
    let second_shutdown = tokio::spawn(async move { second_shutdown_executor.shutdown().await });
    for shutdown in [first_shutdown, second_shutdown] {
        tokio::time::timeout(TEST_TIMEOUT, shutdown)
            .await
            .expect("concurrent shutdown does not miss an empty notification")
            .expect("shutdown task remains healthy")
            .expect("shutdown succeeds");
    }
    executor.shutdown().await.expect("later shutdown succeeds");
    assert_eq!(executor.active_request_count(), 0);
    assert!(matches!(
        executor
            .execute(request(10, [rejected_path.as_os_str().to_os_string()]))
            .await,
        Err(Error::ExecutorShutdown { .. })
    ));
    assert!(!rejected_path.exists());
    let _ = first.await;
    let _ = second.await;
    assert_process_reaped(first_pid);
    assert_process_reaped(second_pid);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_active_request_id_is_rejected_before_spawn() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let first_path = directory.path().join("first.pid");
    let duplicate_path = directory.path().join("duplicate.pid");
    let executor = executor("block", TEST_TIMEOUT);
    let first =
        tokio::spawn(executor.execute(request(11, [first_path.as_os_str().to_os_string()])));
    let child_pid = read_pids(&first_path, 1).await[0];

    let duplicate = executor
        .execute(request(11, [duplicate_path.as_os_str().to_os_string()]))
        .await
        .expect_err("the active identity is refused");
    assert!(matches!(duplicate, Error::DuplicateRequest { .. }));
    assert!(
        !duplicate.is_transient(),
        "a duplicate identity is an internal invariant failure, not backoff",
    );
    assert!(!duplicate_path.exists());
    first.abort();
    let _ = first.await;
    executor
        .shutdown()
        .await
        .expect("shutdown waits for cleanup");
    assert_process_reaped(child_pid);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abort_between_spawn_and_supervisor_handoff_cannot_orphan() {
    let reached = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let spawned_pid = Arc::new(AtomicU32::new(0));
    let hooks = TestHooks {
        after_spawn_reached: Some(Arc::clone(&reached)),
        after_spawn_release: Some(Arc::clone(&release)),
        spawned_pid: Some(Arc::clone(&spawned_pid)),
        ..TestHooks::default()
    };
    let executor = executor("block", TEST_TIMEOUT).with_test_hooks(hooks);
    let dispatch = tokio::spawn(executor.execute(request(12, [])));

    tokio::task::spawn_blocking(move || reached.wait())
        .await
        .expect("barrier task succeeds");
    dispatch.abort();
    tokio::task::spawn_blocking(move || release.wait())
        .await
        .expect("barrier task succeeds");
    let _ = dispatch.await;
    let child_pid = spawned_pid.load(Ordering::SeqCst);
    assert_ne!(child_pid, 0, "spawn hook records the direct child PID");
    executor
        .shutdown()
        .await
        .expect("shutdown waits for cleanup");

    assert_eq!(executor.active_request_count(), 0);
    assert_process_reaped(child_pid);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_start_racing_shutdown_remains_registered_until_cleanup() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let pid_path = directory.path().join("race.pid");
    let reached = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let hooks = TestHooks {
        after_reservation_reached: Some(Arc::clone(&reached)),
        after_reservation_release: Some(Arc::clone(&release)),
        ..TestHooks::default()
    };
    let executor = executor("block", TEST_TIMEOUT).with_test_hooks(hooks);
    let dispatch =
        tokio::spawn(executor.execute(request(13, [pid_path.as_os_str().to_os_string()])));

    tokio::task::spawn_blocking(move || reached.wait())
        .await
        .expect("barrier task succeeds");
    let shutdown_executor = executor.clone();
    let shutdown = tokio::spawn(async move { shutdown_executor.shutdown().await });
    tokio::time::timeout(TEST_TIMEOUT, async {
        while executor.is_accepting() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("shutdown closes admission before the reservation is released");
    tokio::task::spawn_blocking(move || release.wait())
        .await
        .expect("barrier task succeeds");

    shutdown
        .await
        .expect("shutdown task remains healthy")
        .expect("shutdown succeeds");
    let _ = dispatch.await;
    assert_eq!(executor.active_request_count(), 0);
    if pid_path.exists() {
        assert_process_reaped(read_pids(&pid_path, 1).await[0]);
    }
}

#[tokio::test]
async fn shutdown_winning_admission_prevents_spawn() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let pid_path = directory.path().join("never.pid");
    let executor = executor("block", TEST_TIMEOUT);
    executor.shutdown().await.expect("shutdown succeeds");

    assert!(matches!(
        executor
            .execute(request(14, [pid_path.as_os_str().to_os_string()]))
            .await,
        Err(Error::ExecutorShutdown { .. })
    ));
    assert!(!pid_path.exists());
}

#[tokio::test]
async fn unresolvable_executable_is_typed_regardless_of_spawn_error_kind() {
    // A bare name that resolves nowhere is classified by resolution rather
    // than by `io::ErrorKind`. WSL with Windows directories on `PATH`
    // reports `EIO` here, so classifying by kind alone would return an
    // untyped spawn failure for a missing tmux on a supported platform.
    let directory = tempfile::tempdir().expect("temporary directory");
    let empty_path = directory.path().as_os_str().to_os_string();
    let missing = SubprocessExecutor::new("libtmux-missing-executable", TEST_TIMEOUT)
        .with_environment("PATH", empty_path);

    let error = missing
        .execute(request_with_command(40, Command::new("display-message")))
        .await
        .expect_err("missing executable fails");

    assert!(
        matches!(error, Error::ExecutableNotFound { .. }),
        "unresolvable bare name is typed, got {error:?}",
    );
}

#[tokio::test]
async fn present_but_unexecutable_file_is_not_reported_as_missing() {
    // Resolution mirrors `execvp`: it matches regular files, so a present
    // file that cannot be executed is a permission failure rather than an
    // absent tmux. Conflating the two would send callers looking for an
    // installation that is already there.
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("temporary directory");
    let executable = directory.path().join("libtmux-unexecutable");
    fs::write(&executable, b"#!/bin/sh\n").expect("file is created");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o644))
        .expect("permissions are cleared");

    let error = SubprocessExecutor::new("libtmux-unexecutable", TEST_TIMEOUT)
        .with_environment("PATH", directory.path().as_os_str().to_os_string())
        .execute(request_with_command(41, Command::new("display-message")))
        .await
        .expect_err("an unexecutable file fails to spawn");

    assert!(
        !matches!(error, Error::ExecutableNotFound { .. }),
        "a present file is not reported missing, got {error:?}",
    );
}

#[tokio::test]
async fn invalid_executable_and_nul_inputs_are_sanitized_typed_errors() {
    let missing = SubprocessExecutor::new("libtmux-missing-executable", TEST_TIMEOUT);
    let error = missing
        .execute(request_with_command(15, Command::new("display-message")))
        .await
        .expect_err("missing executable fails");
    assert!(matches!(error, Error::ExecutableNotFound { .. }));

    let nul_executable =
        SubprocessExecutor::new(OsString::from_vec(b"tmux\0invalid".to_vec()), TEST_TIMEOUT);
    let nul_commands = [
        Command::new(OsString::from_vec(b"display\0message".to_vec())),
        Command::new("display-message").arg(OsString::from_vec(b"public\0arg".to_vec())),
        Command::new("display-message")
            .sensitive_arg(OsString::from_vec(b"sensitive\0arg".to_vec())),
    ];
    let mut errors = vec![
        nul_executable
            .execute(request_with_command(16, Command::new("display-message")))
            .await
            .expect_err("NUL executable fails"),
    ];
    for (offset, command) in nul_commands.into_iter().enumerate() {
        errors.push(
            executor("echo-last", TEST_TIMEOUT)
                .execute(request_with_command(17 + offset as u64, command))
                .await
                .expect_err("NUL command token fails"),
        );
    }

    for error in &errors {
        assert!(matches!(error, Error::InvalidCommandInput { .. }));
        assert!(StdError::source(error).is_none());
        assert_error_redacted(error, &["tmux\0invalid", "sensitive", "public"]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reader_failure_and_supervisor_loss_are_sanitized_and_cleanup() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let reader_pid_path = directory.path().join("reader.pid");
    let reader_release = Arc::new(tokio::sync::Notify::new());
    let reader = executor("block", TEST_TIMEOUT).with_test_hooks(TestHooks {
        reader_failure: Some(ReaderFailure::Error),
        reader_failure_release: Some(Arc::clone(&reader_release)),
        ..TestHooks::default()
    });
    let reader_dispatch =
        tokio::spawn(reader.execute(request(20, [reader_pid_path.as_os_str().to_os_string()])));
    let reader_pid = read_pids(&reader_pid_path, 1).await[0];
    reader_release.notify_one();
    let reader_error = reader_dispatch
        .await
        .expect("reader dispatch task remains healthy")
        .expect_err("injected reader failure is surfaced");
    assert!(matches!(reader_error, Error::ReadOutput { .. }));
    assert!(
        !reader_error.is_transient(),
        "the child started before its output failed",
    );
    assert!(StdError::source(&reader_error).is_none());
    assert_eq!(reader.active_request_count(), 0);
    assert_process_reaped(reader_pid);
    reader.shutdown().await.expect("shutdown succeeds");

    let lost_pid_path = directory.path().join("lost.pid");
    let supervisor_release = Arc::new(tokio::sync::Notify::new());
    let lost = executor("block", TEST_TIMEOUT).with_test_hooks(TestHooks {
        supervisor_failure_release: Some(Arc::clone(&supervisor_release)),
        ..TestHooks::default()
    });
    let lost_dispatch =
        tokio::spawn(lost.execute(request(21, [lost_pid_path.as_os_str().to_os_string()])));
    let lost_pid = read_pids(&lost_pid_path, 1).await[0];
    supervisor_release.notify_one();
    let lost_error = lost_dispatch
        .await
        .expect("lost-supervisor dispatch task remains healthy")
        .expect_err("lost supervisor is surfaced");
    assert!(matches!(lost_error, Error::SupervisorLost { .. }));
    assert!(
        !lost_error.is_transient(),
        "the child started before its supervisor was lost",
    );
    assert!(StdError::source(&lost_error).is_none());
    assert_eq!(lost.active_request_count(), 0);
    assert_process_reaped(lost_pid);
    lost.shutdown().await.expect("shutdown succeeds");

    for error in [&reader_error, &lost_error] {
        assert_error_redacted(
            error,
            &["sentinel-output-secret", "sentinel-supervisor-panic"],
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_failure_is_typed_and_cleanup_reaps_the_child() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let pid_path = directory.path().join("wait.pid");
    let wait_failure_release = Arc::new(tokio::sync::Notify::new());
    let executor = executor("block", TEST_TIMEOUT).with_test_hooks(TestHooks {
        wait_failure_release: Some(Arc::clone(&wait_failure_release)),
        ..TestHooks::default()
    });
    let dispatch =
        tokio::spawn(executor.execute(request(22, [pid_path.as_os_str().to_os_string()])));
    let child_pid = read_pids(&pid_path, 1).await[0];

    wait_failure_release.notify_one();
    let error = dispatch
        .await
        .expect("wait-failure dispatch task remains healthy")
        .expect_err("injected wait failure is surfaced");

    assert!(matches!(error, Error::WaitChild { .. }));
    assert!(
        !error.is_transient(),
        "the child started before waiting for it failed",
    );
    assert!(StdError::source(&error).is_some());
    assert_eq!(executor.active_request_count(), 0);
    assert_process_reaped(child_pid);
    executor.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test]
async fn shell_metacharacters_reach_the_child_as_one_exact_argument() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let side_effect = directory.path().join("must-not-exist");
    let payload = format!("$(touch {}) ; * & |", side_effect.display());
    let executor = executor("echo-last", TEST_TIMEOUT);
    let command = Command::new("--exact")
        .arg(CHILD_TEST)
        .arg("--nocapture")
        .arg("--")
        .sensitive_arg(payload.clone());
    let result = executor
        .execute(request_with_command(22, command))
        .await
        .expect("helper echoes one literal argument");

    assert!(
        result
            .stdout()
            .windows(payload.len())
            .any(|bytes| bytes == payload.as_bytes())
    );
    assert!(!side_effect.exists());
    assert!(!result.command().to_string().contains(&payload));
    executor.shutdown().await.expect("shutdown succeeds");
}

#[cfg(feature = "tracing")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracing_early_failures_emit_one_sanitized_terminal_event() {
    use std::sync::Mutex;

    use tracing::instrument::WithSubscriber as _;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    struct Writer(Arc<Mutex<Vec<u8>>>);

    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Buffer {
        type Writer = Writer;

        fn make_writer(&'a self) -> Self::Writer {
            Writer(Arc::clone(&self.0))
        }
    }

    fn subscriber(buffer: Buffer) -> impl tracing::Subscriber + Send + Sync {
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(buffer)
            .finish()
    }

    fn assert_single_failure(trace: &str, request_id: u64, secrets: &[&str]) {
        assert_eq!(
            trace.matches("tmux command requested").count(),
            1,
            "trace must contain one requested event: {trace:?}"
        );
        assert_eq!(
            trace.matches("tmux command failed").count(),
            1,
            "trace must contain one terminal failure event: {trace:?}"
        );
        assert!(
            trace.contains(&format!("request_id={request_id}")),
            "trace must retain the safe request ID: {trace:?}"
        );
        for secret in secrets {
            assert!(!trace.contains(secret), "trace leaked {secret}: {trace:?}");
        }
    }

    if !tracing_test_is_isolated_child(TRACE_EARLY_TEST).await {
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let executable_secret = "sentinel-missing-executable-path";
    let argument_secret = "sentinel-missing-argument";
    let missing_buffer = Buffer::default();
    let missing = SubprocessExecutor::new(directory.path().join(executable_secret), TEST_TIMEOUT);
    let missing_error = async {
        let error = missing
            .execute(request_with_command(
                27,
                Command::new("display-message").sensitive_arg(argument_secret),
            ))
            .await
            .expect_err("missing executable fails before supervisor handoff");
        missing.shutdown().await.expect("shutdown succeeds");
        error
    }
    .with_subscriber(subscriber(missing_buffer.clone()))
    .await;
    assert!(matches!(missing_error, Error::ExecutableNotFound { .. }));
    let missing_trace = String::from_utf8_lossy(&missing_buffer.0.lock().unwrap()).into_owned();
    assert_single_failure(&missing_trace, 27, &[executable_secret, argument_secret]);

    let shutdown_secret = "sentinel-shutdown-argument";
    let shutdown_buffer = Buffer::default();
    let closed = executor("echo-last", TEST_TIMEOUT);
    let shutdown_error = async {
        closed.shutdown().await.expect("shutdown succeeds");
        closed
            .execute(request_with_command(
                28,
                Command::new("display-message").sensitive_arg(shutdown_secret),
            ))
            .await
            .expect_err("closed executor rejects the request")
    }
    .with_subscriber(subscriber(shutdown_buffer.clone()))
    .await;
    assert!(matches!(shutdown_error, Error::ExecutorShutdown { .. }));
    let shutdown_trace = String::from_utf8_lossy(&shutdown_buffer.0.lock().unwrap()).into_owned();
    assert_single_failure(&shutdown_trace, 28, &[shutdown_secret]);
}

#[cfg(feature = "tracing")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracing_errors_and_sources_omit_sensitive_argv_and_raw_output() {
    use std::sync::Mutex;

    use tracing::instrument::WithSubscriber as _;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    struct Writer(Arc<Mutex<Vec<u8>>>);

    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Buffer {
        type Writer = Writer;

        fn make_writer(&'a self) -> Self::Writer {
            Writer(Arc::clone(&self.0))
        }
    }

    if !tracing_test_is_isolated_child(TRACE_SUPERVISOR_TEST).await {
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let pid_path = directory.path().join("tracing.pid");
    let buffer = Buffer::default();
    let scoped = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(buffer.clone())
        .finish();
    let (error, successful_result) = async {
        let reader_release = Arc::new(tokio::sync::Notify::new());
        let error_executor = executor("secret-block", TEST_TIMEOUT).with_test_hooks(TestHooks {
            reader_failure: Some(ReaderFailure::Panic),
            reader_failure_release: Some(Arc::clone(&reader_release)),
            ..TestHooks::default()
        });
        let command = helper_command([pid_path.as_os_str().to_os_string()])
            .sensitive_arg("sentinel-argument-secret");
        let observed_path = pid_path.clone();
        let release_after_output = tokio::spawn(async move {
            let child_pid = read_pids(&observed_path, 1).await[0];
            reader_release.notify_one();
            child_pid
        });
        let error = error_executor
            .execute(request_with_command(23, command))
            .await
            .expect_err("reader panic becomes a typed error");
        let error_pid = release_after_output
            .await
            .expect("reader release task remains healthy");
        assert!(matches!(error, Error::ReadOutput { .. }));
        assert!(StdError::source(&error).is_none());
        assert_process_reaped(error_pid);
        error_executor.shutdown().await.expect("shutdown succeeds");

        let success_executor = executor("secret-success", TEST_TIMEOUT);
        let successful_result = success_executor
            .execute(request_with_command(
                26,
                helper_command([]).sensitive_arg("sentinel-success-argument"),
            ))
            .await
            .expect("successful command returns output data");
        success_executor
            .shutdown()
            .await
            .expect("shutdown succeeds");
        (error, successful_result)
    }
    .with_subscriber(scoped)
    .await;

    assert_error_redacted(
        &error,
        &[
            "sentinel-argument-secret",
            "sentinel-output-secret",
            "sentinel-reader-panic",
        ],
    );
    assert!(
        successful_result
            .stdout()
            .windows(b"sentinel-success-output".len())
            .any(|bytes| bytes == b"sentinel-success-output")
    );
    let trace = String::from_utf8_lossy(&buffer.0.lock().unwrap()).into_owned();
    assert!(
        trace.contains("request_id=23"),
        "captured trace did not include the safe request ID: {trace:?}"
    );
    assert!(trace.contains("tmux command requested"));
    assert!(trace.contains("tmux command failed"));
    assert!(trace.contains("tmux command finished"));
    assert!(trace.contains("stdout_len="));
    for secret in [
        "sentinel-argument-secret",
        "sentinel-output-secret",
        "sentinel-reader-panic",
        "sentinel-success-argument",
        "sentinel-success-output",
    ] {
        assert!(!trace.contains(secret), "trace leaked {secret}: {trace:?}");
    }
}

#[test]
fn runtime_teardown_signals_the_group_without_claiming_library_reaping() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let pid_path = directory.path().join("runtime-drop.pid");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime builds");
    let pids = runtime.block_on(async {
        let executor = executor("descendant", TEST_TIMEOUT);
        let dispatch = executor.execute(request(24, [pid_path.as_os_str().to_os_string()]));
        tokio::spawn(dispatch);
        read_pids(&pid_path, 2).await
    });
    drop(runtime);

    let direct = pid(pids[0]);
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match waitpid(Some(direct), WaitOptions::NOHANG) {
            Ok(Some((_pid, status))) => {
                assert!(status.terminating_signal().is_some());
                break;
            }
            Err(Errno::CHILD) => break,
            Ok(None) | Err(Errno::INTR) => {}
            outcome => panic!("unexpected direct-child wait outcome: {outcome:?}"),
        }
        assert!(Instant::now() < deadline, "direct child was not killed");
        std::thread::yield_now();
    }

    while !matches!(test_kill_process(direct), Err(Errno::SRCH)) {
        assert!(Instant::now() < deadline, "direct child did not disappear");
        std::thread::yield_now();
    }

    let descendant = pid(pids[1]);
    while !matches!(test_kill_process(descendant), Err(Errno::SRCH)) {
        assert!(Instant::now() < deadline, "descendant was not killed");
        std::thread::yield_now();
    }
}
