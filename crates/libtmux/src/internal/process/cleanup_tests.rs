#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::Command;
use std::time::Duration;

use crate::driver::{AsyncIoRuntime, Process, Runtime as _};

use super::{ProcessGroupGuard, retry_signal, terminate_child};

fn sleeper() -> Box<dyn Process> {
    let mut command = Command::new("/bin/sleep");
    command.arg("30").process_group(0);
    AsyncIoRuntime
        .spawn_process(command)
        .expect("owned sleeper")
}

#[test]
fn group_signal_retries_interrupted_and_accepts_absence() {
    let mut attempts = 0;
    retry_signal(|| {
        attempts += 1;
        if attempts == 1 {
            Err(io::ErrorKind::Interrupted.into())
        } else {
            Err(rustix::io::Errno::SRCH.into())
        }
    })
    .expect("EINTR is retried and ESRCH means no group remains");
    assert_eq!(attempts, 2);
    assert_eq!(
        retry_signal(|| Err(rustix::io::Errno::PERM.into()))
            .unwrap_err()
            .raw_os_error(),
        Some(1)
    );
}

#[tokio::test]
async fn failed_signals_report_without_waiting_for_the_live_child() {
    let mut child = sleeper();
    let mut group = ProcessGroupGuard::new(child.id());
    group.deny_termination_for_test();
    let outcome = tokio::time::timeout(
        Duration::from_millis(250),
        terminate_child(&mut *child, &mut group),
    )
    .await;
    let still_owned = group.is_armed() && child.id().is_some();
    let still_running = child.try_wait().expect("owned child status").is_none();
    group.fail_group_signal = false;
    group.fail_child_signal = false;
    terminate_child(&mut *child, &mut group)
        .await
        .expect("harness cleans up the owned child");
    let error = outcome
        .expect("injected pre-effect failures return before the deadline")
        .expect_err("termination was denied by the test seam");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(
        error
            .to_string()
            .contains("process-group signal: PermissionDenied (os error 13)")
    );
    assert!(
        error
            .to_string()
            .contains("direct-child signal: PermissionDenied (os error 13)")
    );
    assert!(
        still_owned && still_running,
        "failed cleanup must preserve live ownership"
    );
    assert!(!group.is_armed());
}

#[tokio::test]
async fn failed_group_signal_still_kills_and_reaps_the_direct_child() {
    let mut child = sleeper();
    let mut group = ProcessGroupGuard::new(child.id());
    group.fail_group_signal = true;
    let error = terminate_child(&mut *child, &mut group)
        .await
        .expect_err("group failure remains visible");
    assert!(error.to_string().contains("process-group signal"));
    assert!(!error.to_string().contains("direct-child signal"));
    assert!(!group.is_armed());
    assert!(child.id().is_none());
}

#[tokio::test]
async fn retired_group_cannot_signal_a_replacement_identity() {
    let mut child = sleeper();
    let mut group = ProcessGroupGuard::new(child.id());
    terminate_child(&mut *child, &mut group)
        .await
        .expect("original child reaped");
    let mut replacement = sleeper();
    let mut replacement_group = ProcessGroupGuard::new(replacement.id());
    group.process_group = replacement_group.process_group;
    group.signal().expect("retired guard is inert");
    drop(group);
    let alive = tokio::time::timeout(Duration::from_millis(50), replacement.wait())
        .await
        .is_err();
    terminate_child(&mut *replacement, &mut replacement_group)
        .await
        .expect("replacement cleanup");
    assert!(alive, "retired identity must never signal the replacement");
}

#[cfg(feature = "control-mode")]
#[tokio::test]
async fn persistent_shutdown_retains_cleanup_stage_and_os_code() {
    use super::{LaunchContext, PersistentChild, PersistentClients};
    use crate::command::{CommandRequest, RequestId};
    use crate::{Command as TmuxCommand, ControlClientLimits};
    use std::sync::{Arc, Mutex};

    let clients = PersistentClients::new(ControlClientLimits::default());
    let request = CommandRequest::new(RequestId::new(41), TmuxCommand::new("30"));
    let reservation = clients
        .reserve(request.request_id(), request.summary().clone(), None)
        .await
        .expect("client reservation");
    let mut child = PersistentChild::spawn(
        &LaunchContext::new("/bin/sleep"),
        &request,
        reservation,
        Arc::new(Mutex::new(std::collections::HashSet::default())),
    )
    .expect("persistent child");
    child.process_group.deny_termination_for_test();
    let result = tokio::time::timeout(Duration::from_millis(250), child.terminate()).await;
    child.process_group.fail_group_signal = false;
    child.process_group.fail_child_signal = false;
    child
        .terminate()
        .await
        .expect("harness cleans up the child");
    drop(child);
    let error = result
        .expect("known failure is bounded")
        .expect_err("injected failure");
    let original = std::error::Error::source(&error).unwrap().to_string();
    for _ in 0..2 {
        let error = clients
            .shutdown()
            .await
            .expect_err("shutdown retains cleanup failure");
        assert_eq!(
            std::error::Error::source(&error).unwrap().to_string(),
            original
        );
    }
}
