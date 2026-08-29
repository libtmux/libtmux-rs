//! Recording work, grouping it, and running it against real tmux.
//!
//! The property under test throughout is that a planner changes what a plan
//! *costs* and not what it *does*. Where that stops being true -- a failure
//! inside a shared invocation -- the tests pin the honest answer rather than a
//! convenient one.

#![cfg(all(feature = "plan", feature = "test-support"))]
// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and this file has them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use libtmux::plan::{
    Attribution, CapturePane, KillPane, KillWindow, NewSession, NewWindow, OperationKind,
    OperationReport, OperationValue, Outcome, PaneTarget, Plan, PlanResult,
    PlanValidationErrorKind, Planner, SelectPane, SendKeys, SetEnvironment, SetOption, SplitWindow,
    StepReason, WindowTarget,
};
use libtmux::test::TestServer;
use libtmux::{Command, NewSessionOptions, PaneId, PaneWait, Server, WindowId};

/// A plan that builds a session and types into the pane it makes.
fn build_plan(name: &str) -> Plan {
    let mut plan = Plan::new();
    let session = plan.add(NewSession::new(name));
    let pane = plan.add(SplitWindow::new(session.window()).focus());
    plan.add(SendKeys::new(pane).text("true").enter());
    plan.add(SelectPane::new(session.pane()));
    plan
}

#[test]
fn a_planner_changes_what_a_plan_costs_and_not_what_it_records() {
    let plan = build_plan("counted");

    // Every grouping runs the same four operations.
    for planner in [Planner::Sequential, Planner::Folding, Planner::Marked] {
        let covered: Vec<usize> = planner
            .steps(&plan)
            .iter()
            .flat_map(|step| step.indices().to_vec())
            .collect();
        assert_eq!(
            covered,
            [0, 1, 2, 3],
            "{planner:?} runs every operation once"
        );
    }

    assert_eq!(Planner::Sequential.steps(&plan).len(), 4);
    assert!(Planner::Marked.steps(&plan).len() < Planner::Sequential.steps(&plan).len());
}

#[test]
fn an_operation_that_reads_output_never_shares_an_invocation() {
    let pane: PaneId = "%1".parse().expect("a pane id");
    let mut plan = Plan::new();
    plan.add(SendKeys::new(pane.clone()).text("ls").enter());
    plan.add(CapturePane::new(pane.clone()));
    plan.add(SendKeys::new(pane).text("clear").enter());

    let steps = Planner::Folding.steps(&plan);
    assert_eq!(steps.len(), 3, "the capture splits its neighbours apart");

    let reasons: Vec<StepReason> = Planner::Folding
        .explain(&plan)
        .into_iter()
        .map(|(_, reason)| reason)
        .collect();
    assert_eq!(reasons[1], StepReason::ReadsOutput);
}

#[test]
fn a_boundary_stops_a_fold_without_changing_what_runs() {
    let pane: PaneId = "%1".parse().expect("a pane id");
    let mut plan = Plan::new();
    for text in ["one", "two", "three"] {
        plan.add(SendKeys::new(pane.clone()).text(text).enter());
    }

    assert_eq!(Planner::Folding.steps(&plan).len(), 1);

    let bounded = Planner::Folding.steps_bounded(&plan, &BTreeSet::from([0]));
    assert_eq!(bounded.len(), 2);
    let covered: Vec<usize> = bounded
        .iter()
        .flat_map(|step| step.indices().to_vec())
        .collect();
    assert_eq!(covered, [0, 1, 2], "splitting regroups, it does not drop");
}

#[test]
fn a_detached_split_does_not_take_the_marked_fold() {
    let window: WindowId = "@1".parse().expect("a window id");

    // The fold marks the active pane, so a split that leaves focus alone would
    // send its decorations to whichever pane was already active.
    let mut detached = Plan::new();
    let pane = detached.add(SplitWindow::new(window.clone()));
    detached.add(SendKeys::new(pane).text("go").enter());
    assert_eq!(Planner::Marked.steps(&detached).len(), 2);

    let mut focused = Plan::new();
    let pane = focused.add(SplitWindow::new(window).focus());
    focused.add(SendKeys::new(pane).text("go").enter());
    assert_eq!(Planner::Marked.steps(&focused).len(), 1);
}

