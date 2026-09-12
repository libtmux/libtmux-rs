//! Installed command grammar and process stream contracts.
#![cfg(feature = "cli")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::Path;
use std::process::{Command, Output};

fn at(arguments: &[&str], directory: &Path) -> Output {
    at_pane(arguments, directory, None)
}

fn at_pane(arguments: &[&str], directory: &Path, pane: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"));
    command
        .args(arguments)
        .current_dir(directory)
        .env("HOME", directory)
        .env("TMUXP_CONFIGDIR", directory.join(".tmuxp"))
        .env("XDG_CONFIG_HOME", directory.join(".config"))
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    if let Some(pane) = pane {
        command.env("TMUX_PANE", pane);
    }
    command.output().unwrap()
}

async fn current_pane(session: &libtmux::Session) -> String {
    session
        .active_window()
        .await
        .unwrap()
        .unwrap()
        .active_pane()
        .await
        .unwrap()
        .unwrap()
        .id()
        .to_string()
}

fn cli(arguments: &[&str]) -> Output {
    let binary = env!("CARGO_BIN_EXE_tmux-workspace");
    Command::new(binary)
        .args(arguments)
        .env("PATH", "/nonexistent")
        .env_remove("LIBTMUX_TEST_TMUX")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .expect("run workspace CLI")
}

#[test]
fn help_and_version_do_not_require_tmux() {
    let output = cli(&["--help"]);
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
        "convert",
        "debug-info",
        "edit",
        "freeze",
        "import",
        "load",
        "ls",
        "search",
        "shell",
    ] {
        assert!(help.contains(command), "missing command {command}: {help}");
    }
    assert!(cli(&["--version"]).status.success());
}

