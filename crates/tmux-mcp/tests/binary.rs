//! The shipped process, including startup selection and stdio framing.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Output, Stdio};
use std::time::Duration;

use libtmux::test::TestServer;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_tmux-mcp");

struct Process {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: Option<ChildStderr>,
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
        let stderr = child.stderr.take();
        let mut process = Self {
            child,
            stdin,
            stdout,
            stderr,
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
        let id = json!(self.seq);
        self.request_with_id(&id, method, params).0
    }

    fn request_with_id(&mut self, id: &Value, method: &str, params: &Value) -> (Value, usize) {
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .expect("request writes");
        self.stdin.flush().expect("request flushes");
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).expect("stdout reads");
            // A server that exits during startup says why on stderr, and this
            // used to throw that away -- leaving a lane the author cannot
            // reach reporting only that it closed.
            assert!(
                read != 0,
                "server closed while {method} was pending; stderr: {}",
                self.explanation()
            );
            let message: Value = serde_json::from_str(line.trim_end()).unwrap_or_else(|error| {
                panic!("stdout carried non-JSON-RPC data: {line:?} ({error})")
            });
            if message.get("id") == Some(id)
                || (message.get("error").is_some() && message.get("id").is_none_or(Value::is_null))
            {
                return (message, line.len());
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

    /// Drain whatever the process explained before it stopped answering.
    ///
    /// Reading blocks until the process closes stderr, so every caller must
    /// have stopped expecting it to answer first.
    fn explanation(&mut self) -> String {
        let Some(mut stderr) = self.stderr.take() else {
            return String::from("(already read)");
        };
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut text);
        text
    }

