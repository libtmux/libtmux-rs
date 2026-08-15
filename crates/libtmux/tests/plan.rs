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

use libtmux::plan::{
    Attribution, CapturePane, KillPane, NewSession, NewWindow, Outcome, Plan, Planner, SelectPane,
    SendKeys, SetOption, SplitWindow, StepReason,
};
use libtmux::test::TestServer;
use libtmux::{Command, PaneId, Server, WindowId};

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

/// How many panes the server holds, as tmux counts them.
async fn pane_count(server: &Server) -> usize {
    server.try_panes().await.expect("panes list").len()
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
            result.outcomes(),
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
    assert!(result.is_complete(), "{:?}", result.outcomes());

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
    assert_eq!(sequential.outcomes()[0], Outcome::Complete);
    assert_eq!(sequential.outcomes()[1], Outcome::Failed);
    assert_eq!(sequential.outcomes()[2], Outcome::Skipped);
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
        folded.outcomes(),
        [Outcome::Unknown, Outcome::Unknown],
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
