//! The startup-frozen public surface and its closed wire grammars.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::error::Error;

use serde_json::json;
use tmux_mcp::{Selection, TmuxTools};

type TestResult = Result<(), Box<dyn Error>>;

const INSPECT: &[&str] = &[
    "list_sessions",
    "list_windows",
    "list_panes",
    "get_server_info",
    "get_session_info",
    "get_window_info",
    "get_pane_info",
    "capture_pane",
    "capture_since",
    "snapshot_pane",
    "search_panes",
    "find_pane_by_position",
    "wait_for_text",
    "get_tmux_variables",
    "show_option",
    "show_environment",
    "show_hooks",
    "call_read_tools_batch",
];
const MANAGE: &[&str] = &[
    "rename_session",
    "rename_window",
    "select_window",
    "select_pane",
    "select_layout",
    "resize_window",
    "resize_pane",
    "move_window",
    "swap_pane",
    "set_pane_title",
    "enter_copy_mode",
    "exit_copy_mode",
    "wait_for_channel",
    "signal_channel",
    "set_mouse_enabled",
    "set_history_limit",
];
const EXECUTE: &[&str] = &[
    "create_session",
    "create_window",
    "split_window",
    "respawn_pane",
    "run_shell_command",
    "send_keys",
    "send_keys_batch",
    "paste_text",
    "set_synchronize_panes",
];
const TEARDOWN: &[&str] = &[
    "clear_pane_scrollback",
    "kill_pane",
    "kill_window",
    "kill_session",
];

fn tools(toolsets: &str) -> Result<TmuxTools, Box<dyn Error>> {
    Ok(TmuxTools::builder(libtmux::Server::new()?)
        .selection(Selection::parse(Some(toolsets), None, None)?)
        .build())
}

#[test]
fn every_toolset_permutation_is_exact() -> TestResult {
    let groups = [
        ("inspect", INSPECT),
        ("manage", MANAGE),
        ("execute", EXECUTE),
        ("teardown", TEARDOWN),
    ];
    for mask in 0_u8..16 {
        let selected = groups
            .iter()
            .enumerate()
            .filter(|(bit, _)| mask & (1 << bit) != 0)
            .map(|(_, (name, _))| *name)
            .collect::<Vec<_>>()
            .join(",");
        let expected: BTreeSet<_> = groups
            .iter()
            .enumerate()
            .filter(|(bit, _)| mask & (1 << bit) != 0)
            .flat_map(|(_, (_, names))| names.iter().map(|name| (*name).to_owned()))
            .collect();
        let actual: BTreeSet<_> = tools(&selected)?
            .offered()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();

        assert_eq!(actual, expected, "LIBTMUX_TOOLSETS={selected:?}");
    }
    Ok(())
}

#[test]
fn every_advertised_schema_is_valid_and_closed() -> TestResult {
    let all_tools = tools("inspect,manage,execute,teardown")?;
    for tool in all_tools.offered() {
        let input = serde_json::Value::Object((*tool.input_schema).clone());
        jsonschema::draft202012::meta::validate(&input)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        assert_eq!(input["additionalProperties"], false, "{} input", tool.name);
        if let Some(output) = tool.output_schema {
            let output = serde_json::Value::Object((*output).clone());
            jsonschema::draft202012::meta::validate(&output)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
        }
    }
    Ok(())
}

#[test]
fn configured_process_routes_have_no_executable_payload() -> TestResult {
    let tools = tools("execute")?;
    for name in [
        "create_session",
        "create_window",
        "split_window",
        "respawn_pane",
    ] {
        let tool = tools
            .offered()
            .into_iter()
            .find(|tool| tool.name == name)
            .expect("configured-process route");
        let properties = tool.input_schema["properties"]
            .as_object()
            .expect("object properties");
        for prohibited in ["command", "environment", "env"] {
            assert!(!properties.contains_key(prohibited), "{name}: {prohibited}");
        }
    }
    Ok(())
}

