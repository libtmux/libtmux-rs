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
    let tools = tools("inspect,manage,execute,teardown")?;
    for tool in tools.offered() {
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
