//! The closed wire grammars advertised to MCP clients.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;

use libtmux::plan::{
    CapturePane, KillPane, KillWindow, NewSession, NewWindow, Plan, RenameWindow, SelectLayout,
    SelectPane, SelectWindow, SendKeys, SetEnvironment, SetOption, SplitWindow,
};
use serde_json::json;
use tmux_mcp::{
    FilterArgs, OptionArgs, ResizePaneArgs, RunPlanArgs, Safety, SelectPaneArgs, SelectWindowArgs,
    SplitPaneArgs, TmuxTools, TreeFilterArgs,
};

type TestResult = Result<(), Box<dyn Error>>;

fn every_operation_plan() -> Plan {
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
    plan
}

fn advertised_operations(schema: &serde_json::Value) -> BTreeSet<&str> {
    schema["$defs"]["Op"]["oneOf"]
        .as_array()
        .expect("Op is a tagged union")
        .iter()
        .map(|variant| {
            let required = variant["required"]
                .as_array()
                .expect("an operation requires its tag");
            assert_eq!(required.len(), 1, "an operation has one tag: {variant}");
            required[0].as_str().expect("the operation tag is text")
        })
        .collect()
}

fn output_schema(tools: &TmuxTools, name: &str) -> Result<serde_json::Value, Box<dyn Error>> {
    let tool = tools
        .offered()
        .into_iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| std::io::Error::other(format!("{name} is not offered")))?;
    let schema = tool
        .output_schema
        .as_ref()
        .ok_or_else(|| std::io::Error::other(format!("{name} has no output schema")))?;
    Ok(serde_json::to_value(schema)?)
}

#[test]
fn each_safety_tier_advertises_exactly_the_operations_it_accepts() -> TestResult {
    const READ_ONLY: &[&str] = &["CapturePane"];
    const MUTATING: &[&str] = &[
        "CapturePane",
        "NewSession",
        "NewWindow",
        "RenameWindow",
        "SelectLayout",
        "SelectPane",
        "SelectWindow",
        "SendKeys",
        "SetEnvironment",
        "SetOption",
        "SplitWindow",
    ];
    const DESTRUCTIVE: &[&str] = &[
        "CapturePane",
        "KillPane",
        "KillWindow",
        "NewSession",
        "NewWindow",
        "RenameWindow",
        "SelectLayout",
        "SelectPane",
        "SelectWindow",
        "SendKeys",
        "SetEnvironment",
        "SetOption",
        "SplitWindow",
    ];

    for (tier, expected) in [
        (Safety::ReadOnly, READ_ONLY),
        (Safety::Mutating, MUTATING),
        (Safety::Destructive, DESTRUCTIVE),
    ] {
        let tools = TmuxTools::builder(libtmux::Server::new()?)
            .safety(tier)
            .build();
        let tool = tools
            .offered()
            .into_iter()
            .find(|tool| tool.name == "run_plan")
            .expect("run_plan is offered");
        let schema = serde_json::to_value(tool.input_schema)?;
        jsonschema::draft202012::meta::validate(&schema)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let advertised = advertised_operations(&schema);

        assert_eq!(advertised, expected.iter().copied().collect(), "{tier:?}");
        let validator = jsonschema::draft202012::new(&schema)?;
        let operations = serde_json::to_value(every_operation_plan())?;
        for operation in operations.as_array().expect("a plan is an array") {
            let name = operation
                .as_object()
                .and_then(|object| object.keys().next())
                .expect("an operation has one tag");
            let arguments = json!({"plan": [operation]});
            assert_eq!(
                validator.is_valid(&arguments),
                expected.contains(&name.as_str()),
                "{tier:?} admission for {name}",
            );
        }
    }

    Ok(())
}

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

    let plan = every_operation_plan();

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

