//! Generated artifacts reflect parser contracts without starting a backend.
#![cfg(feature = "cli")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::process::{Command, Output};

use serde_json::Value;

fn cli(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args(arguments)
        .env_clear()
        .env("PATH", "/unavailable")
        .env("HOME", "/unavailable")
        .env("LIBTMUX_TEST_TMUX", "/unavailable/tmux")
        .env("TMUX_WORKSPACE_PYTHON", "/unavailable/python")
        .env("TMUXP_PROGRESS_LINES", "invalid-but-dormant")
        .output()
        .unwrap()
}

fn generated(arguments: &[&str]) -> Vec<u8> {
    let output = cli(arguments);
    assert!(output.status.success(), "{arguments:?}: {output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    output.stdout
}

fn command<'a>(root: &'a Value, name: &str) -> &'a Value {
    root["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap()
}

fn argument<'a>(command: &'a Value, name: &str) -> &'a Value {
    command["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap()
}

#[test]
fn generated_metadata_exposes_parser_constraints_and_labels_runtime_environment() {
    let schema: Value = serde_json::from_slice(&generated(&["--generate", "schema"])).unwrap();
    let root = &schema["command"];
    let load = command(root, "load");
    assert_eq!(argument(load, "workspace_files")["index"], 1);
    assert_eq!(argument(load, "workspace_files")["arity"]["min"], 1);
    assert!(argument(load, "workspace_files")["arity"]["max"].is_null());
    let progress = argument(load, "progress-lines");
    assert_eq!(progress["arity"], serde_json::json!({"min":1,"max":1}));
    assert_eq!(progress["numeric_bounds"]["minimum"], -1);
    assert_eq!(progress["numeric_bounds"]["maximum"], i32::MAX);
    assert_eq!(progress["numeric_bounds"]["source"], "argument declaration");
    assert!(progress["environment"].is_null());
    assert_eq!(
        argument(load, "colors256")["conflicts"],
        serde_json::json!(["colors88"])
    );
    let shell = command(root, "shell");
    assert_eq!(argument(shell, "window_name")["index"], 2);
    assert_eq!(
        argument(shell, "use-vi-mode")["overrides"],
        serde_json::json!(["no-vi-mode"])
    );
    let backend = shell["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["name"] == "backend")
        .unwrap();
    assert_eq!(backend["multiple"], false);
    assert!(
        backend["members"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("ipython"))
    );
    assert_eq!(command(root, "import")["subcommand_required"], true);
    let environment = schema["runtime_environment"].as_array().unwrap();
    let lines = environment
        .iter()
        .find(|v| v["name"] == "TMUXP_PROGRESS_LINES")
        .unwrap();
    assert_eq!(lines["binding"], "runtime");
    assert!(lines["condition"].as_str().unwrap().contains("active"));
    for arguments in [
        vec![
            "load",
            "/unavailable/workspace",
            "-d",
            "--progress-lines",
            "-2",
        ],
        vec![
            "load",
            "/unavailable/workspace",
            "-d",
            "--progress-lines",
            "2147483648",
        ],
        vec!["load", "/unavailable/workspace", "-d", "-2", "-8"],
        vec!["shell", "--ipython", "--bpython"],
    ] {
        assert_eq!(cli(&arguments).status.code(), Some(2), "{arguments:?}");
    }
    for value in ["-1", "0", "2147483647"] {
        let output = cli(&[
            "load",
            "/unavailable/workspace",
            "-d",
            "--progress-lines",
            value,
        ]);
        assert_eq!(output.status.code(), Some(1), "{value}: {output:?}");
    }
    let dormant = cli(&["load", "/unavailable/workspace", "-d", "--json"]);
    assert_eq!(dormant.status.code(), Some(1), "{dormant:?}");
}

#[test]
fn generated_manual_contains_leaf_and_nested_command_instructions() {
    let output = generated(&["--generate", "man"]);
    let manual = String::from_utf8(output).unwrap();
    for heading in [
        "tmux-workspace load",
        "tmux-workspace freeze",
        "tmux-workspace import teamocil",
        "tmux-workspace import tmuxinator",
        "tmux-workspace shell",
    ] {
        assert!(manual.contains(heading), "missing {heading}");
    }
    for option in ["progress\\-lines", "save\\-to", "use\\-pythonrc"] {
        assert!(manual.contains(option), "missing {option}");
    }
    assert_eq!(manual.matches(".TH ").count(), 1);
    assert!(!manual.contains("tmux-workspace-load(1)"));
}

#[test]
fn machine_generation_wraps_the_exact_human_artifact() {
    for format in [
        "schema",
        "man",
        "bash",
        "zsh",
        "fish",
        "powershell",
        "elvish",
    ] {
        let human = generated(&["--generate", format]);
        let output = generated(&["--generate", format, "--json"]);
        let document: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(document["command"], "generate", "{format}");
        assert_eq!(document["status"], "ok");
        assert_eq!(document["artifact"]["format"], format);
        assert_eq!(document["artifact"]["encoding"], "utf-8");
        assert_eq!(
            document["artifact"]["content"].as_str().unwrap().as_bytes(),
            human
        );
        let stream = generated(&["--generate", format, "--ndjson", "--json"]);
        let events: Vec<Value> = stream
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(events.len(), 1, "{format}");
        assert_eq!(events[0]["event"], "completed");
        assert_eq!(events[0]["sequence"], 1);
        assert_eq!(events[0]["artifact"], document["artifact"]);
    }
    assert_eq!(generated(&["--help"]), generated(&["--json", "--help"]));
    assert_eq!(
        generated(&["load", "--help"]),
        generated(&["load", "--ndjson", "--help"])
    );
}