#[test]
fn a_plan_renders_what_it_can_before_it_runs() {
    let plan = build_plan("previewed");
    let rendered = plan.preview();

    assert_eq!(rendered.len(), 4);
    assert!(
        rendered[0]
            .as_ref()
            .is_some_and(|command| command.summary().to_string().contains("new-session")),
        "an operation naming nothing unbuilt renders now",
    );
    assert!(
        rendered[1].is_none(),
        "an operation targeting an object no step has made yet cannot render yet",
    );
}

#[test]
fn sensitive_plan_arguments_are_absent_from_diagnostics() {
    let secret = "sentinel-plan-secret";
    let session: libtmux::SessionId = "$1".parse().expect("a session id");
    let window: WindowId = "@1".parse().expect("a window id");
    let pane: PaneId = "%1".parse().expect("a pane id");

    let mut plan = Plan::new();
    plan.add(
        NewWindow::new(session.clone())
            .environment("TOKEN", secret)
            .command(secret),
    );
    plan.add(
        SplitWindow::new(window.clone())
            .environment("TOKEN", secret)
            .command(secret),
    );
    plan.add(SendKeys::new(pane).text(secret));
    plan.add(SetOption::window(window, "status-left", secret));
    plan.add(SetEnvironment::new(session, "TOKEN", secret));

    let mut diagnostics = vec![format!("{plan:?}")];
    diagnostics.extend(
        plan.steps()
            .iter()
            .map(|operation| format!("{operation:?}")),
    );
    diagnostics.extend(
        plan.preview()
            .into_iter()
            .flatten()
            .map(|command| format!("{:?}", command.summary())),
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.contains(secret)),
        "a plan diagnostic exposed a sensitive argument: {diagnostics:#?}",
    );

    let sensitive_arguments: usize = plan
        .preview()
        .into_iter()
        .flatten()
        .map(|command| command.summary().sensitive_argument_count())
        .sum();
    assert_eq!(sensitive_arguments, 7);
}

#[test]
fn plan_validation_rejects_a_dependency_that_is_not_earlier() {
    let mut other = Plan::new();
    other.add(NewSession::new("other-first"));
    let future_session = other.add(NewSession::new("other-second"));

    let mut plan = Plan::new();
    plan.add(NewSession::new("first"));
    plan.add(NewWindow::new(future_session));

    let failure = plan
        .validate()
        .expect_err("step one cannot depend on itself");
    assert_eq!(failure.step(), 1);
    assert_eq!(failure.source_step(), 1);
    assert_eq!(failure.kind(), PlanValidationErrorKind::SourceNotEarlier);
}

#[test]
fn destructive_targets_are_inspectable_without_serialization() {
    let pane: PaneId = "%7".parse().expect("a pane id");
    let window: WindowId = "@8".parse().expect("a window id");

    assert_eq!(KillPane::new(pane.clone()).target(), &PaneTarget::Id(pane));
    assert_eq!(
        KillWindow::new(window.clone()).target(),
        &WindowTarget::Id(window),
    );
}

/// How many panes the server holds, as tmux counts them.
async fn pane_count(server: &Server) -> usize {
    server.panes().await.expect("panes list").len()
}

#[tokio::test]
async fn an_invalid_plan_refuses_before_its_first_mutation() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let mut other = Plan::new();
    other.add(NewSession::new("other-first"));
    let future_session = other.add(NewSession::new("other-second"));

    let mut plan = Plan::new();
    plan.add(NewSession::new("must-not-exist"));
    plan.add(NewWindow::new(future_session));

    let failure = plan
        .run(server, Planner::Sequential)
        .await
        .expect_err("the plan is invalid");
    assert!(
        server.sessions_or_empty().await.is_empty(),
        "validation happened after a mutation",
    );
    assert_eq!(failure.kind(), libtmux::ErrorKind::InvalidInput);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn every_planner_leaves_the_same_tmux_state_for_a_different_price() {
    let mut costs = Vec::new();
    let mut shapes = Vec::new();

    for (index, planner) in [Planner::Sequential, Planner::Folding, Planner::Marked]
        .into_iter()
        .enumerate()
    {
        let guard = TestServer::builder().start().await.expect("tmux starts");
        let server = guard.server();

        let plan = build_plan(&format!("priced-{index}"));
        let result = plan.run(server, planner).await.expect("the plan runs");

        assert!(
            result.is_complete(),
            "{planner:?} completed every operation: {:?}",
            result.operations(),
        );
        costs.push((planner, result.dispatches()));
        shapes.push(pane_count(server).await);

        guard.shutdown().await.expect("tmux fixture shuts down");
    }

    assert!(
        shapes.windows(2).all(|pair| pair[0] == pair[1]),
        "the planner did not change the tmux state that resulted: {shapes:?}",
    );
    let dispatches: Vec<usize> = costs.iter().map(|(_, count)| *count).collect();
    assert!(
        dispatches[0] > dispatches[2],
        "folding costs fewer tmux invocations than one per operation: {costs:?}",
    );
}

