use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::PaneId;
use crate::command::{CommandRequest, CommandResult, ProcessStatus};
use crate::internal::executor::{DispatchFuture, Executor, ShutdownFuture};
use crate::plan::{CapturePane, NewSession, SendKeys};

#[derive(Clone, Copy)]
enum SecondOutcome {
    DispatchError,
    Refused,
}

struct SequentialExecutor {
    calls: AtomicUsize,
    second: SecondOutcome,
}

impl Executor for SequentialExecutor {
    fn execute(&self, request: CommandRequest) -> DispatchFuture {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let second = self.second;
        DispatchFuture::new(move || {
            if call == 0 {
                return Ok(CommandResult::new(
                    request.request_id(),
                    request.summary().clone(),
                    ProcessStatus::from_exit_status(ExitStatus::from_raw(0)),
                    Vec::new(),
                    Vec::new(),
                ));
            }

            match second {
                SecondOutcome::DispatchError => Err(Error::Overloaded {
                    request_id: request.request_id().get(),
                    command: request.summary().clone(),
                    in_flight: 1,
                }),
                SecondOutcome::Refused => Ok(CommandResult::new(
                    request.request_id(),
                    request.summary().clone(),
                    ProcessStatus::from_exit_status(ExitStatus::from_raw(256)),
                    Vec::new(),
                    b"second operation refused".to_vec(),
                )),
            }
        })
    }

    fn shutdown(&self) -> ShutdownFuture {
        ShutdownFuture::new(|| Ok(()))
    }
}

fn two_step_plan(first_read_only: bool) -> Plan {
    let pane: PaneId = "%1".parse().expect("a pane id");
    let mut plan = Plan::new();
    if first_read_only {
        plan.add(CapturePane::new(pane.clone()));
    } else {
        plan.add(SendKeys::new(pane.clone()).text("first"));
    }
    plan.add(SendKeys::new(pane).text("second"));
    plan
}

struct CreationOutputExecutor {
    calls: AtomicUsize,
    stdout: &'static [u8],
}