#[test]
fn generated_metadata_completion_and_manual_use_the_command_graph() {
    let output = cli(&["--generate", "schema"]);
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let commands = value["command"]["subcommands"].as_array().unwrap();
    assert_eq!(commands.len(), 9);
    let load = commands.iter().find(|c| c["name"] == "load").unwrap();
    let colors = load["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "colors88")
        .unwrap();
    assert_eq!(colors["long"], "88-colors");
    assert!(
        colors["description"]
            .as_str()
            .unwrap()
            .contains("Reject legacy 88-color mode")
    );
    assert_eq!(
        commands.iter().find(|c| c["name"] == "import").unwrap()["subcommands"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for format in ["bash", "zsh", "fish", "powershell", "elvish", "man"] {
        let output = cli(&["--generate", format]);
        assert!(output.status.success(), "{format}: {output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("tmux-workspace"));
        if format != "man" {
            assert!(String::from_utf8_lossy(&output.stdout).contains("88-colors"));
        }
    }
}

#[test]
fn malformed_invocations_fail_before_backend_work() {
    for args in [
        vec!["--json", "load", "-2", "-8", "missing"],
        vec!["--json", "load", "-8", "-2", "missing"],
        vec!["--json", "load", "-2", "--88-colors", "missing"],
        vec!["--json", "load", "--88-colors", "-2", "missing"],
        vec!["load", "--json", "--unknown", "missing"],
        vec!["--json", "import", "teamocil"],
        vec!["import", "tmuxinator", "--ndjson"],
        vec!["shell", "--json", "--code", "--ipython"],
        vec!["freeze", "--json", "-f", "xml"],
        vec!["search", "--json"],
        vec!["search", "--json", "["],
    ] {
        let output = cli(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        assert!(output.stderr.starts_with(b"{"), "{args:?}: {output:?}");
        assert!(!output.stderr.contains(&0x1b), "{args:?}: {output:?}");
    }
}

#[test]
fn legacy_color_mode_is_rejected_before_reading_inputs() {
    for flag in ["-8", "--88-colors"] {
        for mode in [None, Some("--json"), Some("--ndjson")] {
            let mut args = vec!["load", "-d", flag, "missing-first", "missing-second"];
            args.extend(mode);
            let output = cli(&args);
            assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
            assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
            assert!(!output.stderr.contains(&0x1b));
            if mode.is_some() {
                let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
                assert_eq!(error["code"], "unsupported_color_mode");
            } else {
                assert!(String::from_utf8_lossy(&output.stderr).contains("88-color"));
            }
        }
    }
}

#[tokio::test]
async fn color_validation_preserves_sessions_and_256_reaches_tmux() {
    use std::os::unix::fs::PermissionsExt as _;

    let guard = libtmux::test::TestServer::new().await.unwrap();
    guard
        .server()
        .new_session(libtmux::NewSessionOptions::new("existing"))
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let trace = directory.path().join("trace");
    let wrapper = directory.path().join("tmux");
    let python = directory.path().join("python");
    std::fs::write(&wrapper, "#!/bin/sh\nprintf '<%s>' \"$@\" >> \"$WORKSPACE_TMUX_TRACE\"\nprintf '\\n' >> \"$WORKSPACE_TMUX_TRACE\"\nexec \"$WORKSPACE_REAL_TMUX\" \"$@\"\n").unwrap();
    std::fs::write(
        &python,
        "#!/bin/sh\nprintf 'python\\n' >> \"$WORKSPACE_TMUX_TRACE\"\nexit 97\n",
    )
    .unwrap();
    for executable in [&wrapper, &python] {
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for name in ["first", "second", "bridge"] {
        let mut workspace =
            serde_json::json!({"session_name":name,"windows":[{"panes":["blank"]}]});
        if name == "bridge" {
            workspace["plugins"] = serde_json::json!(["missing_plugin"]);
        }
        std::fs::write(
            directory.path().join(format!("{name}.json")),
            workspace.to_string(),
        )
        .unwrap();
    }
    let topology = || {
        guard.server().cmd(
            libtmux::Command::new("list-panes")
                .arg("-a")
                .arg("-F")
                .arg("#{session_id}:#{window_id}:#{pane_id}"),
        )
    };
    let before = topology().await.unwrap();
    assert!(before.success());
    let run = |options: &[&str], files: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
            .args([
                "load",
                "-d",
                "-S",
                guard.server().socket_path().to_str().unwrap(),
            ])
            .args(options)
            .args(files)
            .current_dir(directory.path())
            .env("LIBTMUX_TEST_TMUX", &wrapper)
            .env("TMUX_WORKSPACE_PYTHON", &python)
            .env("WORKSPACE_TMUX_TRACE", &trace)
            .env(
                "WORKSPACE_REAL_TMUX",
                std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into()),
            )
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .unwrap()
    };
    for flag in ["-8", "--88-colors"] {
        for mode in [None, Some("--json"), Some("--ndjson")] {
            let mut options = vec![flag];
            options.extend(mode);
            for last in ["second.json", "bridge.json"] {
                let output = run(&options, &["first.json", last]);
                assert_eq!(output.status.code(), Some(2), "{options:?}: {output:?}");
                assert!(output.stdout.is_empty(), "{output:?}");
                assert!(!trace.exists(), "validation invoked tmux or Python");
                assert_eq!(topology().await.unwrap().stdout(), before.stdout());
            }
        }
    }
    let output = run(&["--json", "-2"], &["first.json", "second.json"]);
    assert!(output.status.success(), "{output:?}");
    for name in ["first", "second"] {
        assert!(guard.server().has_session(name).await.unwrap());
    }
    let calls = std::fs::read_to_string(trace).unwrap();
    assert!(calls.contains("<-2>"), "{calls}");
    assert!(
        calls
            .lines()
            .all(|line| line == "<-V>" || line.contains("<-2>")),
        "{calls}"
    );
    guard.shutdown().await.unwrap();
}

#[test]
fn every_leaf_accepts_machine_flags_before_or_after_its_name() {
    for leaf in [
        vec!["convert"],
        vec!["debug-info"],
        vec!["edit"],
        vec!["freeze"],
        vec!["import", "teamocil"],
        vec!["import", "tmuxinator"],
        vec!["load"],
        vec!["ls"],
        vec!["search"],
        vec!["shell"],
    ] {
        for flag in ["--json", "--ndjson"] {
            for before in [true, false] {
                let mut args = leaf.clone();
                if before {
                    args.insert(0, flag);
                } else {
                    args.push(flag);
                }
                args.push("--help");
                let output = cli(&args);
                assert!(output.status.success(), "{args:?}: {output:?}");
                assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
            }
        }
    }
}

#[test]
fn empty_discovery_has_stable_json_and_ndjson_shapes() {
    let directory = tempfile::tempdir().unwrap();
    let output = at(&["ls", "--json"], directory.path());
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["workspaces"], serde_json::json!([]));
    assert!(value["global_workspace_dirs"].is_array());
    assert!(value["global_workspace_dirs"][0]["exists"].is_boolean());
    assert!(value["global_workspace_dirs"][0]["workspace_count"].is_number());
    let output = at(&["--json", "ls", "--ndjson"], directory.path());
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    let output = at(&["search", "--json", "absent"], directory.path());
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!([])
    );
}

#[test]
fn conversion_preserves_extension_values_and_protects_existing_files() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("project.yaml"), "session_name: demo\nwindows: []\ncustom:\n  n: 42\n  yes: true\n  values: [null, 'a\\nb', '雪']\n").unwrap();
    let output = at(&["convert", "project.yaml", "--json"], directory.path());
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["custom"]["n"], 42);
    assert_eq!(value["custom"]["yes"], true);
    assert_eq!(value["custom"]["values"][2], "雪");
    assert!(!directory.path().join("project.json").exists());
    std::fs::write(directory.path().join("kept.json"), "retained").unwrap();
    let output = at(
        &[
            "convert",
            "project.yaml",
            "--json",
            "--save-to",
            "kept.json",
        ],
        directory.path(),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(directory.path().join("kept.json")).unwrap(),
        "retained"
    );
    let output = at(
        &[
            "convert",
            "project.yaml",
            "--ndjson",
            "--save-to",
            "kept.json",
            "--force",
        ],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.path().join("kept.json")).unwrap())
            .unwrap();
    assert_eq!(saved, value);
}