#[test]
fn foreground_commands_expose_no_retired_job_handle() -> TestResult {
    let tool = tools("execute")?
        .offered()
        .into_iter()
        .find(|tool| tool.name == "run_shell_command")
        .expect("pane-command route");
    let output = tool.output_schema.expect("typed output");
    let properties = output["properties"].as_object().expect("object properties");

    assert!(!properties.contains_key("job"));
    Ok(())
}

#[test]
fn pane_text_results_also_declare_their_structured_tmux_metadata() -> TestResult {
    let tools = tools("inspect,execute")?;
    for name in [
        "capture_pane",
        "capture_since",
        "wait_for_text",
        "run_shell_command",
    ] {
        let tool = tools
            .offered()
            .into_iter()
            .find(|tool| tool.name == name)
            .expect("pane text route");
        let capability = tool
            .meta
            .as_ref()
            .and_then(|meta| meta.0.get("com.git-pull.libtmux-mcp/capability"))
            .expect("capability row");

        assert_eq!(
            capability["outputClasses"],
            json!(["tmux-metadata", "terminal-content"]),
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn choice_vocabularies_reject_unknown_values() -> TestResult {
    let tools = tools("manage,execute")?;
    for (name, valid, invalid) in [
        (
            "split_window",
            json!({"pane": "%1", "direction": "above"}),
            json!({"pane": "%1", "direction": "sideways"}),
        ),
        (
            "resize_pane",
            json!({"pane": "%1", "direction": "left", "cells": 1}),
            json!({"pane": "%1", "direction": "inward", "cells": 1}),
        ),
        (
            "select_window",
            json!({"window": "@1", "direction": "last"}),
            json!({"window": "@1", "direction": "first"}),
        ),
    ] {
        let tool = tools
            .offered()
            .into_iter()
            .find(|tool| tool.name == name)
            .expect("route");
        let schema = serde_json::Value::Object((*tool.input_schema).clone());
        let validator = jsonschema::draft202012::new(&schema)?;
        assert!(validator.is_valid(&valid), "{name} rejected {valid}");
        assert!(!validator.is_valid(&invalid), "{name} accepted {invalid}");
    }
    Ok(())
}

#[test]
fn batch_error_policy_is_a_typed_on_error_enum() -> TestResult {
    let tools = tools("inspect,execute")?;
    for name in ["call_read_tools_batch", "send_keys_batch"] {
        let tool = tools
            .offered()
            .into_iter()
            .find(|tool| tool.name == name)
            .expect("batch route");
        let schema = serde_json::Value::Object((*tool.input_schema).clone());
        let properties = schema["properties"].as_object().expect("input properties");
        let on_error = &properties["on_error"];
        let enum_schema = on_error["$ref"]
            .as_str()
            .and_then(|reference| reference.strip_prefix("#/$defs/"))
            .map_or(on_error, |name| &schema["$defs"][name]);
        let values: BTreeSet<_> = enum_schema["enum"]
            .as_array()
            .expect("typed on_error enum")
            .iter()
            .map(|value| value.as_str().expect("on_error value"))
            .collect();

        assert_eq!(values, BTreeSet::from(["continue", "stop"]), "{name}");
        assert!(!properties.contains_key("continue_on_error"), "{name}");
    }
    Ok(())
}

#[test]
fn pattern_schemas_publish_the_runtime_limits() -> TestResult {
    let tools = tools("inspect")?;
    for (name, valid, invalid) in [
        (
            "search_panes",
            json!({"pattern": "x".repeat(4096)}),
            json!({"pattern": "x".repeat(4097)}),
        ),
        (
            "wait_for_text",
            json!({"pane": "%1", "patterns": vec!["x"; 32]}),
            json!({"pane": "%1", "patterns": vec!["x"; 33]}),
        ),
    ] {
        let tool = tools
            .offered()
            .into_iter()
            .find(|tool| tool.name == name)
            .expect("route");
        let schema = serde_json::Value::Object((*tool.input_schema).clone());
        let validator = jsonschema::draft202012::new(&schema)?;

        assert!(validator.is_valid(&valid), "{name} rejected its limit");
        assert!(!validator.is_valid(&invalid), "{name} exceeded its limit");
    }
    Ok(())
}

#[test]
fn tmux_variable_schema_accepts_only_a_bounded_name_list() -> TestResult {
    let tool = tools("inspect")?
        .offered()
        .into_iter()
        .find(|tool| tool.name == "get_tmux_variables")
        .expect("tmux variable route");
    let schema = serde_json::Value::Object((*tool.input_schema).clone());
    let validator = jsonschema::draft202012::new(&schema)?;

    assert!(validator.is_valid(&json!({"names": ["session_name"], "pane": "%1"})));
    for invalid in [
        json!({"names": []}),
        json!({"names": vec!["pane_id"; 33]}),
        json!({"names": ["#{pane_id}"]}),
        json!({"names": ["pane-id"]}),
    ] {
        assert!(!validator.is_valid(&invalid), "accepted {invalid}");
    }
    Ok(())
}

#[test]
fn read_batch_schema_names_exact_effective_nested_authority() -> TestResult {
    let eligible: Vec<_> = INSPECT
        .iter()
        .copied()
        .filter(|name| !matches!(*name, "wait_for_text" | "call_read_tools_batch"))
        .collect();
    let mut expected: BTreeSet<_> = eligible.iter().copied().collect();
    expected.remove("capture_pane");
    let selected = Selection::parse(Some("inspect"), None, Some("capture_pane"))?;
    let tools = TmuxTools::builder(libtmux::Server::new()?)
        .selection(selected)
        .build();
    let batch = tools
        .offered()
        .into_iter()
        .find(|tool| tool.name == "call_read_tools_batch")
        .expect("batch route");
    let schema = serde_json::Value::Object((*batch.input_schema).clone());
    let operations = &schema["properties"]["operations"];
    assert_eq!(operations["minItems"], 1);
    assert_eq!(operations["maxItems"], 16);
    let names: BTreeSet<_> = schema["properties"]["operations"]["items"]["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("nested tool enum: {schema}"))
        .iter()
        .map(|branch| {
            branch["properties"]["tool"]["const"]
                .as_str()
                .expect("tool name")
        })
        .collect();

    assert!(!names.contains("capture_pane"));
    assert_eq!(names, expected);

    let exclusions = eligible.join(",");
    let selected = Selection::parse(Some("inspect"), None, Some(&exclusions))?;
    let tools = TmuxTools::builder(libtmux::Server::new()?)
        .selection(selected)
        .build();
    let batch = tools
        .offered()
        .into_iter()
        .find(|tool| tool.name == "call_read_tools_batch")
        .expect("batch route");
    let schema = serde_json::Value::Object((*batch.input_schema).clone());
    jsonschema::draft202012::meta::validate(&schema)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let validator = jsonschema::draft202012::new(&schema)?;
    assert!(!validator.is_valid(&json!({
        "operations": [{"tool": "list_sessions", "arguments": {}}],
        "on_error": "stop"
    })));
    Ok(())
}

#[test]
fn capability_rows_use_the_shared_sink_and_literalization_vocabulary() -> TestResult {
    let all_tools = tools("inspect,manage,execute,teardown")?;
    let allowed: BTreeSet<_> = [
        "none",
        "tmux-lookup",
        "tmux-state",
        "tmux-format",
        "pane-input",
        "shell-command",
        "process-argv",
        "regex",
        "nested-tool",
    ]
    .into_iter()
    .collect();

    for tool in all_tools.offered() {
        let capability = tool
            .meta
            .as_ref()
            .and_then(|meta| meta.0.get("com.git-pull.libtmux-mcp/capability"))
            .expect("capability row");
        let sinks = capability["inputSinks"].as_object().expect("input sinks");
        let literalization = capability["inputLiteralization"]
            .as_object()
            .expect("input literalization");
        let format_controls = capability["tmuxFormatControls"]
            .as_object()
            .expect("tmux format controls");
        let format_inputs: BTreeSet<_> = sinks
            .iter()
            .filter(|(_, values)| {
                values
                    .as_array()
                    .is_some_and(|values| values.iter().any(|value| value == "tmux-format"))
            })
            .map(|(name, _)| name.as_str())
            .collect();
        let controlled_inputs: BTreeSet<_> = format_controls.keys().map(String::as_str).collect();
        let literalized_inputs: BTreeSet<_> = format_controls
            .iter()
            .filter(|(_, control)| *control == "double-hash-once")
            .map(|(name, _)| name.as_str())
            .collect();

        for values in sinks.values() {
            for sink in values.as_array().expect("sink set") {
                let sink = sink.as_str().expect("sink name");
                assert!(allowed.contains(sink), "{}: {sink}", tool.name);
            }
        }
        assert_eq!(controlled_inputs, format_inputs, "{}", tool.name);
        assert_eq!(
            literalization
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            literalized_inputs,
            "{}",
            tool.name,
        );
        assert!(
            format_controls.values().all(|strategy| matches!(
                strategy.as_str(),
                Some("double-hash-once" | "validated-variable-name")
            )),
            "{}: {format_controls:?}",
            tool.name,
        );
    }

    let variables = tools("inspect")?
        .offered()
        .into_iter()
        .find(|tool| tool.name == "get_tmux_variables")
        .expect("variable tool");
    let capability = variables
        .meta
        .as_ref()
        .and_then(|meta| meta.0.get("com.git-pull.libtmux-mcp/capability"))
        .expect("capability row");
    assert_eq!(capability["inputSinks"]["names"], json!(["tmux-format"]));
    assert_eq!(
        capability["tmuxFormatControls"]["names"],
        "validated-variable-name"
    );
    assert!(capability["inputLiteralization"].get("names").is_none());
    Ok(())
}

#[test]
fn aggregate_only_selection_keeps_native_pruned_operation_schemas() -> TestResult {
    let selected = Selection::parse(
        Some(""),
        Some("call_read_tools_batch"),
        Some("capture_pane"),
    )?;
    let tools = TmuxTools::builder(libtmux::Server::new()?)
        .selection(selected)
        .build();
    let offered = tools.offered();
    assert_eq!(offered.len(), 1);
    let batch = &offered[0];
    assert_eq!(batch.name, "call_read_tools_batch");
    let capability = batch
        .meta
        .as_ref()
        .and_then(|meta| meta.0.get("com.git-pull.libtmux-mcp/capability"))
        .expect("capability row");
    let authority: BTreeSet<_> = capability["nestedAuthority"]
        .as_array()
        .expect("nested authority")
        .iter()
        .map(|name| name.as_str().expect("nested name"))
        .collect();
    assert_eq!(authority.len(), 15);
    assert!(!authority.contains("capture_pane"));

    let schema = serde_json::Value::Object((*batch.input_schema).clone());
    jsonschema::draft202012::meta::validate(&schema)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let validator = jsonschema::draft202012::new(&schema)?;
    assert!(validator.is_valid(&json!({
        "operations": [{"tool": "list_sessions", "arguments": {}}],
        "on_error": "stop"
    })));
    assert!(validator.is_valid(&json!({
        "operations": [{"tool": "get_pane_info", "arguments": {"pane": "%1"}}],
        "on_error": "stop"
    })));
    assert!(!validator.is_valid(&json!({
        "operations": [{"tool": "get_pane_info", "arguments": {}}],
        "on_error": "stop"
    })));
    assert!(!validator.is_valid(&json!({
        "operations": [{"tool": "list_sessions", "arguments": {"extra": true}}],
        "on_error": "stop"
    })));
    assert!(!validator.is_valid(&json!({
        "operations": [{"tool": "capture_pane", "arguments": {"pane": "%1"}}],
        "on_error": "stop"
    })));
    Ok(())
}

#[test]
fn empty_aggregate_authority_has_a_valid_impossible_schema() -> TestResult {
    let eligible = INSPECT
        .iter()
        .copied()
        .filter(|name| !matches!(*name, "wait_for_text" | "call_read_tools_batch"))
        .collect::<Vec<_>>();
    let selected = Selection::parse(
        Some(""),
        Some("call_read_tools_batch"),
        Some(&eligible.join(",")),
    )?;
    let tools = TmuxTools::builder(libtmux::Server::new()?)
        .selection(selected)
        .build();
    let batch = tools.offered().into_iter().next().expect("batch route");
    let schema = serde_json::Value::Object((*batch.input_schema).clone());
    jsonschema::draft202012::meta::validate(&schema)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let validator = jsonschema::draft202012::new(&schema)?;
    assert!(!validator.is_valid(&json!({
        "operations": [{"tool": "list_sessions", "arguments": {}}],
        "on_error": "stop"
    })));
    let capability = batch
        .meta
        .as_ref()
        .and_then(|meta| meta.0.get("com.git-pull.libtmux-mcp/capability"))
        .expect("capability row");
    assert_eq!(capability["nestedAuthority"], json!([]));
    assert_eq!(capability["tmuxEffects"], json!(["observe"]));
    assert_eq!(capability["outputClasses"], json!([]));
    Ok(())
}

#[test]
fn retired_background_job_subsystem_is_structurally_absent() -> TestResult {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert!(!source.join("jobs.rs").exists());
    assert!(!source.join("jobs").exists());

    let library = std::fs::read_to_string(source.join("lib.rs"))?;
    let observer = std::fs::read_to_string(source.join("tools/observe.rs"))?;
    let errors = std::fs::read_to_string(source.join("tools/error.rs"))?;
    let joined = format!("{library}\n{observer}\n{errors}");
    for retired in [
        "mod jobs;",
        "JobTable",
        "MAX_JOBS",
        "background job capacity",
    ] {
        assert!(
            !joined.contains(retired),
            "retired job token remains: {retired}"
        );
    }
    Ok(())
}

#[test]
fn aggregate_effects_and_outputs_follow_pruned_authority() -> TestResult {
    let selected = Selection::parse(
        Some(""),
        Some("call_read_tools_batch"),
        Some("capture_since"),
    )?;
    let tools = TmuxTools::builder(libtmux::Server::new()?)
        .selection(selected)
        .build();
    let batch = tools.offered().into_iter().next().expect("batch route");
    let capability = batch
        .meta
        .as_ref()
        .and_then(|meta| meta.0.get("com.git-pull.libtmux-mcp/capability"))
        .expect("capability row");

    assert_eq!(capability["tmuxEffects"], json!(["observe"]));
    assert_eq!(
        capability["outputClasses"],
        json!([
            "tmux-metadata",
            "terminal-content",
            "process-environment",
            "configured-command"
        ])
    );

    let excluded = INSPECT
        .iter()
        .copied()
        .filter(|name| {
            !matches!(
                *name,
                "wait_for_text" | "call_read_tools_batch" | "capture_pane"
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let selected = Selection::parse(Some(""), Some("call_read_tools_batch"), Some(&excluded))?;
    let tools = TmuxTools::builder(libtmux::Server::new()?)
        .selection(selected)
        .build();
    let batch = tools.offered().into_iter().next().expect("batch route");
    let capability = batch
        .meta
        .as_ref()
        .and_then(|meta| meta.0.get("com.git-pull.libtmux-mcp/capability"))
        .expect("capability row");
    assert_eq!(capability["tmuxEffects"], json!(["observe"]));
    assert_eq!(
        capability["outputClasses"],
        json!(["tmux-metadata", "terminal-content"])
    );
    Ok(())
}
