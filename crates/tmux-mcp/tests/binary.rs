//! The shipped process, including startup selection and stdio framing.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};

use libtmux::test::TestServer;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_tmux-mcp");

struct Process {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    seq: i64,
}

impl Process {
    fn start(args: &[&str], environment: &[(&str, &str)]) -> Self {
        let mut command = base_command();
        command.args(args);
        for (name, value) in environment {
            command.env(name, value);
        }
        let mut child = command.spawn().expect("the binary runs");
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let mut process = Self {
            child,
            stdin,
            stdout,
            seq: 0,
        };
        process.request(
            "initialize",
            &json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "binary-suite", "version": "0"},
            }),
        );
        process.notify("notifications/initialized");
        process
    }

    fn request(&mut self, method: &str, params: &Value) -> Value {
        self.seq += 1;
        let id = self.seq;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .expect("request writes");
        self.stdin.flush().expect("request flushes");
        loop {
            let mut line = String::new();
            assert_ne!(
                self.stdout.read_line(&mut line).expect("stdout reads"),
                0,
                "server closed while {method} was pending",
            );
            let message: Value = serde_json::from_str(line.trim_end()).unwrap_or_else(|error| {
                panic!("stdout carried non-JSON-RPC data: {line:?} ({error})")
            });
            if message.get("id") == Some(&json!(id)) {
                return message;
            }
        }
    }

    fn notify(&mut self, method: &str) {
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "method": method})
        )
        .expect("notification writes");
        self.stdin.flush().expect("notification flushes");
    }

    fn tool_names(&mut self) -> Vec<String> {
        self.request("tools/list", &json!({}))["result"]["tools"]
            .as_array()
            .expect("tool list")
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name").to_owned())
            .collect()
    }

    fn finish(self) -> String {
        drop(self.stdin);
        let output = self
            .child
            .wait_with_output()
            .expect("the process exits when stdin closes");
        assert!(output.status.success(), "{output:?}");
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
}

fn base_command() -> Command {
    let mut command = Command::new(BIN);
    command
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("LIBTMUX_SAFETY")
        .env_remove("TMUX_MCP_SAFETY")
        .env_remove("LIBTMUX_TOOLSETS")
        .env_remove("LIBTMUX_TOOLS")
        .env_remove("LIBTMUX_EXCLUDE_TOOLS")
        .env_remove("LIBTMUX_SOCKET")
        .env_remove("LIBTMUX_SOCKET_PATH")
        .env_remove("LIBTMUX_TMUX_CONFIG")
        .env_remove("TMUX_TMPDIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn failed_start(environment: &[(&str, &str)]) -> Output {
    let mut command = base_command();
    for (name, value) in environment {
        command.env(name, value);
    }
    command.stdin(Stdio::null()).output().expect("binary runs")
}

#[test]
fn explicit_existing_socket_defaults_without_teardown() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let guard = runtime.block_on(async {
        let guard = TestServer::builder().start().await.expect("tmux starts");
        guard.server().new_session("binary").await.expect("session");
        guard
    });
    let socket = guard.socket_path().to_str().expect("UTF-8 socket");
    let mut process = Process::start(&["--socket", socket], &[]);
    let names = process.tool_names();
    let listed = process.request(
        "tools/call",
        &json!({"name": "list_sessions", "arguments": {}}),
    );

    assert_eq!(names.len(), 43);
    assert!(!names.iter().any(|name| name == "kill_session"));
    assert_eq!(
        listed["result"]["structuredContent"]["sessions"][0]["name"],
        "binary",
    );
    let logged = process.finish();
    assert!(logged.contains("operator-selected socket"), "{logged}");
    runtime.block_on(async { guard.shutdown().await.expect("tmux stops") });
}

#[test]
fn explicit_toolsets_reach_the_running_process() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let guard =
        runtime.block_on(async { TestServer::builder().start().await.expect("tmux starts") });
    let socket = guard.socket_path().to_str().expect("UTF-8 socket");
    let mut inspect = Process::start(&["--socket", socket], &[("LIBTMUX_TOOLSETS", "inspect")]);
    let inspect_names = inspect.tool_names();
    inspect.finish();
    let mut all = Process::start(
        &["--socket", socket],
        &[("LIBTMUX_TOOLSETS", "inspect,manage,execute,teardown")],
    );
    let all_names = all.tool_names();
    all.finish();

    assert_eq!(inspect_names.len(), 18);
    assert_eq!(all_names.len(), 47);
    assert!(!inspect_names.contains(&"kill_session".to_owned()));
    assert!(all_names.contains(&"kill_session".to_owned()));
    runtime.block_on(async { guard.shutdown().await.expect("tmux stops") });
}