#[test]
fn importers_transform_native_source_documents() {
    let directory = tempfile::tempdir().unwrap();
    for (kind, source) in [
        (
            "teamocil",
            "session:\n  name: demo\n  windows:\n    - name: editor\n      splits:\n        - cmd: echo hello\n",
        ),
        (
            "tmuxinator",
            "name: demo\nroot: /tmp\nwindows:\n  - editor:\n      panes:\n        - echo hello\n",
        ),
    ] {
        std::fs::write(directory.path().join("input.yaml"), source).unwrap();
        let output = at(&["import", kind, "input.yaml", "--json"], directory.path());
        assert!(output.status.success(), "{kind}: {output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["session_name"], "demo");
        assert_eq!(value["windows"][0]["window_name"], "editor");
        assert_eq!(value["windows"][0]["panes"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn discovery_and_search_use_workspace_fields_and_case_modes() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join(".tmuxp")).unwrap();
    std::fs::write(directory.path().join(".tmuxp/demo.yaml"), "session_name: Project\nwindows:\n  - window_name: Editor\n    panes:\n      - echo Needle\n").unwrap();
    let output = at(&["ls", "--json", "--full"], directory.path());
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(value["workspaces"][0]["session_name"], "Project");
    for query in [
        vec!["-i", "pane:needle"],
        vec!["-S", "session:project", "window:editor"],
        vec!["-F", "-f", "name", "demo"],
    ] {
        let mut args = vec!["search", "--json"];
        args.extend(query);
        let output = at(&args, directory.path());
        assert!(output.status.success(), "{args:?}: {output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value.as_array().unwrap().len(), 1, "{args:?}: {value}");
        assert!(value[0]["matched_fields"].is_array());
    }
}

#[tokio::test]
async fn native_load_freeze_reuse_and_append_preserve_owned_session_state() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("created");
    let source = serde_json::json!({"session_name":"cli-native","start_directory":directory.path(),"environment":{"SESSION_VALUE":"session"},"windows":[
        {"window_name":"zero","window_index":0,"environment":{"WINDOW_VALUE":"window"},"options":{"automatic-rename":false},"panes":[{"shell_command":format!("printf '%s' \"$SESSION_VALUE:$WINDOW_VALUE\" > {}; exec sleep 30",marker.display())},null]},
        {"window_name":"seven","window_index":7,"panes":["blank"]}
    ]});
    std::fs::write(directory.path().join("project.json"), source.to_string()).unwrap();
    let socket = guard.server().socket_path().to_str().unwrap();
    let output = at(
        &["load", "-S", socket, "-d", "--ndjson", "project.json"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.last().unwrap()["event"], "completed");
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event"] == "completed")
            .count(),
        1
    );
    assert!(events.iter().any(|event| event["event"] == "pane-created"));
    for pair in events.windows(2) {
        assert!(pair[0]["sequence"].as_u64().unwrap() < pair[1]["sequence"].as_u64().unwrap());
    }
    for _ in 0..100 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "session:window");
    let output = at(
        &["freeze", "-S", socket, "--json", "cli-native"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let captured: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(captured["windows"][0]["window_index"], 0);
    assert_eq!(captured["windows"][1]["window_index"], 7);
    assert_eq!(captured["windows"][0]["panes"].as_array().unwrap().len(), 2);
    let output = at(
        &["load", "-S", socket, "-d", "--json", "project.json"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    std::fs::write(
        directory.path().join("append.yaml"),
        "session_name: ignored\nwindows:\n  - window_name: appended\n    panes: [blank]\n",
    )
    .unwrap();
    let session = guard.server().session("cli-native").await.unwrap().unwrap();
    let pane = current_pane(&session).await;
    let output = at_pane(
        &[
            "load",
            "-S",
            socket,
            "-s",
            "ignored-rename",
            "--append",
            "--json",
            "append.yaml",
        ],
        directory.path(),
        Some(&pane),
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        guard
            .server()
            .session("cli-native")
            .await
            .unwrap()
            .unwrap()
            .windows()
            .await
            .unwrap()
            .len(),
        3
    );
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn load_renames_only_the_last_input_and_retessellates_many_panes() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    for name in ["first", "second"] {
        std::fs::write(directory.path().join(format!("{name}.json")), serde_json::json!({"session_name":name,"windows":[{"window_name":"many","layout":"tiled","panes":vec!["blank";10]}]}).to_string()).unwrap();
    }
    let socket = guard.server().socket_path().to_str().unwrap();
    let output = at(
        &[
            "load",
            "-S",
            socket,
            "-d",
            "--json",
            "-s",
            "renamed",
            "first.json",
            "second.json",
        ],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["results"][0]["session_name"], "first");
    assert_eq!(value["results"][1]["session_name"], "renamed");
    let session = guard.server().session("renamed").await.unwrap().unwrap();
    assert_eq!(
        session
            .active_window()
            .await
            .unwrap()
            .unwrap()
            .panes()
            .await
            .unwrap()
            .len(),
        10
    );
    let output = at(
        &["load", "-S", socket, "--append", "--json", "first.json"],
        directory.path(),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("current_pane_required"));
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_append_reports_borrowed_session_and_created_ids() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let session = guard
        .server()
        .new_session(libtmux::NewSessionOptions::new("borrowed"))
        .await
        .unwrap();
    let pane = session
        .active_window()
        .await
        .unwrap()
        .unwrap()
        .active_pane()
        .await
        .unwrap()
        .unwrap()
        .id()
        .to_string();
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("broken.yaml"), "session_name: different\nwindows:\n  - window_name: created\n    panes: [blank]\n    options_after:\n      invalid-workspace-test-option: true\n").unwrap();
    let output = at_pane(
        &[
            "load",
            "-S",
            guard.server().socket_path().to_str().unwrap(),
            "--append",
            "--json",
            "broken.yaml",
        ],
        directory.path(),
        Some(&pane),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "partial");
    assert_eq!(value["errors"][0]["effects"]["session_name"], "borrowed");
    assert_eq!(value["errors"][0]["effects"]["owned_session"], false);
    assert_eq!(
        value["errors"][0]["effects"]["window_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(guard.server().has_session("borrowed").await.unwrap());
    guard.shutdown().await.unwrap();
}

#[test]
fn invalid_execution_models_are_rejected_before_creating_a_server() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("bad.yaml"), "session_name: bad\nwindows:\n  - panes:\n      - shell_command: [{cmd: hello, sleep_before: -1}]\n").unwrap();
    let output = cli(&[
        "load",
        "-d",
        "--json",
        directory.path().join("bad.yaml").to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("sleep_before"),
        "{output:?}"
    );
}

#[tokio::test]
async fn bootstrap_output_streams_escaped_records_before_the_child_finishes() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("bootstrap.sh"),
        "printf 'ready\\t雪\\033[31m\\n'\nwhile ! test -f released; do sleep 0.01; done\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("stream.yaml"),
        "session_name: streamed\nbefore_script: sh bootstrap.sh\nwindows:\n  - panes: [blank]\n",
    )
    .unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args([
            "load",
            "-d",
            "-S",
            guard.server().socket_path().to_str().unwrap(),
            "--ndjson",
            "stream.yaml",
        ])
        .current_dir(directory.path())
        .env("HOME", directory.path())
        .env_remove("TMUX")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let saw_script = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            assert!(!line.contains('\x1b'));
            let event: serde_json::Value = serde_json::from_str(&line).unwrap();
            if event["event"] == "script-output" {
                return event["text"]
                    .as_str()
                    .unwrap()
                    .contains("ready\t雪\x1b[31m");
            }
        }
        false
    })
    .await
    .unwrap();
    std::fs::write(directory.path().join("released"), "release").unwrap();
    let output = child.wait_with_output().await.unwrap();
    assert!(saw_script, "{output:?}");
    assert!(output.status.success(), "{output:?}");
    guard.shutdown().await.unwrap();
}

#[test]
fn editor_arguments_and_failure_status_are_preserved_in_machine_mode() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("project.yaml"),
        "session_name: editor\nwindows: []\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args(["edit", "project.yaml", "--json"])
        .current_dir(directory.path())
        .env(
            "EDITOR",
            "sh -c 'test -f \"$1\"; printf edited; exit 7' editor",
        )
        .env("HOME", directory.path())
        .env_remove("TMUX")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["child_status"], 7);
    assert_eq!(result["stdout"], "edited");
}