impl Executor for CreationOutputExecutor {
    fn execute(&self, request: CommandRequest) -> DispatchFuture {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let stdout = if call == 0 {
            self.stdout.to_vec()
        } else {
            Vec::new()
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

#[test]
fn creation_binding_rejects_a_wrong_scope_without_partial_state() {
    let mut plan = Plan::new();
    plan.add(NewSession::new("work"));
    let step = Planner::Sequential
        .steps(&plan)
        .into_iter()
        .next()
        .expect("one plan step");
    let mut bound = HashMap::new();

    let error = bind(
        &mut bound,
        plan.steps(),
        &step,
        b"$1 %2 sentinel-binding-output\n",
    )
    .expect_err("the second and third ids have the wrong scopes");

    assert_eq!(error.kind(), crate::ErrorKind::Decode);
    assert!(bound.is_empty(), "a failed decode binds nothing");
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains("sentinel-binding-output"));
}

#[test]
fn malformed_creation_output_stops_before_a_dependent_operation() {
    for stdout in [
        b"$1 %2 @3\n".as_slice(),
        b"$1 @2 sentinel-binding-output\n".as_slice(),
        b"$1 @2 %3".as_slice(),
    ] {
        let executor = Arc::new(CreationOutputExecutor {
            calls: AtomicUsize::new(0),
            stdout,
        });
        let server = Server::from_executor_for_test(executor.clone());
        let mut plan = Plan::new();
        let session = plan.add(NewSession::new("work"));
        plan.add(SendKeys::new(session.pane()).text("do-not-send"));

        let outcome = plan.run(&server, Planner::Sequential);

        assert_eq!(
            executor.calls.load(Ordering::SeqCst),
            1,
            "the dependent operation must not be dispatched",
        );
        let error = outcome.expect_err("malformed creation output fails the run");
        assert_eq!(error.kind(), crate::ErrorKind::PartialEffect);
        assert!(!error.is_transient());
        let diagnostic = format!("{error:?} {error}");
        assert!(!diagnostic.contains("sentinel-binding-output"));
    }
}

#[test]
fn a_later_dispatch_error_follows_the_first_mutating_plan_effect() {
    let executor = Arc::new(SequentialExecutor {
        calls: AtomicUsize::new(0),
        second: SecondOutcome::DispatchError,
    });
    let server = Server::from_executor_for_test(executor.clone());

    let error = two_step_plan(false)
        .run(&server, Planner::Sequential)
        .expect_err("the second dispatch fails");

    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
    assert!(
        matches!(
            &error,
            Error::AfterEffect { operation: "plan", source }
                if source.kind() == crate::ErrorKind::Refused && source.is_transient()
        ),
        "{error:?}",
    );
    assert!(!error.is_transient());
}

#[test]
fn a_dispatch_error_after_only_reads_keeps_its_retryability() {
    let executor = Arc::new(SequentialExecutor {
        calls: AtomicUsize::new(0),
        second: SecondOutcome::DispatchError,
    });
    let server = Server::from_executor_for_test(executor.clone());

    let error = two_step_plan(true)
        .run(&server, Planner::Sequential)
        .expect_err("the second dispatch fails");

    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
    assert_eq!(error.kind(), crate::ErrorKind::Refused);
    assert!(error.is_transient());
    assert!(!matches!(error, Error::AfterEffect { .. }));
}

#[test]
fn a_later_refusal_remains_plan_result_data_after_a_mutation() {
    let executor = Arc::new(SequentialExecutor {
        calls: AtomicUsize::new(0),
        second: SecondOutcome::Refused,
    });
    let server = Server::from_executor_for_test(executor.clone());

    let result = two_step_plan(false)
        .run(&server, Planner::Sequential)
        .expect("tmux refusals remain inspectable plan data");

    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
    assert!(!result.is_complete());
    assert_eq!(result.operations()[0].outcome(), Outcome::Complete);
    assert_eq!(result.operations()[1].outcome(), Outcome::Failed);
    assert!(result.steps()[1].refusal().is_some());
}

#[test]
fn a_refusal_after_only_reads_remains_plan_result_data() {
    let executor = Arc::new(SequentialExecutor {
        calls: AtomicUsize::new(0),
        second: SecondOutcome::Refused,
    });
    let server = Server::from_executor_for_test(executor.clone());

    let result = two_step_plan(true)
        .run(&server, Planner::Sequential)
        .expect("nothing changed before the refusal");

    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
    assert!(!result.is_complete());
    assert!(result.steps()[1].refusal().is_some());
}

#[test]
fn raw_plan_output_is_absent_from_debug() {
    let secret = b"sentinel-plan-output";
    let pane: PaneId = "%1".parse().expect("a pane id");
    let mut plan = Plan::new();
    plan.add(CapturePane::new(pane));
    let step = Planner::Sequential
        .steps(&plan)
        .into_iter()
        .next()
        .expect("one plan step");
    let reported = StepOutcome {
        step,
        outcomes: vec![Outcome::Failed],
        attribution: Attribution::PerCommand,
        command: "capture-pane",
        sensitive_input: true,
        stdout: secret.to_vec(),
        stderr: secret.to_vec(),
    };
    let result = PlanResult {
        operations: vec![OperationReport {
            index: 0,
            kind: OperationKind::CapturePane,
            outcome: Outcome::Failed,
            attribution: Some(Attribution::PerCommand),
            value: None,
        }],
        steps: vec![reported.clone()],
        bound: HashMap::new(),
        dispatches: 1,
    };
    let exposed = format!("{:?}", secret.to_vec());

    for diagnostic in [format!("{reported:?}"), format!("{result:?}")] {
        assert!(!diagnostic.contains(&exposed), "{diagnostic}");
    }
}

#[test]
fn sensitive_refusal_withholds_tmux_output() {
    let secret = "sentinel-plan-secret";
    let pane: PaneId = "%1".parse().expect("a pane id");
    let mut plan = Plan::new();
    plan.add(CapturePane::new(pane));
    let step = Planner::Sequential
        .steps(&plan)
        .into_iter()
        .next()
        .expect("one plan step");
    let reported = StepOutcome {
        step,
        outcomes: vec![Outcome::Failed],
        attribution: Attribution::PerCommand,
        command: "set-option",
        sensitive_input: true,
        stdout: Vec::new(),
        stderr: format!("bad value: {secret}\n").into_bytes(),
    };

    assert!(String::from_utf8_lossy(reported.stderr()).contains(secret));
    let refusal = reported.refusal().expect("the invocation failed");
    assert!(matches!(&refusal, Error::CommandFailed { .. }));
    let diagnostic = format!("{refusal:?} {refusal}");
    assert!(!diagnostic.contains(secret), "{diagnostic}");
}