#[tokio::test]
async fn a_slot_addresses_an_object_the_plan_has_not_made_yet() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let mut plan = Plan::new();
    let session = plan.add(NewSession::new("forward"));
    let window = plan.add(NewWindow::new(session).name("built"));
    plan.add(SetOption::window(window, "synchronize-panes", "on"));

    let result = plan.run(server, Planner::Sequential).await.expect("runs");
    assert!(result.is_complete(), "{:?}", result.operations());

    // The ids came back from the commands that made them, so no listing was
    // needed to address the window from the session.
    let created = result.created(1).expect("the window bound an id");
    let synchronized = server
        .cmd(
            Command::new("show-options")
                .arg("-w")
                .arg("-v")
                .arg("-t")
                .arg(created)
                .arg("synchronize-panes"),
        )
        .await
        .expect("tmux reports the option");
    assert_eq!(synchronized.stdout_lossy().trim(), "on");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

fn expected_attributions(planner: Planner) -> [Option<Attribution>; 4] {
    match planner {
        Planner::Sequential => [Some(Attribution::PerCommand); 4],
        Planner::Folding => [
            Some(Attribution::PerCommand),
            Some(Attribution::Merged),
            Some(Attribution::Merged),
            Some(Attribution::PerCommand),
        ],
        Planner::Marked => [
            Some(Attribution::Merged),
            Some(Attribution::Merged),
            Some(Attribution::Merged),
            Some(Attribution::PerCommand),
        ],
        _ => panic!("the test needs an attribution matrix for {planner:?}"),
    }
}

fn assert_operation_reports(result: &PlanResult, planner: Planner, marker: &str) {
    let reports = result.operations();
    assert_eq!(reports.len(), 4);
    for (index, report) in reports.iter().enumerate() {
        assert_eq!(report.index(), index);
        assert_eq!(report.outcome(), Outcome::Complete);
    }
    assert_eq!(
        reports
            .iter()
            .map(OperationReport::kind)
            .collect::<Vec<_>>(),
        [
            OperationKind::NewWindow,
            OperationKind::SendKeys,
            OperationKind::SelectPane,
            OperationKind::CapturePane,
        ],
    );
    assert_eq!(
        reports
            .iter()
            .map(OperationReport::attribution)
            .collect::<Vec<_>>(),
        expected_attributions(planner),
    );

    let Some(OperationValue::CreatedWindow {
        window: created_window,
        pane,
    }) = reports[0].value()
    else {
        panic!("the creating operation carries typed bindings: {reports:?}");
    };
    assert_eq!(
        result.created(0).and_then(|id| id.to_str()),
        Some(created_window.as_ref()),
    );
    assert!(pane.as_ref().starts_with('%'));
    assert!(matches!(
        reports[1].value(),
        Some(OperationValue::Acknowledged)
    ));
    assert!(matches!(
        reports[2].value(),
        Some(OperationValue::Acknowledged)
    ));
    let Some(OperationValue::CapturedPane(text)) = reports[3].value() else {
        panic!("the capture operation carries pane bytes: {reports:?}");
    };
    assert!(
        text.as_bytes()
            .windows(marker.len())
            .any(|window| window == marker.as_bytes()),
        "the captured bytes stay on operation 3",
    );
}

