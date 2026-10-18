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
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{Pid, test_kill_process};

use super::{SubprocessExecutor, validate_request};
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
#[allow(
    clippy::disallowed_methods,
    reason = "this body runs as the spawned child, so the process it ends is its own"
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

fn read_pids(path: &Path, count: usize) -> Vec<u32> {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if let Ok(contents) = fs::read_to_string(path) {
            let pids = contents
                .lines()
                .filter_map(|line| line.parse().ok())
                .collect::<Vec<_>>();
            if pids.len() == count {
                return pids;
            }
        }
        assert!(
            Instant::now() < deadline,
            "child publishes PIDs before deadline"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}
fn pid(value: u32) -> Pid {
    Pid::from_raw(i32::try_from(value).expect("PID fits i32")).expect("PID is nonzero")
}
fn assert_process_gone(value: u32) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while !matches!(test_kill_process(pid(value)), Err(Errno::SRCH)) {
        assert!(
            Instant::now() < deadline,
            "process {value} was not cleaned up"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}
fn assert_process_reaped(value: u32) {
    assert!(
        matches!(test_kill_process(pid(value)), Err(Errno::SRCH)),
        "process {value} not reaped before return"
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

#[test]
fn drains_stdout_and_stderr_concurrently_as_exact_bytes() {
    let executor = executor("streams", TEST_TIMEOUT);
    let result = executor
        .execute(request(1, []))
        .run()
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
    executor.shutdown().run().expect("shutdown succeeds");
}

#[test]
fn nonzero_exit_and_stdout_are_returned_as_data() {
    let executor = executor("nonzero", TEST_TIMEOUT);
    let result = executor
        .execute(request(2, []))
        .run()
        .expect("nonzero status remains result data");

    assert_eq!(result.exit_code(), Some(7));
    assert!(result.stdout().ends_with(b"nonzero-stdout\n\n"));
    executor.shutdown().run().expect("shutdown succeeds");
}

#[test]
fn child_stdin_is_null_and_reaches_eof() {
    let executor = executor("stdin-eof", TEST_TIMEOUT);
    let result = executor
        .execute(request(3, []))
        .run()
        .expect("helper reads EOF and exits");

    assert!(
        result
            .stdout()
            .windows(b"stdin=0\n".len())
            .any(|bytes| bytes == b"stdin=0\n")
    );
    executor.shutdown().run().expect("shutdown succeeds");
}

#[test]
fn deadline_kills_awaits_and_unregisters_the_child() {
    let root = tempfile::tempdir().expect("temporary directory");
    let file = root.path().join("pid");
    let executor = executor("block", Duration::from_millis(250));
    let result = executor
        .execute(request(1, [file.as_os_str().to_owned()]))
        .run();
    assert!(matches!(result, Err(Error::Timeout { request_id: 1, .. })));
    assert_process_reaped(read_pids(&file, 1)[0]);
    assert_eq!(executor.active_request_count(), 0);
}

#[test]
fn cancellation_kills_same_group_descendants_and_holds_capacity_until_reaped() {
    let root = tempfile::tempdir().expect("temporary directory");
    let file = root.path().join("pids");
    let executor = executor("descendant", TEST_TIMEOUT)
        .with_dispatch_limits(DispatchLimits::default().max_in_flight(1));
    let token = crate::CancellationToken::new();
    let dispatch = executor
        .execute(request(1, [file.as_os_str().to_owned()]).with_cancellation(token.clone()));
    let worker = std::thread::spawn(move || dispatch.run());
    let pids = read_pids(&file, 2);
    token.cancel();
    assert!(matches!(
        worker.join().expect("dispatch thread"),
        Err(Error::Cancelled { request_id: 1, .. })
    ));
    assert_process_reaped(pids[0]);
    assert_process_gone(pids[1]);
    assert_eq!(executor.active_request_count(), 0);
}

#[test]
fn exited_leader_anchors_group_while_descendant_holds_pipes() {
    let root = tempfile::tempdir().expect("temporary directory");
    let file = root.path().join("pids");
    let executor = executor("descendant-parent-exits", Duration::from_millis(250));
    let result = executor
        .execute(request(1, [file.as_os_str().to_owned()]))
        .run();
    assert!(matches!(result, Err(Error::Timeout { .. })));
    let pids = read_pids(&file, 2);
    assert_process_reaped(pids[0]);
    assert_process_gone(pids[1]);
}

#[test]
fn shutdown_cancels_all_children_rejects_new_work_and_is_idempotent() {
    let root = tempfile::tempdir().expect("temporary directory");
    let executor = executor("block", TEST_TIMEOUT);
    let mut workers = Vec::new();
    let mut pids = Vec::new();
    for id in 1..=3 {
        let file = root.path().join(id.to_string());
        let dispatch = executor.execute(request(id, [file.as_os_str().to_owned()]));
        workers.push(std::thread::spawn(move || dispatch.run()));
        pids.extend(read_pids(&file, 1));
    }
    executor.shutdown().run().expect("shutdown");
    for worker in workers {
        assert!(matches!(
            worker.join().expect("worker"),
            Err(Error::ExecutorShutdown { .. })
        ));
    }
    for pid in pids {
        assert_process_reaped(pid);
    }
    assert_eq!(executor.active_request_count(), 0);
    executor.shutdown().run().expect("repeated shutdown");
    assert!(matches!(
        executor.execute(request(8, [])).run(),
        Err(Error::ExecutorShutdown { request_id: 8, .. })
    ));
}

#[test]
fn cancelling_a_capacity_waiter_does_not_wait_for_the_active_child() {
    let root = tempfile::tempdir().expect("temporary directory");
    let file = root.path().join("pid");
    let executor = executor("block", TEST_TIMEOUT)
        .with_dispatch_limits(DispatchLimits::default().max_in_flight(1));
    let running = executor.execute(request(1, [file.as_os_str().to_owned()]));
    let worker = std::thread::spawn(move || running.run());
    read_pids(&file, 1);
    let token = crate::CancellationToken::new();
    let pending = executor.execute(request(2, []).with_cancellation(token.clone()));
    let waiting = std::thread::spawn(move || pending.run());
    token.cancel();
    assert!(matches!(
        waiting.join().expect("waiter"),
        Err(Error::Cancelled { request_id: 2, .. })
    ));
    assert_eq!(executor.active_request_count(), 1);
    executor.shutdown().run().expect("shutdown");
    assert!(worker.join().expect("worker").is_err());
}

#[test]
fn duplicate_active_request_id_is_rejected_before_spawn() {
    let root = tempfile::tempdir().expect("temporary directory");
    let file = root.path().join("pid");
    let executor = executor("block", TEST_TIMEOUT);
    let running = executor.execute(request(1, [file.as_os_str().to_owned()]));
    let worker = std::thread::spawn(move || running.run());
    read_pids(&file, 1);
    assert!(matches!(
        executor.execute(request(1, [])).run(),
        Err(Error::DuplicateRequest { .. })
    ));
    executor.shutdown().run().expect("shutdown");
    assert!(worker.join().expect("worker").is_err());
}

#[test]
fn output_limits_fail_instead_of_returning_truncated_data() {
    let executor = executor("streams", TEST_TIMEOUT)
        .with_output_limits(crate::OutputLimits::default().max_stdout_bytes(1024));
    assert!(matches!(
        executor.execute(request(1, [])).run(),
        Err(Error::OutputLimitExceeded {
            stream: "stdout",
            limit: 1024,
            ..
        })
    ));
    assert_eq!(executor.active_request_count(), 0);
}

#[test]
fn parked_dispatch_drains_both_streams_and_enforces_output_limits() {
    let executor = executor("streams", TEST_TIMEOUT);
    let result = executor
        .execute(request(1, []).without_deadline())
        .run()
        .expect("both readers finish");
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
    let limited =
        executor.with_output_limits(crate::OutputLimits::default().max_stdout_bytes(1024));
    assert!(matches!(
        limited.execute(request(2, []).without_deadline()).run(),
        Err(Error::OutputLimitExceeded {
            stream: "stdout",
            ..
        })
    ));
    assert_eq!(limited.active_request_count(), 0);
}

#[test]
fn parked_client_budget_refuses_extra_clients_and_leaves_ordinary_capacity() {
    let root = tempfile::tempdir().expect("temporary directory");
    let executor = executor("block", TEST_TIMEOUT).with_dispatch_limits(
        DispatchLimits::default()
            .max_in_flight(1)
            .max_wait_clients(1),
    );
    let parked_file = root.path().join("parked");
    let parked =
        executor.execute(request(1, [parked_file.as_os_str().to_owned()]).without_deadline());
    let worker = std::thread::spawn(move || parked.run());
    let parked_pid = read_pids(&parked_file, 1)[0];
    let extra_file = root.path().join("extra");
    let extra = executor
        .execute(request(2, [extra_file.as_os_str().to_owned()]).without_deadline())
        .run();
    assert!(matches!(extra, Err(Error::Overloaded { in_flight: 1, .. })));
    assert!(!extra_file.exists(), "rejected client never starts");
    let ordinary_file = root.path().join("ordinary");
    let ordinary = executor.execute(request(3, [ordinary_file.as_os_str().to_owned()]));
    let ordinary_worker = std::thread::spawn(move || ordinary.run());
    let ordinary_pid = read_pids(&ordinary_file, 1)[0];
    executor
        .shutdown()
        .run()
        .expect("shutdown wakes both kinds of client");
    assert!(matches!(
        worker.join().expect("parked worker"),
        Err(Error::ExecutorShutdown { .. })
    ));
    assert!(matches!(
        ordinary_worker.join().expect("ordinary worker"),
        Err(Error::ExecutorShutdown { .. })
    ));
    assert_process_reaped(parked_pid);
    assert_process_reaped(ordinary_pid);
    assert_eq!(executor.active_request_count(), 0);
}

#[test]
fn cancelling_parked_dispatch_wakes_readers_and_reaps_descendants() {
    let root = tempfile::tempdir().expect("temporary directory");
    let file = root.path().join("pids");
    let executor = executor("descendant", TEST_TIMEOUT);
    let token = crate::CancellationToken::new();
    let dispatch = executor.execute(
        request(1, [file.as_os_str().to_owned()])
            .without_deadline()
            .with_cancellation(token.clone()),
    );
    let worker = std::thread::spawn(move || dispatch.run());
    let pids = read_pids(&file, 2);
    token.cancel();
    assert!(matches!(
        worker.join().expect("parked worker"),
        Err(Error::Cancelled { .. })
    ));
    assert_process_reaped(pids[0]);
    assert_process_gone(pids[1]);
    assert_eq!(executor.active_request_count(), 0);
}

#[test]
fn parked_helper_failure_closes_readers_and_survives_repeated_shutdown() {
    let root = tempfile::tempdir().expect("temporary directory");
    let file = root.path().join("pids");
    let launch = LaunchContext::new(std::env::current_exe().expect("test executable"))
        .with_environment(CHILD_ENV, "descendant")
        .with_group_signal_helper("/usr/bin/false".into());
    let executor = SubprocessExecutor::new("unused", TEST_TIMEOUT).with_launch_context(launch);
    let dispatch = executor.execute(request(1, [file.as_os_str().to_owned()]).without_deadline());
    let worker = std::thread::spawn(move || dispatch.run());
    let pids = read_pids(&file, 2);
    let shutdown = executor.shutdown().run();
    let result = worker.join().expect("parked worker");
    // A failed helper cannot contain descendants. Clean this test's survivor
    // explicitly before asserting the reported failure.
    let _ = rustix::process::kill_process(pid(pids[1]), rustix::process::Signal::KILL);
    assert_process_gone(pids[1]);
    assert_process_reaped(pids[0]);
    assert!(matches!(shutdown, Err(Error::WaitChild { .. })));
    assert!(matches!(result, Err(Error::WaitChild { .. })));
    assert!(matches!(
        executor.shutdown().run(),
        Err(Error::WaitChild { .. })
    ));
    assert_eq!(executor.active_request_count(), 0);
}

#[test]
fn missing_group_helper_prevents_the_target_process_starting() {
    let root = tempfile::tempdir().expect("temporary directory");
    let file = root.path().join("pid");
    let launch = LaunchContext::new(std::env::current_exe().expect("test executable"))
        .with_environment(CHILD_ENV, "block")
        .with_group_signal_helper(root.path().join("missing-kill"));
    let executor = SubprocessExecutor::new("unused", TEST_TIMEOUT).with_launch_context(launch);
    assert!(
        executor
            .execute(request(1, [file.as_os_str().to_owned()]))
            .run()
            .is_err()
    );
    assert!(!file.exists(), "target never started");
    assert_eq!(executor.active_request_count(), 0);
}

#[test]
fn failed_group_helper_is_reported_and_the_direct_child_is_still_reaped() {
    let root = tempfile::tempdir().expect("temporary directory");
    let file = root.path().join("pid");
    let launch = LaunchContext::new(std::env::current_exe().expect("test executable"))
        .with_environment(CHILD_ENV, "block")
        .with_group_signal_helper("/usr/bin/false".into());
    let executor =
        SubprocessExecutor::new("unused", Duration::from_millis(250)).with_launch_context(launch);
    let initial = executor
        .execute(request(1, [file.as_os_str().to_owned()]))
        .run();
    assert_process_reaped(read_pids(&file, 1)[0]);
    assert_eq!(executor.active_request_count(), 0);
    for result in [
        initial.map(|_| ()),
        executor.shutdown().run(),
        executor.shutdown().run(),
    ] {
        match result.expect_err("helper failure remains visible") {
            Error::WaitChild {
                request_id, source, ..
            } => {
                assert_eq!(request_id, 1);
                assert_eq!(source.kind(), std::io::ErrorKind::Other);
                assert_eq!(source.to_string(), "process-group signal: Other");
            }
            other => panic!("expected cleanup failure, got {other:?}"),
        }
    }
}

#[test]
fn cancelling_before_dispatch_starts_no_child_and_redacts_arguments() {
    let token = crate::CancellationToken::new();
    token.cancel();
    let command = helper_command([]).sensitive_arg("private-value");
    let executor = executor("block", TEST_TIMEOUT);
    let error = executor
        .execute(request_with_command(9, command).with_cancellation(token))
        .run()
        .expect_err("cancelled");
    assert!(matches!(error, Error::Cancelled { request_id: 9, .. }));
    assert_error_redacted(&error, &["private-value"]);
    assert_eq!(executor.active_request_count(), 0);
}