#[test]
fn tool_schemas_close_every_documented_choice_vocabulary() -> TestResult {
    let tools = TmuxTools::builder(libtmux::Server::new()?)
        .safety(Safety::Destructive)
        .build();

    for (name, valid, invalid) in [
        (
            "run_plan",
            json!({"plan": [], "grouping": "marked"}),
            json!({"plan": [], "grouping": "parallel"}),
        ),
        (
            "split_pane",
            json!({"pane": "%1", "direction": "above"}),
            json!({"pane": "%1", "direction": "sideways"}),
        ),
        (
            "resize_pane",
            json!({"pane": "%1", "direction": "left", "cells": 1}),
            json!({"pane": "%1", "direction": "inward", "cells": 1}),
        ),
        (
            "select_pane",
            json!({"pane": "%1", "direction": "previous"}),
            json!({"pane": "%1", "direction": "sideways"}),
        ),
        (
            "select_window",
            json!({"window": "@1", "direction": "last"}),
            json!({"window": "@1", "direction": "first"}),
        ),
        (
            "show_option",
            json!({"name": "status", "scope": "pane", "target": "%1"}),
            json!({"name": "status", "scope": "planet", "target": "%1"}),
        ),
    ] {
        let tool = tools
            .offered()
            .into_iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("{name} is offered"));
        let schema = serde_json::to_value(tool.input_schema)?;
        let validator = jsonschema::draft202012::new(&schema)?;

        assert!(validator.is_valid(&valid), "{name} rejected {valid}");
        assert!(
            !validator.is_valid(&invalid),
            "{name} advertised an open vocabulary: {invalid}",
        );
    }

    Ok(())
}

#[test]
fn plan_output_schema_closes_every_documented_choice_vocabulary() -> TestResult {
    let tools = TmuxTools::builder(libtmux::Server::new()?)
        .safety(Safety::Destructive)
        .build();

    let schema = output_schema(&tools, "run_plan")?;
    let validator = jsonschema::draft202012::new(&schema)?;
    let operation = |kind: &str, outcome: &str, attribution: serde_json::Value| {
        json!({
            "operations": [{
                "index": 0,
                "kind": kind,
                "outcome": outcome,
                "attribution": attribution,
                "value": null,
            }],
            "failures": [],
            "dispatches": 1,
            "complete": outcome == "complete",
        })
    };
    for kind in [
        "new-session",
        "new-window",
        "split-window",
        "send-keys",
        "select-pane",
        "select-window",
        "rename-window",
        "set-option",
        "set-environment",
        "select-layout",
        "capture-pane",
        "kill-pane",
        "kill-window",
    ] {
        assert!(
            validator.is_valid(&operation(kind, "complete", json!("per_command"))),
            "run_plan rejected operation kind {kind}",
        );
    }
    for (outcome, attribution) in [
        ("complete", json!("per_command")),
        ("failed", json!("merged")),
        ("skipped", serde_json::Value::Null),
        ("unknown", json!("merged")),
    ] {
        assert!(
            validator.is_valid(&operation("send-keys", outcome, attribution)),
            "run_plan rejected outcome {outcome}",
        );
    }
    for invalid in [
        operation("unknown-operation", "complete", json!("per_command")),
        operation("send-keys", "pending", json!("per_command")),
        operation("send-keys", "complete", json!("batched")),
    ] {
        assert!(
            !validator.is_valid(&invalid),
            "run_plan advertised an open output vocabulary: {invalid}",
        );
    }
    let failure = json!({
        "operations": [],
        "failures": [{
            "operations": [0],
            "attribution": "merged",
            "kind": "refused",
            "stderr_bytes": 0,
            "stderr_withheld": false,
        }],
        "dispatches": 1,
        "complete": false,
    });
    assert!(validator.is_valid(&failure), "run_plan rejected {failure}");
    let mut invalid = failure;
    invalid["failures"][0]["attribution"] = json!("batched");
    assert!(
        !validator.is_valid(&invalid),
        "run_plan advertised an open failure attribution: {invalid}",
    );

    Ok(())
}

#[test]
fn other_output_schemas_close_documented_choice_vocabularies() -> TestResult {
    let tools = TmuxTools::builder(libtmux::Server::new()?)
        .safety(Safety::Destructive)
        .build();

    for (name, valid, invalid) in [
        (
            "watch_pane",
            json!({"pane": "%1", "output": "", "bytes": 0, "stopped": "deadline"}),
            json!({"pane": "%1", "output": "", "bytes": 0, "stopped": "quiet"}),
        ),
        (
            "set_option",
            json!({"name": "status", "scope": "global-session"}),
            json!({"name": "status", "scope": "planet"}),
        ),
        (
            "wait_for_channel",
            json!({"channel": "ready", "outcome": "signalled"}),
            json!({"channel": "ready", "outcome": "waiting"}),
        ),
    ] {
        let schema = output_schema(&tools, name)?;
        let validator = jsonschema::draft202012::new(&schema)?;
        assert!(validator.is_valid(&valid), "{name} rejected {valid}");
        assert!(
            !validator.is_valid(&invalid),
            "{name} advertised an open output vocabulary: {invalid}",
        );
    }

    Ok(())
}

