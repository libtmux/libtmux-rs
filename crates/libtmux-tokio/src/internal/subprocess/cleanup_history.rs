#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use rustix::io::Errno;
use rustix::process::{Pid, WaitOptions, waitpid};
use tokio::sync::Notify;

use super::{PipeRegistrationFailure, SubprocessExecutor, TestHooks};
use crate::command::{CommandRequest, RequestId};
use crate::internal::executor::Executor;
use crate::{Command, Error, Server};

const BUDGET: Duration = Duration::from_millis(200);
const LIMIT: Duration = Duration::from_secs(5);

struct Release(Arc<Notify>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

fn child_command() -> Command {
    Command::new("-c")
        .sensitive_arg("printf 'private-stdout'; printf 'private-stderr' >&2; exec sleep 30")
        .arg(format!("public-argument-{}", "x".repeat(8192)))
}

fn held_executor() -> (Arc<SubprocessExecutor>, Arc<AtomicU32>, Release) {
    let release = Arc::new(Notify::new());
    let pid = Arc::new(AtomicU32::new(0));
    let executor = Arc::new(SubprocessExecutor::new("/bin/sh", BUDGET).with_test_hooks(
        TestHooks {
            spawned_pid: Some(pid.clone()),
            wait_failure_release: Some(release.clone()),
            reported_group_signal_failure: Some(io::ErrorKind::PermissionDenied),
            ..TestHooks::default()
        },
    ));
    (executor, pid, Release(release))
}

async fn started(pid: &AtomicU32) -> u32 {
    tokio::time::timeout(LIMIT, async {
        loop {
            let value = pid.load(Ordering::SeqCst);
            if value != 0 {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("real child starts")
}

fn assert_reaped(value: u32) {
    let pid = Pid::from_raw(i32::try_from(value).unwrap()).unwrap();
    assert!(matches!(
        waitpid(Some(pid), WaitOptions::NOHANG),
        Err(Errno::CHILD)
    ));
}

fn assert_cleanup_failure(error: &Error) {
    let Error::WaitChild {
        source, command, ..
    } = error
    else {
        panic!("shutdown lost cleanup evidence: {error:?}");
    };
    assert!(
        source
            .to_string()
            .contains("process-group signal failed (PermissionDenied)")
    );
    assert_eq!(command.public_argument_count(), 0);
    let diagnostic = format!("{error:?} {error}");
    assert!(diagnostic.len() < 1024, "retained history is bounded");
    for private in [
        "private-stdout",
        "private-stderr",
        "exec sleep",
        "public-argument",
    ] {
        assert!(!diagnostic.contains(private), "{diagnostic}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_retains_cleanup_failure_after_the_ordinary_future_is_dropped() {
    let (executor, pid, release) = held_executor();
    let server = Server::from_executor_for_test(executor.clone());
    let caller = server.clone();
    let dispatch = tokio::spawn(async move { caller.cmd(child_command()).await });
    let pid = started(&pid).await;
    dispatch.abort();
    assert!(dispatch.await.unwrap_err().is_cancelled());
    assert_eq!(executor.active_request_count(), 1);
    drop(release);
    for holder in [&server, &server.clone()] {
        let error = holder
            .shutdown()
            .await
            .expect_err("unobserved cleanup failure is retained");
        assert_cleanup_failure(&error);
    }
    assert_eq!(executor.active_request_count(), 0);
    assert_reaped(pid);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_retains_cleanup_failure_after_a_pending_ordinary_reply() {
    let (executor, pid, release) = held_executor();
    let server = Server::from_executor_for_test(executor.clone());
    let owner = executor.clone();
    let dispatch = tokio::spawn(async move {
        owner
            .execute(CommandRequest::new(RequestId::new(77), child_command()))
            .await
    });
    let pid = started(&pid).await;
    let error = tokio::time::timeout(LIMIT, dispatch)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(matches!(error, Error::OperationCleanupPending { source }
        if matches!(*source, Error::Timeout { request_id: 77, .. })));
    assert!(matches!(
        server.shutdown().await,
        Err(Error::ShutdownPending)
    ));
    assert_eq!(executor.active_request_count(), 1);
    drop(release);
    for holder in [&server, &server.clone()] {
        let error = holder
            .shutdown()
            .await
            .expect_err("late cleanup failure is retained");
        assert_cleanup_failure(&error);
    }
    assert_eq!(executor.active_request_count(), 0);
    assert_reaped(pid);
    assert!(matches!(
        server.cmd(child_command()).await,
        Err(Error::ExecutorShutdown { .. })
    ));
}

#[tokio::test]
async fn shutdown_keeps_a_cleanup_failure_already_delivered_with_its_primary() {
    let executor = Arc::new(SubprocessExecutor::new("/bin/sh", BUDGET).with_test_hooks(
        TestHooks {
            reported_group_signal_failure: Some(io::ErrorKind::PermissionDenied),
            ..TestHooks::default()
        },
    ));
    let server = Server::from_executor_for_test(executor.clone());
    for id in [88, 89] {
        let error = executor
            .execute(CommandRequest::new(RequestId::new(id), child_command()))
            .await
            .unwrap_err();
        let Error::OperationAndCleanup { primary, cleanup } = error else {
            panic!("caller lost its primary or cleanup error: {error:?}");
        };
        assert!(matches!(*primary, Error::Timeout { request_id, .. } if request_id == id));
        assert!(matches!(*cleanup, Error::WaitChild { request_id, .. } if request_id == id));
    }
    for holder in [&server, &server.clone()] {
        let error = holder
            .shutdown()
            .await
            .expect_err("shutdown repeats the same cleanup failure");
        assert_cleanup_failure(&error);
        assert!(matches!(error, Error::WaitChild { request_id: 88, .. }));
    }
}

#[tokio::test]
async fn shutdown_retains_startup_cleanup_failure_after_real_child_registration_fails() {
    let pid = Arc::new(AtomicU32::new(0));
    let executor = Arc::new(SubprocessExecutor::new("/bin/sh", BUDGET).with_test_hooks(
        TestHooks {
            spawned_pid: Some(pid.clone()),
            pipe_registration_failure: Some(PipeRegistrationFailure::Stdout),
            reported_group_signal_failure: Some(io::ErrorKind::PermissionDenied),
            ..TestHooks::default()
        },
    ));
    let server = Server::from_executor_for_test(executor.clone());
    let error = executor
        .execute(CommandRequest::new(RequestId::new(99), child_command()))
        .await
        .unwrap_err();
    let Error::OperationAndCleanup { primary, cleanup } = error else {
        panic!("startup and cleanup causes were not kept: {error:?}");
    };
    assert!(matches!(*primary, Error::OperationFailure { source, .. }
        if matches!(*source, Error::ReadOutput { request_id: 99, .. })));
    assert!(matches!(*cleanup, Error::WaitChild { request_id: 99, .. }));
    let error = server
        .shutdown()
        .await
        .expect_err("startup cleanup also survives registry removal");
    assert_cleanup_failure(&error);
    let value = pid.load(Ordering::SeqCst);
    assert_ne!(value, 0);
    assert_reaped(value);
}