#[test]
fn diagnostics_are_structured_without_contacting_a_default_server() {
    let output = cli(&["debug-info", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["port"], "rust");
    assert_eq!(result["workspace_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(result["tmux_available"], false);
}

#[test]
fn missing_python_bridge_has_a_precise_runtime_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args(["shell", "--json", "-c", "print('bridge')"])
        .env("TMUX_WORKSPACE_PYTHON", "/nonexistent/python")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["code"], "python_runtime");
}

#[tokio::test]
#[ignore = "requires TMUX_WORKSPACE_PYTHON with tmuxp 1.74.0"]
async fn python_shell_uses_the_checked_console_entrypoint() {
    let python = std::env::var_os("TMUX_WORKSPACE_PYTHON").expect("bridge interpreter");
    let guard = libtmux::test::TestServer::new().await.unwrap();
    guard
        .server()
        .new_session(libtmux::NewSessionOptions::new("bridge"))
        .await
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args([
            "shell",
            "-S",
            guard.server().socket_path().to_str().unwrap(),
            "--json",
            "--code",
            "-c",
            "print(session.name)",
            "bridge",
        ])
        .env("TMUX_WORKSPACE_PYTHON", python)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["stdout"].as_str().unwrap().ends_with("\nbridge\n"));
    guard.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires TMUX_WORKSPACE_PYTHON with tmuxp 1.74.0"]
async fn python_extension_bridge_builds_and_preserves_borrowed_session_on_failure() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("extension.py"), "from tmuxp.workspace.builder.classic import ClassicWorkspaceBuilder\nfrom tmuxp.plugin import TmuxpPlugin\nclass Plugin(TmuxpPlugin):\n    def before_workspace_builder(self, session):\n        session.set_environment('PLUGIN_CALLED', 'yes')\nclass Builder(ClassicWorkspaceBuilder):\n    def build(self, *args, **kwargs):\n        super().build(*args, **kwargs)\n        self.session.set_environment('BRIDGE_CALLED', 'yes')\n").unwrap();
    let config = serde_json::json!({"session_name":"extension","workspace_builder":"extension:Builder","plugins":["extension.Plugin"],"workspace_builder_paths":[directory.path()],"windows":[{"window_name":"native","panes":["blank"]}]});
    std::fs::write(directory.path().join("extension.json"), config.to_string()).unwrap();
    let socket = guard.server().socket_path().to_str().unwrap();
    let output = at(
        &["load", "-S", socket, "-d", "-2", "--json", "extension.json"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let session = guard.server().session("extension").await.unwrap().unwrap();
    let environment = session.environment_all().await.unwrap();
    assert_eq!(
        environment.get("BRIDGE_CALLED"),
        Some(&libtmux::EnvironmentEntry::Set("yes".into()))
    );
    assert_eq!(
        environment.get("PLUGIN_CALLED"),
        Some(&libtmux::EnvironmentEntry::Set("yes".into()))
    );
    let pane = current_pane(&session).await;
    let mut broken = config;
    broken["before_script"] = serde_json::json!("sh -c 'exit 9'");
    std::fs::write(directory.path().join("broken.json"), broken.to_string()).unwrap();
    let output = at_pane(
        &["load", "-S", socket, "--append", "--json", "broken.json"],
        directory.path(),
        Some(&pane),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(guard.server().has_session("extension").await.unwrap());
    guard.shutdown().await.unwrap();
}

#[test]
fn interrupted_editor_terminates_its_owned_child_group() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("workspace.yaml"),
        "session_name: demo\nwindows: []\n",
    )
    .unwrap();
    let marker = directory.path().join("child.pid");
    let script = directory.path().join("editor.sh");
    std::fs::write(
        &script,
        format!(
            "sleep 30 &\nprintf '%s' $! > '{}'\nwait\n",
            marker.display()
        ),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args(["edit", "workspace.yaml", "--ndjson"])
        .current_dir(directory.path())
        .env("EDITOR", format!("sh {}", script.display()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    for _ in 0..200 {
        if marker.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let pid = std::fs::read_to_string(&marker).unwrap();
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while child.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let descendant_alive = Command::new("kill")
        .args(["-0", &pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    if descendant_alive {
        let _ = Command::new("kill").args(["-KILL", &pid]).status();
    }
    assert_eq!(output.status.code(), Some(130), "{output:?}");
    assert!(
        !descendant_alive,
        "owned editor descendant survived interrupt"
    );
}

#[test]
fn machine_editor_uses_a_controlling_terminal_without_contaminating_json() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("workspace.yaml"),
        "session_name: demo\nwindows: []\n",
    )
    .unwrap();
    let script = r"
import errno, fcntl, json, os, pathlib, pty, subprocess, sys, termios
master, slave = pty.openpty()
def session():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)
env = dict(os.environ, EDITOR='sh -c \'test -t 0 && test -t 1 && printf EDITOR_TTY\' editor')
with open('result.json', 'wb') as result:
    child = subprocess.Popen([sys.argv[1], 'edit', 'workspace.yaml', '--json'], stdin=slave, stdout=result, stderr=slave, env=env, preexec_fn=session)
os.close(slave)
text = b''
while True:
    try:
        chunk = os.read(master, 4096)
    except OSError as error:
        if error.errno == errno.EIO: break
        raise
    if not chunk: break
    text += chunk
os.close(master)
assert child.wait(timeout=3) == 0, text
assert b'EDITOR_TTY' in text, text
result = json.loads(pathlib.Path('result.json').read_text())
assert result['terminal'] is True, result
assert result['stdout'] == '', result
";
    let output = Command::new("python3")
        .args(["-c", script, env!("CARGO_BIN_EXE_tmux-workspace")])
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn exited_editor_cannot_leave_a_descendant_holding_capture_pipes() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("workspace.yaml"),
        "session_name: demo\nwindows: []\n",
    )
    .unwrap();
    let marker = directory.path().join("child.pid");
    let mut child = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args(["edit", "workspace.yaml", "--json"])
        .current_dir(directory.path())
        .env(
            "EDITOR",
            format!(
                "sh -c 'sleep 30 & echo $! > {}; printf ready' editor",
                marker.display()
            ),
        )
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while child.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let completed = child.try_wait().unwrap().is_some();
    if !completed {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let pid = std::fs::read_to_string(marker).unwrap();
    let _ = Command::new("kill")
        .args(["-KILL", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status();
    assert!(completed && output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["stdout"], "ready");
    assert_eq!(value["truncated"], true);
}

#[test]
fn successful_editor_can_leave_a_background_service_with_closed_streams() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("workspace.yaml"),
        "session_name: demo\nwindows: []\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args(["edit", "workspace.yaml", "--json"])
        .current_dir(directory.path())
        .env(
            "EDITOR",
            "sh -c 'sleep 30 >/dev/null 2>&1 & echo $!' editor",
        )
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let pid = value["stdout"].as_str().unwrap().trim();
    let state = Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .unwrap();
    let _ = Command::new("kill")
        .args(["-KILL", pid])
        .stderr(std::process::Stdio::null())
        .status();
    assert!(
        state.status.success() && !state.stdout.starts_with(b"Z"),
        "{state:?}"
    );
}
