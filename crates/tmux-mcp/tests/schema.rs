//! The closed wire grammars advertised to MCP clients.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::error::Error;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;

use libtmux::plan::{
    CapturePane, KillPane, KillWindow, NewSession, NewWindow, Plan, RenameWindow, SelectLayout,
    SelectPane, SelectWindow, SendKeys, SetEnvironment, SetOption, SplitWindow,
};
use serde_json::json;
use tmux_mcp::{RunPlanArgs, Safety, TmuxTools};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn run_plan_schema_matches_every_operation_and_rejects_malformed_plans() -> TestResult {
    let tools = TmuxTools::builder(libtmux::Server::new()?)
        .safety(Safety::Destructive)
        .build();
    let tool = tools
        .offered()
        .into_iter()
        .find(|tool| tool.name == "run_plan")
        .expect("run_plan is offered");
    let schema = serde_json::to_value(tool.input_schema)?;
    jsonschema::draft202012::meta::validate(&schema)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let validator = jsonschema::draft202012::new(&schema)?;

    let mut plan = Plan::new();
    let session = plan.add(
        NewSession::new(OsString::from_vec(vec![0xff]))
            .start_directory("/tmp")
            .window_name("first"),
    );
    let window = plan.add(
        NewWindow::new(session)
            .name("work")
            .start_directory("/tmp")
            .command("sleep 30")
            .environment("MODE", "build")
            .index(2)
            .focus(),
    );
    let pane = plan.add(
        SplitWindow::new(window)
            .horizontal()
            .start_directory("/tmp")
            .command("sleep 30")
            .environment("ROLE", "worker")
            .focus(),
    );
    plan.add(
        SendKeys::new(pane)
            .text("cargo test")
            .keys(["Escape"])
            .enter(),
    );
    plan.add(SelectPane::new(pane));
    plan.add(SelectWindow::new(window));
    plan.add(RenameWindow::new(window, "renamed"));
    plan.add(SetOption::session(session, "status", "off"));
    plan.add(SetEnvironment::new(session, "CI", "1"));
    plan.add(SelectLayout::new(window, "even-horizontal"));
    plan.add(CapturePane::new(pane).escape_sequences());
    plan.add(KillPane::new(pane));
    plan.add(KillWindow::new(window));

    let valid = json!({
        "plan": serde_json::to_value(&plan)?,
        "grouping": "marked",
    });
    assert!(validator.is_valid(&valid), "schema rejected {valid}");
    serde_json::from_value::<RunPlanArgs>(valid)?;

    for malformed in [
        json!({"plan": {"NewSession": {}}}),
        json!({"plan": [{"Unknown": {}}]}),
        json!({"plan": [{"NewSession": {"start_directory": null, "window_name": null}}]}),
        json!({"plan": [{"NewSession": {"name": "work", "start_directory": null, "window_name": null, "extra": false}}]}),
        json!({"plan": [{"CapturePane": {"target": {"Id": "not-a-pane"}, "escape_sequences": false}}]}),
        json!({"plan": [{"CapturePane": {"target": {"Id": "%4294967296"}, "escape_sequences": false}}]}),
        json!({"plan": [{"SendKeys": {"target": {"Id": "%1"}, "text": null, "keys": "Escape", "enter": false}}]}),
    ] {
        assert!(
            !validator.is_valid(&malformed),
            "schema accepted malformed arguments: {malformed}",
        );
        assert!(
            serde_json::from_value::<RunPlanArgs>(malformed.clone()).is_err(),
            "serde accepted malformed arguments: {malformed}",
        );
    }

    Ok(())
}