#[test]
fn exclusions_win_over_every_inclusion_path() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let guard =
        runtime.block_on(async { TestServer::builder().start().await.expect("tmux starts") });
    let socket = guard.socket_path().to_str().expect("UTF-8 socket");
    let mut process = Process::start(
        &["--socket", socket],
        &[
            ("LIBTMUX_TOOLSETS", ""),
            ("LIBTMUX_TOOLS", "kill_session"),
            ("LIBTMUX_EXCLUDE_TOOLS", "kill_session"),
        ],
    );

    assert!(process.tool_names().is_empty());
    process.finish();
    runtime.block_on(async { guard.shutdown().await.expect("tmux stops") });
}

#[test]
fn retired_safety_settings_are_fatal_migration_errors() {
    for name in ["LIBTMUX_SAFETY", "TMUX_MCP_SAFETY"] {
        let output = failed_start(&[(name, "readonly")]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{name}");
        assert!(stderr.contains(name), "{stderr}");
        assert!(stderr.contains("LIBTMUX_TOOLSETS"), "{stderr}");
    }
}

#[test]
fn malformed_or_unknown_selection_is_fatal() {
    for value in ["inspect,", "read-only"] {
        let output = failed_start(&[("LIBTMUX_TOOLSETS", value)]);
        assert!(!output.status.success(), "{value}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("LIBTMUX_TOOLSETS") || stderr.contains("toolset"));
    }
}

#[test]
fn configured_tmux_file_must_be_absolute() {
    let output = failed_start(&[("LIBTMUX_TMUX_CONFIG", "relative.conf")]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("LIBTMUX_TMUX_CONFIG"), "{stderr}");
    assert!(stderr.contains("absolute"), "{stderr}");
}

#[test]
fn stale_default_socket_path_is_replaced_before_claiming_a_live_daemon() {
    let root = PathBuf::from("/tmp/libtmux-rs-test")
        .join(format!("mcp-provenance-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("fixture root");
    let uid = std::fs::metadata(&root).expect("fixture metadata").uid();
    let socket_dir = root.join(format!("tmux-{uid}"));
    std::fs::create_dir(&socket_dir).expect("socket directory");
    std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o700))
        .expect("safe socket-directory permissions");
    let socket = socket_dir.join("libtmux-mcp");
    std::fs::write(&socket, b"stale, not a daemon").expect("stale socket fixture");

    let mut process = Process::start(
        &[],
        &[("TMUX_TMPDIR", root.to_str().expect("UTF-8 fixture path"))],
    );
    let response = process.request("resources/read", &json!({"uri": "tmux://capabilities"}));
    let text = response["result"]["contents"][0]["text"]
        .as_str()
        .expect("capability report text");
    let report: Value = serde_json::from_str(text).expect("capability report JSON");

    process.finish();
    std::fs::remove_file(&socket).expect("socket link cleanup");
    std::fs::remove_dir_all(&root).expect("fixture cleanup");

    assert_eq!(report["socket"]["serverState"], "created");
    assert_eq!(report["socket"]["configurationProvenance"], "minimal");
    assert_eq!(report["toolCount"], 47);
}

#[test]
fn default_startup_reports_dedicated_minimal_socket_provenance() {
    let root = PathBuf::from("/tmp/libtmux-rs-test")
        .join(format!("mcp-capabilities-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("fixture root");
    let mut process = Process::start(
        &[],
        &[("TMUX_TMPDIR", root.to_str().expect("UTF-8 fixture path"))],
    );

    let response = process.request("resources/read", &json!({"uri": "tmux://capabilities"}));
    let text = response["result"]["contents"][0]["text"]
        .as_str()
        .expect("capability report text");
    let report: Value = serde_json::from_str(text).expect("capability report JSON");

    assert_eq!(report["socket"]["selector"], "name:libtmux-mcp");
    assert_eq!(report["socket"]["selectionProvenance"], "default-dedicated");
    assert_eq!(report["socket"]["serverState"], "created");
    assert_eq!(report["socket"]["configurationProvenance"], "minimal");
    assert_eq!(report["connection"]["socketSelector"], "name:libtmux-mcp");
    assert_eq!(
        report["connection"]["socketProvenance"],
        "default-dedicated"
    );
    assert_eq!(report["connection"]["serverState"], "created");
    assert_eq!(report["connection"]["configurationProvenance"], "minimal");
    assert!(report["connection"]["resolvedSocketPath"].is_string());
    assert!(
        report["connection"]["attachCommand"]
            .as_str()
            .is_some_and(|command| command.contains(" -N -S ") && command.ends_with(" attach"))
    );
    assert_eq!(report["boundary"]["oneSocketPerProcess"], true);
    assert_eq!(report["boundary"]["perCallSocketSelection"], false);
    assert_eq!(report["boundary"]["hostCommandExecution"], false);
    assert_eq!(report["boundary"]["dynamicResources"], false);
    assert_eq!(report["toolCount"], 47);

    process.finish();
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn only_the_process_whose_config_marker_loaded_claims_minimal_provenance() {
    let root = PathBuf::from("/tmp/libtmux-rs-test")
        .join(format!("mcp-launch-owner-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("fixture root");
    let environment = [("TMUX_TMPDIR", root.to_str().expect("UTF-8 fixture path"))];
    let mut owner = Process::start(&[], &environment);
    let owner_response = owner.request("resources/read", &json!({"uri": "tmux://capabilities"}));
    let owner_report: Value = serde_json::from_str(
        owner_response["result"]["contents"][0]["text"]
            .as_str()
            .expect("owner capability report"),
    )
    .expect("owner report JSON");

    let mut follower = Process::start(&[], &environment);
    let follower_response =
        follower.request("resources/read", &json!({"uri": "tmux://capabilities"}));
    let follower_report: Value = serde_json::from_str(
        follower_response["result"]["contents"][0]["text"]
            .as_str()
            .expect("follower capability report"),
    )
    .expect("follower report JSON");

    follower.finish();
    owner.finish();
    let socket_dir = root.join(format!(
        "tmux-{}",
        std::fs::metadata(&root).expect("fixture metadata").uid()
    ));
    let socket = socket_dir.join("libtmux-mcp");
    if socket.exists() {
        std::fs::remove_file(socket).expect("socket cleanup");
    }
    std::fs::remove_dir_all(root).expect("fixture cleanup");

    assert_eq!(owner_report["socket"]["configurationProvenance"], "minimal");
    assert_eq!(owner_report["toolCount"], 47);
    assert_eq!(follower_report["socket"]["serverState"], "existing");
    assert_eq!(
        follower_report["socket"]["configurationProvenance"],
        "unknown"
    );
    assert_eq!(follower_report["toolCount"], 43);
}

#[test]
fn default_daemon_loads_the_shipped_minimal_configuration() {
    let root = PathBuf::from("/tmp/libtmux-rs-test")
        .join(format!("mcp-minimal-config-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("fixture root");
    let mut process = Process::start(
        &[],
        &[("TMUX_TMPDIR", root.to_str().expect("UTF-8 fixture path"))],
    );

    let created = process.request(
        "tools/call",
        &json!({"name": "create_session", "arguments": {"name": "minimal-config"}}),
    );
    assert_ne!(created["result"]["isError"], true, "{created}");
    let status = process.request(
        "tools/call",
        &json!({
            "name": "show_option",
            "arguments": {"name": "status", "scope": "global-session"}
        }),
    );
    assert_eq!(status["result"]["structuredContent"]["value"], "off");
    let killed = process.request(
        "tools/call",
        &json!({"name": "kill_session", "arguments": {"session": "minimal-config"}}),
    );
    assert_ne!(killed["result"]["isError"], true, "{killed}");

    process.finish();
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn help_names_current_startup_controls_only() {
    let output = Command::new(BIN).arg("--help").output().expect("help runs");
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    for flag in ["--socket", "--socket-name"] {
        assert!(help.contains(flag), "{flag}");
    }
    for retired in ["--safety", "--confirm", "--no-confirm", "TMUX_MCP_CONFIRM"] {
        assert!(!help.contains(retired), "{retired}: {help}");
    }
}