    fn finish(self) -> String {
        let Self {
            mut child,
            stdin,
            stderr,
            ..
        } = self;
        // stdin first: the process exits when it closes, and stderr does not
        // reach end of file until it does.
        drop(stdin);
        let mut text = String::new();
        if let Some(mut stderr) = stderr {
            let _ = std::io::Read::read_to_string(&mut stderr, &mut text);
        }
        let status = child.wait().expect("the process exits when stdin closes");
        assert!(status.success(), "{status:?}; stderr: {text}");
        text
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
        .env_remove("LIBTMUX_ENVIRONMENT_VALUES")
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

fn daemon_is_alive(socket: &Path) -> bool {
    Command::new("tmux")
        .arg("-S")
        .arg(socket)
        .arg("display-message")
        .arg("-p")
        .arg("alive")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn daemon_stops(socket: &Path) -> bool {
    for _ in 0..200 {
        if !daemon_is_alive(socket) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

fn stop_daemon(socket: &Path) {
    let _ = Command::new("tmux")
        .arg("-S")
        .arg(socket)
        .arg("kill-server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn tool_error(response: &Value) -> Value {
    assert_eq!(response["result"]["isError"], true, "{response}");
    serde_json::from_str(
        response["result"]["content"][0]["text"]
            .as_str()
            .expect("tool error text"),
    )
    .expect("structured tool error")
}

fn layout_process(guard: &TestServer) -> Process {
    let executable = guard
        .server()
        .resolved_tmux_executable()
        .expect("fixture executable resolves");
    let mut paths = vec![
        executable
            .parent()
            .expect("executable directory")
            .to_owned(),
    ];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths).expect("executable search path");
    Process::start(
        &[
            "--socket",
            guard.socket_path().to_str().expect("UTF-8 socket"),
        ],
        &[("PATH", path.to_str().expect("UTF-8 executable search path"))],
    )
}

async fn layout_window() -> (TestServer, libtmux::Window) {
    let guard = TestServer::new().await.expect("tmux starts");
    let session = guard.session("keeper").await.expect("session starts");
    let window = session.windows().await.expect("windows").remove(0);
    window
        .split(libtmux::SplitDirection::Below)
        .await
        .expect("split");
    (guard, window)
}

#[test]
fn mcp_layout_invalid_syntax_precedes_target_lookup() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let guard = runtime.block_on(async {
        let guard = TestServer::new().await.expect("tmux starts");
        guard.session("keeper").await.expect("session starts");
        guard
    });
    let mut process = layout_process(&guard);
    let response = process.request(
        "tools/call",
        &json!({
            "name": "select_layout",
            "arguments": {"window": "@999999", "layout": "32d2,80x24,0,0{}"},
        }),
    );
    process.finish();
    runtime.block_on(guard.shutdown()).expect("tmux stops");
    let error = tool_error(&response);
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(
        error["data"],
        json!({"kind": "invalid_input", "retryable": false, "stale": false}),
        "{response}",
    );
}

#[test]
fn mcp_layout_returns_the_applied_saved_layout() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (guard, mut window) = runtime.block_on(layout_window());
    let mut process = layout_process(&guard);
    let response = process.request(
        "tools/call",
        &json!({
            "name": "select_layout",
            "arguments": {"window": window.id().to_string(), "layout": "even-h"},
        }),
    );
    process.finish();
    runtime.block_on(window.refresh()).expect("window survives");
    runtime.block_on(guard.shutdown()).expect("tmux stops");
    assert_eq!(
        response["result"]["structuredContent"],
        json!({"window": window.id().to_string(), "layout": window.layout().to_string_lossy()}),
        "{response}",
    );
}

#[test]
fn real_tmux_compat_mcp_layout_wire_preserves_keeper_and_native_errors() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (guard, mut window) = runtime.block_on(layout_window());
    let version = runtime.block_on(async {
        let version = guard
            .server()
            .format(None, "#{version}")
            .await
            .expect("version");
        libtmux::TmuxVersion::parse_output(
            format!("tmux {}\n", version.to_string_lossy()).as_bytes(),
        )
        .expect("native version")
    });
    eprintln!("MCP layout wire daemon: {}", version.raw());
    let mirrored = version.meets(&libtmux::since::MIRRORED_LAYOUTS);
    let cases = [
        ("not-a-layout", Some("invalid_input")),
        ("-E", Some("invalid_input")),
        ("32d2,80x24,0,0{}", Some("invalid_input")),
        (
            "4a17,80x24,0,0{39x24,0,0,0,40x24,40,0[]}",
            Some("invalid_input"),
        ),
        ("ffff,80x24,0,0,0", Some("invalid_input")),
        ("even", Some("invalid_input")),
        ("79f5,80x24,0,0{39x23,0,0,0,40x24,40,0,1}", Some("refused")),
        ("main-h", mirrored.then_some("invalid_input")),
        (
            "main-horizontal-mirrored",
            (!mirrored).then_some("unsupported_version"),
        ),
        ("main-horizontal", None),
        ("even-h", None),
        ("t", None),
        ("8A08,1x1,0,0{39x24,0,0,0,40x24,40,0,1}", None),
    ];
    let mut process = layout_process(&guard);
    let mut observed = Vec::new();
    for (layout, error) in cases {
        runtime.block_on(window.refresh()).expect("window remains");
        let before = window.layout().to_owned();
        let response = process.request(
            "tools/call",
            &json!({
                "name": "select_layout",
                "arguments": {"window": window.id().to_string(), "layout": layout},
            }),
        );
        runtime
            .block_on(window.refresh())
            .expect("window survives layout");
        observed.push((layout, error, response, before, window.layout().to_owned()));
    }
    let missing = process.request(
        "tools/call",
        &json!({
            "name": "select_layout",
            "arguments": {"window": "@999999", "layout": "tiled"},
        }),
    );
    process.finish();
    let (sessions, panes, pid) = runtime.block_on(async {
        (
            guard.server().sessions().await.expect("keeper sessions"),
            window.panes().await.expect("keeper panes"),
            guard
                .server()
                .format(None, "#{pid}")
                .await
                .expect("daemon PID"),
        )
    });
    let expected_pid = guard.daemon_pid();
    runtime.block_on(guard.shutdown()).expect("tmux stops");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id(), window.session_id());
    assert_eq!(panes.len(), 2);
    assert_eq!(pid.to_string_lossy(), expected_pid.to_string());
    assert_eq!(
        tool_error(&missing)["data"]["kind"],
        "object_gone",
        "{missing}"
    );
    for (layout, error, response, before, after) in observed {
        if let Some(kind) = error {
            let error = tool_error(&response);
            assert_eq!(error["data"]["kind"], kind, "{layout}: {response}");
            assert_eq!(error["data"]["retryable"], false, "{response}");
            assert_eq!(error["data"]["stale"], false, "{response}");
            assert_eq!(before, after, "{layout} changed a refused layout");
        } else {
            assert_eq!(
                response["result"]["structuredContent"],
                json!({
                    "window": window.id().to_string(), "layout": after.to_string_lossy(),
                }),
                "{layout}: {response}"
            );
        }
    }
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

    assert_eq!(names.len(), 40);
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
    assert_eq!(all_names.len(), 45);
    assert!(!inspect_names.contains(&"kill_session".to_owned()));
    assert!(all_names.contains(&"kill_session".to_owned()));
    runtime.block_on(async { guard.shutdown().await.expect("tmux stops") });
}

#[test]
fn oversized_request_id_fails_before_tool_dispatch() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let guard =
        runtime.block_on(async { TestServer::builder().start().await.expect("tmux starts") });
    let socket = guard.socket_path().to_str().expect("UTF-8 socket");
    let mut process = Process::start(&["--socket", socket], &[]);
    let accepted_id = json!("i".repeat(512 * 1024 - 2));
    let (accepted, _) = process.request_with_id(&accepted_id, "tools/list", &json!({}));
    assert!(accepted["result"]["tools"].is_array());