#[tokio::test]
async fn operation_reports_keep_typed_values_aligned_across_planners() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let marker = "operation-report";
    let session = guard
        .session(
            NewSessionOptions::new("report-parent")
                .command(format!("printf '{marker}\\n'; exec sleep 60")),
        )
        .await
        .expect("the source pane is created");
    let pane = session.panes().await.expect("panes list").remove(0);
    assert_eq!(
        pane.wait_for_text(marker, Duration::from_secs(5))
            .await
            .expect("capture waits"),
        PaneWait::Arrived,
    );

    for (case, planner) in [Planner::Sequential, Planner::Folding, Planner::Marked]
        .into_iter()
        .enumerate()
    {
        let mut plan = Plan::new();
        let window = plan.add(
            NewWindow::new(session.id().clone())
                .name(format!("report-window-{case}"))
                .command("sleep 60")
                .focus(),
        );
        plan.add(SendKeys::new(window.pane()).text("ignored"));
        plan.add(SelectPane::new(window.pane()));
        plan.add(CapturePane::new(pane.id().clone()));

        let result = plan.run(server, planner).await.expect("the plan runs");
        assert_operation_reports(&result, planner, marker);
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_failure_alone_is_named_and_a_failure_in_a_fold_is_not() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let absent: PaneId = "%999".parse().expect("a pane id");

    // Alone, every operation has its own exit status, so the failure is placed
    // exactly.
    let mut plan = Plan::new();
    plan.add(NewSession::new("named"));
    plan.add(KillPane::new(absent.clone()));
    plan.add(SendKeys::new(absent.clone()).text("never").enter());

    let sequential = plan.run(server, Planner::Sequential).await.expect("runs");
    assert_eq!(sequential.operations()[0].outcome(), Outcome::Complete);
    assert_eq!(sequential.operations()[1].outcome(), Outcome::Failed);
    assert_eq!(sequential.operations()[2].outcome(), Outcome::Skipped);
    assert_eq!(sequential.steps()[1].attribution(), Attribution::PerCommand,);

    // Folded, the same two operations share one exit status. tmux reports the
    // same status and stderr whichever member failed, so neither is blamed.
    let mut folded_plan = Plan::new();
    folded_plan.add(KillPane::new(absent.clone()));
    folded_plan.add(SendKeys::new(absent).text("never").enter());

    let folded = folded_plan
        .run(server, Planner::Folding)
        .await
        .expect("runs");
    assert_eq!(folded.steps().len(), 1, "the two shared an invocation");
    assert_eq!(folded.steps()[0].attribution(), Attribution::Merged);
    assert_eq!(
        folded
            .operations()
            .iter()
            .map(OperationReport::outcome)
            .collect::<Vec<_>>(),
        vec![Outcome::Unknown, Outcome::Unknown],
        "a merged failure names no member, and unknown is not success",
    );
    assert!(!folded.is_complete());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[cfg(feature = "serde")]
#[test]
fn a_plan_survives_a_round_trip_through_json() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let mut plan = Plan::new();
    let session = plan.add(NewSession::new("wired"));
    let window = plan.add(NewWindow::new(session).name("build").focus());
    plan.add(SendKeys::new(window.pane()).text("cargo test").enter());
    // An argument tmux accepts but a text format cannot carry as text.
    plan.add(SendKeys::new(window.pane()).text(OsString::from_vec(vec![0xff, b'x'])));
    plan.add(SetOption::window(window, "synchronize-panes", "on"));

    let json = serde_json::to_string(&plan).expect("a plan serialises");
    // The common case stays readable rather than becoming an array of bytes.
    assert!(json.contains("\"cargo test\""), "{json}");

    let restored: Plan = serde_json::from_str(&json).expect("a plan deserialises");
    assert_eq!(restored.len(), plan.len());

    // Rendering is what the plan is for, so comparing rendered commands
    // compares what actually reaches tmux, including the bytes that are not
    // text.
    let before: Vec<_> = plan.preview().iter().map(|c| format!("{c:?}")).collect();
    let after: Vec<_> = restored
        .preview()
        .iter()
        .map(|c| format!("{c:?}"))
        .collect();
    assert_eq!(before, after, "a round trip changes nothing that renders");

    // Grouping is a property of the operations, so it survives too.
    assert_eq!(
        Planner::Marked.steps(&restored).len(),
        Planner::Marked.steps(&plan).len(),
    );
}

#[cfg(feature = "serde")]
#[test]
fn deserialization_rejects_a_slot_with_the_wrong_scope() {
    let session: libtmux::SessionId = "$1".parse().expect("a session id");
    let mut plan = Plan::new();
    plan.add(NewWindow::new(session.clone()));
    plan.add(NewWindow::new(session));

    let mut wire = serde_json::to_value(plan).expect("the plan serializes");
    wire[1]["NewWindow"]["target"] = serde_json::json!({
        "Slot": {"index": 0, "part": "Created"}
    });

    let failure = serde_json::from_value::<Plan>(wire).expect_err("window is not a session");
    assert!(failure.to_string().contains("not Session"), "{failure}");
}