#[test]
fn public_argument_struct_literals_keep_string_vocabulary_fields() {
    let _ = RunPlanArgs {
        plan: Plan::new(),
        grouping: Some("sequential".into()),
    };
    let _ = SplitPaneArgs {
        pane: "%1".into(),
        direction: Some("right".into()),
        percent: None,
        command: None,
    };
    let _ = ResizePaneArgs {
        pane: "%1".into(),
        direction: "up".into(),
        cells: 1,
    };
    let _ = SelectPaneArgs {
        pane: "%1".into(),
        direction: Some("next".into()),
    };
    let _ = SelectWindowArgs {
        window: "@1".into(),
        direction: Some("last".into()),
    };
    let _ = OptionArgs {
        name: "status".into(),
        scope: Some("global-session".into()),
        target: None,
        value: None,
    };
}

#[test]
fn portable_filter_schemas_reject_target_field_and_operator_mismatches() -> TestResult {
    let tools = TmuxTools::builder(libtmux::Server::new()?).build();

    for (name, cases) in [
        (
            "find_panes",
            vec![
                (
                    true,
                    json!({"filter": {"version": 1, "target": "pane", "expr":
                        {"op": "eq", "field": "pane_active", "value": true}}}),
                ),
                (
                    true,
                    json!({"filter": {"version": 1, "target": "pane", "expr":
                    {"op": "and", "args": [
                        {"op": "contains", "field": "pane_title", "value": "build"},
                        {"op": "gte", "field": "pane_width", "value": "80"}
                    ]}}}),
                ),
                (
                    false,
                    json!({"filter": {"version": 1, "target": "window", "expr":
                        {"op": "eq", "field": "pane_active", "value": true}}}),
                ),
                (
                    false,
                    json!({"filter": {"version": 1, "target": "pane", "expr":
                        {"op": "eq", "field": "not_a_pane_field", "value": true}}}),
                ),
                (
                    false,
                    json!({"filter": {"version": 1, "target": "pane", "expr":
                        {"op": "contains", "field": "pane_active", "value": "yes"}}}),
                ),
            ],
        ),
        (
            "find_sessions",
            vec![
                (
                    true,
                    json!({"filter": {"version": 1, "target": "session_tree", "expr": {
                        "op": "relation", "field": "windows", "quantifier": "any",
                        "expr": {"op": "relation", "field": "panes", "quantifier": "none",
                            "expr": {"op": "eq", "field": "pane_dead", "value": true}}
                    }}}),
                ),
                (
                    false,
                    json!({"filter": {"version": 1, "target": "session_tree", "expr": {
                        "op": "relation", "field": "panes", "quantifier": "any",
                        "expr": {"op": "eq", "field": "pane_dead", "value": true}
                    }}}),
                ),
                (
                    false,
                    json!({"filter": {"version": 1, "target": "pane", "expr":
                        {"op": "eq", "field": "session_name", "value": "build"}}}),
                ),
            ],
        ),
    ] {
        let tool = tools
            .offered()
            .into_iter()
            .find(|tool| tool.name == name)
            .expect("filter tool is offered");
        let schema = serde_json::to_value(tool.input_schema)?;
        let bytes = serde_json::to_vec(&schema)?.len();
        let ceiling = if name == "find_panes" {
            8 * 1_024
        } else {
            16 * 1_024
        };
        assert!(
            bytes <= ceiling,
            "{name} schema grew to {bytes} bytes (ceiling {ceiling})",
        );
        jsonschema::draft202012::meta::validate(&schema)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let validator = jsonschema::draft202012::new(&schema)?;

        for (expected, arguments) in cases {
            let decoded = if name == "find_panes" {
                serde_json::from_value::<FilterArgs>(arguments.clone()).is_ok()
            } else {
                serde_json::from_value::<TreeFilterArgs>(arguments.clone()).is_ok()
            };
            assert_eq!(decoded, expected, "typed decoder admission for {arguments}");
            assert_eq!(
                validator.is_valid(&arguments),
                expected,
                "advertised schema admission for {arguments}",
            );
        }
    }

    Ok(())
}