    let (response, response_bytes) = process.request_with_id(
        &json!("i".repeat(1_000_000)),
        "tools/call",
        &json!({"name": "create_session", "arguments": {"name": "must-not-exist"}}),
    );
    let listed = process.request(
        "tools/call",
        &json!({"name": "list_sessions", "arguments": {}}),
    );
    let sessions = listed["result"]["structuredContent"]["sessions"]
        .as_array()
        .expect("session list");

    assert!(
        sessions
            .iter()
            .all(|session| session["name"] != "must-not-exist"),
        "oversized request reached create_session",
    );
    assert_eq!(response["error"]["code"], -32600);
    assert!(response.get("id").is_none() || response["id"].is_null());
    assert!(response_bytes <= 1_000_000, "{response_bytes} bytes");
    process.finish();
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
    assert_eq!(report["toolCount"], 45);
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
    assert_eq!(report["toolCount"], 45);

    process.finish();
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn default_owner_stops_its_dedicated_daemon_when_stdio_closes() {
    let root = PathBuf::from("/tmp/libtmux-rs-test")
        .join(format!("mcp-owner-shutdown-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("fixture root");
    let socket = root.join(format!(
        "tmux-{}/libtmux-mcp",
        std::fs::metadata(&root).expect("fixture metadata").uid()
    ));
    let process = Process::start(
        &[],
        &[("TMUX_TMPDIR", root.to_str().expect("UTF-8 fixture path"))],
    );

    process.finish();
    let stopped = daemon_stops(&socket);
    if !stopped {
        stop_daemon(&socket);
    }
    std::fs::remove_dir_all(root).expect("fixture cleanup");

    assert!(
        stopped,
        "the process left its dedicated tmux daemon running"
    );
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
    assert_eq!(owner_report["toolCount"], 45);
    assert_eq!(follower_report["socket"]["serverState"], "existing");
    assert_eq!(
        follower_report["socket"]["configurationProvenance"],
        "unknown"
    );
    assert_eq!(follower_report["toolCount"], 40);
}

/// Two clients on the default socket share one daemon, so the one that
/// started it must not stop it while the other still answers from it.
#[test]
fn the_owner_leaves_a_shared_dedicated_daemon_running() {
    let root = PathBuf::from("/tmp/libtmux-rs-test")
        .join(format!("mcp-shared-owner-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("fixture root");
    let socket = root.join(format!(
        "tmux-{}/libtmux-mcp",
        std::fs::metadata(&root).expect("fixture metadata").uid()
    ));
    let environment = [("TMUX_TMPDIR", root.to_str().expect("UTF-8 fixture path"))];
    let mut owner = Process::start(&[], &environment);
    let created = owner.request(
        "tools/call",
        &json!({"name": "create_session", "arguments": {"name": "shared"}}),
    );
    let mut follower = Process::start(&[], &environment);

    let owner_log = owner.finish();
    let listed = follower.request(
        "tools/call",
        &json!({"name": "list_sessions", "arguments": {}}),
    );
    follower.finish();
    let alive_after_both = daemon_is_alive(&socket);
    stop_daemon(&socket);
    std::fs::remove_dir_all(&root).expect("fixture cleanup");

    assert_ne!(created["result"]["isError"], true, "{created}");
    assert_eq!(
        listed["result"]["structuredContent"]["sessions"][0]["name"], "shared",
        "the follower lost its daemon when the owner exited: {listed}"
    );
    assert!(owner_log.contains("leaving"), "{owner_log}");
    assert!(
        alive_after_both,
        "a follower stopped a daemon it did not start"
    );
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

/// Errors rmcp raises before a tool runs carry the same `kind` as the rest,
/// and a withheld tool says who can offer it.
#[test]
fn argument_and_routing_errors_are_typed() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let guard =
        runtime.block_on(async { TestServer::builder().start().await.expect("tmux starts") });
    let socket = guard.socket_path().to_str().expect("UTF-8 socket");
    let mut process = Process::start(&["--socket", socket], &[]);

    let missing = process.request("tools/call", &json!({"name": "send_keys", "arguments": {}}));
    let batched = process.request(
        "tools/call",
        &json!({
            "name": "call_read_tools_batch",
            "arguments": {"operations": [{"tool": "capture_pane", "arguments": {}}]}
        }),
    );
    let withheld = process.request(
        "tools/call",
        &json!({"name": "kill_session", "arguments": {"session": "x"}}),
    );
    let unknown = process.request(
        "tools/call",
        &json!({"name": "no_such_tool", "arguments": {}}),
    );
    process.finish();
    runtime.block_on(async { guard.shutdown().await.expect("tmux stops") });

    let body: Value = serde_json::from_str(
        missing["result"]["content"][0]["text"]
            .as_str()
            .expect("failed result text"),
    )
    .unwrap_or_else(|error| panic!("untyped argument error {missing}: {error}"));
    assert_eq!(missing["result"]["isError"], true, "{missing}");
    assert_eq!(body["data"]["kind"], "invalid_input", "{missing}");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|text| text.contains("pane")),
        "{missing}"
    );
    let nested =
        batched["result"]["structuredContent"]["results"][0]["result"]["content"][0]["text"]
            .as_str()
            .expect("nested failed result text");
    assert!(nested.contains("invalid_input"), "{batched}");

    assert_eq!(
        withheld["error"]["data"]["kind"], "invalid_input",
        "{withheld}"
    );
    assert!(
        withheld["error"]["message"]
            .as_str()
            .is_some_and(|text| text.contains("LIBTMUX_TOOLSETS")),
        "{withheld}"
    );
    assert_eq!(
        unknown["error"]["data"]["kind"], "invalid_input",
        "{unknown}"
    );
    assert!(
        unknown["error"]["message"]
            .as_str()
            .is_some_and(|text| !text.contains("LIBTMUX_TOOLSETS")),
        "{unknown}"
    );
}

/// `TMUX= TMUX_PANE=` is how a shell un-nests tmux; it means detached. A
/// context that is set and malformed says so once, at startup.
#[test]
fn empty_caller_variables_are_detached_and_malformed_ones_are_logged() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (guard, pane) = runtime.block_on(async {
        let guard = TestServer::builder().start().await.expect("tmux starts");
        let pane = guard
            .server()
            .new_session("caller")
            .await
            .expect("session starts")
            .panes()
            .await
            .expect("panes list")
            .remove(0)
            .id()
            .to_string();
        (guard, pane)
    });
    let socket = guard.socket_path().to_str().expect("UTF-8 socket");
    let typed = json!({"name": "send_keys", "arguments": {"pane": pane, "text": "x"}});

    let mut empty = Process::start(&["--socket", socket], &[("TMUX", ""), ("TMUX_PANE", "")]);
    let sent = empty.request("tools/call", &typed);
    let empty_log = empty.finish();
    assert_ne!(sent["result"]["isError"], true, "{sent}");
    assert!(!empty_log.contains("TMUX_PANE"), "{empty_log}");

    let mut malformed = Process::start(
        &["--socket", socket],
        &[("TMUX", "not-a-context"), ("TMUX_PANE", "%0")],
    );
    let refused = malformed.request("tools/call", &typed);
    let malformed_log = malformed.finish();
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert_eq!(
        malformed_log.matches("TMUX and TMUX_PANE").count(),
        1,
        "{malformed_log}"
    );
    runtime.block_on(async { guard.shutdown().await.expect("tmux stops") });
}

/// Measured over JSON-RPC, because a transcript is where a value leaks to.
///
/// The fixture daemon inherits this test's environment, so no failure message
/// prints a response.
#[test]
fn environment_values_stay_off_the_wire_unless_allowed() {
    const SECRET: &str = "planted-secret-2d7e";
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let guard = runtime.block_on(async {
        let guard = TestServer::builder().start().await.expect("tmux starts");
        let server = guard.server();
        server.new_session("wire").await.expect("session starts");
        server
            .set_environment("PLANTED_API_KEY", SECRET)
            .await
            .expect("secret is planted");
        server
            .set_environment("PLANTED_ALLOWED", "allowed-value")
            .await
            .expect("allowed value is planted");
        guard
    });
    let socket = guard.socket_path().to_str().expect("UTF-8 socket");
    let calls = [
        json!({"name": "show_environment", "arguments": {}}),
        json!({"name": "get_tmux_variables", "arguments": {"names": ["PLANTED_API_KEY"]}}),
        json!({
            "name": "call_read_tools_batch",
            "arguments": {"operations": [
                {"tool": "show_environment", "arguments": {}},
                {"tool": "get_tmux_variables", "arguments": {"names": ["PLANTED_API_KEY"]}}
            ]}
        }),
    ];

    let mut default = Process::start(&["--socket", socket], &[]);
    for call in &calls {
        let response = default.request("tools/call", call).to_string();
        assert!(
            !response.contains(SECRET),
            "{} put a withheld value on the wire",
            call["name"]
        );
    }
    default.finish();

    let mut allowed = Process::start(
        &["--socket", socket],
        &[("LIBTMUX_ENVIRONMENT_VALUES", "PLANTED_ALLOWED")],
    );
    for call in &calls {
        let response = allowed.request("tools/call", call).to_string();
        assert!(
            !response.contains(SECRET),
            "{} put a withheld value on the wire",
            call["name"]
        );
    }
    let listing = allowed.request("tools/call", &calls[0]).to_string();
    assert!(
        listing.contains("allowed-value"),
        "an allowed value is returned"
    );
    allowed.finish();

    let refused = failed_start(&[("LIBTMUX_ENVIRONMENT_VALUES", "A=B")]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success());
    assert!(stderr.contains("LIBTMUX_ENVIRONMENT_VALUES"), "{stderr}");
    runtime.block_on(async { guard.shutdown().await.expect("tmux stops") });
}

#[test]
fn help_names_current_startup_controls_only() {
    let output = Command::new(BIN).arg("--help").output().expect("help runs");
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    for flag in [
        "--socket",
        "--socket-name",
        "LIBTMUX_SOCKET_PATH",
        "LIBTMUX_SOCKET ",
        "LIBTMUX_TMUX_CONFIG",
        "LIBTMUX_TOOLSETS",
        "LIBTMUX_TOOLS ",
        "LIBTMUX_EXCLUDE_TOOLS",
        "LIBTMUX_ENVIRONMENT_VALUES",
    ] {
        assert!(help.contains(flag), "{flag}");
    }
    for retired in ["--safety", "--confirm", "--no-confirm", "TMUX_MCP_CONFIRM"] {
        assert!(!help.contains(retired), "{retired}: {help}");
    }
}
