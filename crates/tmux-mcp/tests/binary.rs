//! The shipped process, including startup selection and stdio framing.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Output, Stdio};

use libtmux::test::TestServer;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_tmux-mcp");

/// A directory of the process's own for `TMUX_TMPDIR`, so a start that names
/// no socket reaches a private default socket and never the tester's.
struct PrivateRoot(PathBuf);

impl PrivateRoot {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let root = PathBuf::from("/tmp/libtmux-rs-test").join(format!(
            "mcp-private-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("private root");
        Self(root)
    }
}

impl Drop for PrivateRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Process {
    _root: PrivateRoot,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: Option<ChildStderr>,
    seq: i64,
}

impl Process {
    fn start(args: &[&str], environment: &[(&str, &str)]) -> Self {
        let root = PrivateRoot::new();
        let mut command = base_command(&root);
        command.args(args);
        for (name, value) in environment {
            command.env(name, value);
        }
        let mut child = command.spawn().expect("the binary runs");
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let stderr = child.stderr.take();
        let mut process = Self {
            _root: root,
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

/// Whether this is a checkout, which has the sandbox's `.cargo/config.toml`.
///
/// A package does not, and there nothing applies one.
fn in_checkout() -> bool {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cargo/config.toml")
        .is_file()
}

/// Refuse to start a child when a checkout's sandbox did not reach this process,
/// naming the variable at fault and never its value.
fn require_sandbox() {
    if !in_checkout() {
        return;
    }
    for name in ["TMUX", "TMUX_PANE", "LIBTMUX_SOCKET", "LIBTMUX_SOCKET_PATH"] {
        assert!(
            std::env::var_os(name).is_none(),
            "{name} is set; run this from the checkout, where Cargo sandboxes it",
        );
    }
    assert!(
        std::env::var_os("LIBTMUX_TEST_SANDBOX").is_some(),
        "LIBTMUX_TEST_SANDBOX is unset; run this from the checkout, where Cargo sandboxes it",
    );
}

#[test]
fn a_pane_environment_is_refused_and_no_value_is_printed() {
    if !in_checkout() {
        return;
    }
    let rerun = |environment: Option<&[(&str, &str)]>| {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command.args([
            "--exact",
            "configured_tmux_file_must_be_absolute",
            "--nocapture",
            "--test-threads=1",
        ]);
        if let Some(environment) = environment {
            command.env_clear().envs(environment.iter().copied());
        }
        let output = command.output().expect("the test executable runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        (output.status.success(), text)
    };

    let (passes, text) = rerun(None);
    assert!(
        passes && text.contains("1 passed"),
        "the control must pass:\n{text}",
    );

    let (passes, text) = rerun(Some(&[
        ("TMUX", "/nonexistent/decoy.sock,1,0"),
        ("TMUX_PANE", "%9"),
    ]));
    assert!(!passes && text.contains("1 failed"), "a pane got through");
    assert!(text.contains("TMUX is set"), "{text}");
    assert!(!text.contains("decoy"), "a failure printed a value");
}

fn base_command(root: &PrivateRoot) -> Command {
    require_sandbox();
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
        .env("TMUX_TMPDIR", &root.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn failed_start(environment: &[(&str, &str)]) -> Output {
    let root = PrivateRoot::new();
    let mut command = base_command(&root);
    for (name, value) in environment {
        command.env(name, value);
    }
    command.stdin(Stdio::null()).output().expect("binary runs")
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
    let mirrored = version.has_behavior(&libtmux::since::MIRRORED_LAYOUTS);
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

/// What `tmux://capabilities` says about the socket, read from a running
/// process.
fn capability_report(process: &mut Process) -> Value {
    let response = process.request("resources/read", &json!({"uri": "tmux://capabilities"}));
    let text = response["result"]["contents"][0]["text"]
        .as_str()
        .expect("capability report text");
    serde_json::from_str(text).expect("capability report JSON")
}

#[test]
fn default_startup_passes_tmux_no_socket_and_starts_no_daemon() {
    let root = PathBuf::from("/tmp/libtmux-rs-test")
        .join(format!("mcp-capabilities-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("fixture root");
    let canonical = root.canonicalize().expect("fixture root resolves");
    let uid = std::fs::metadata(&root).expect("fixture metadata").uid();
    let mut process = Process::start(
        &[],
        &[("TMUX_TMPDIR", root.to_str().expect("UTF-8 fixture path"))],
    );

    let report = capability_report(&mut process);
    let selector = format!("path:{}/tmux-{uid}/default", canonical.display());

    assert_eq!(report["socket"]["selector"], selector);
    assert_eq!(report["socket"]["selectionProvenance"], "operator-current");
    assert_eq!(report["socket"]["serverState"], "absent");
    assert_eq!(report["socket"]["configurationProvenance"], "unknown");
    assert_eq!(report["connection"]["socketSelector"], selector);
    assert_eq!(report["connection"]["socketProvenance"], "operator-current");
    assert_eq!(report["connection"]["serverState"], "absent");
    assert!(
        report["connection"]["attachCommand"]
            .as_str()
            .is_some_and(|command| command.contains(" -N -S ") && command.ends_with(" attach"))
    );
    assert_eq!(report["boundary"]["oneSocketPerProcess"], true);
    assert_eq!(report["boundary"]["perCallSocketSelection"], false);
    assert_eq!(report["boundary"]["hostCommandExecution"], false);
    assert_eq!(report["boundary"]["dynamicResources"], false);
    assert_eq!(report["toolCount"], 40, "teardown is not in the default");

    process.finish();
    // tmux makes the socket directory for any client, so only a socket in it
    // shows a daemon.
    let started = std::fs::read_dir(root.join(format!("tmux-{uid}")))
        .is_ok_and(|mut entries| entries.next().is_some());
    std::fs::remove_dir_all(root).expect("fixture cleanup");
    assert!(!started, "the process started a daemon of its own");
}

/// Start a daemon on a socket name in a private root, the way a user's own
/// would be, and keep it findable by a bare `tmux -L`.
struct NamedDaemon {
    root: PathBuf,
    name: &'static str,
}

impl NamedDaemon {
    fn start(label: &str, name: &'static str, session: &str) -> Self {
        let root = PathBuf::from("/tmp/libtmux-rs-test")
            .join(format!("mcp-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("fixture root");
        let daemon = Self { root, name };
        let started = daemon
            .tmux(&["new-session", "-d", "-s", session])
            .status()
            .expect("tmux runs");
        assert!(started.success());
        daemon
    }

    fn tmux(&self, arguments: &[&str]) -> Command {
        let mut command =
            Command::new(std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or("tmux".into()));
        command
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env("TMUX_TMPDIR", &self.root)
            .args(["-f", "/dev/null", "-L", self.name])
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn root(&self) -> &str {
        self.root.to_str().expect("UTF-8 fixture path")
    }
}

impl Drop for NamedDaemon {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]).status();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn session_names(process: &mut Process) -> Vec<String> {
    let listed = process.request(
        "tools/call",
        &json!({"name": "list_sessions", "arguments": {}}),
    );
    listed["result"]["structuredContent"]["sessions"]
        .as_array()
        .expect("session list")
        .iter()
        .map(|session| session["name"].as_str().expect("session name").to_owned())
        .collect()
}

#[test]
fn the_socket_name_variable_selects_the_daemon_when_no_flag_is_given() {
    let daemon = NamedDaemon::start("env-name", "chosen-by-env", "by-name");
    let mut process = Process::start(
        &[],
        &[
            ("TMUX_TMPDIR", daemon.root()),
            ("LIBTMUX_SOCKET", daemon.name),
        ],
    );

    assert_eq!(session_names(&mut process), ["by-name"]);
    let report = capability_report(&mut process);
    assert_eq!(report["socket"]["selector"], "name:chosen-by-env");
    assert_eq!(report["socket"]["serverState"], "existing");
    process.finish();
}

#[test]
fn the_socket_path_variable_selects_the_daemon_and_wins_over_the_name() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let guard = runtime.block_on(async {
        let guard = TestServer::new().await.expect("tmux starts");
        guard.session("by-path").await.expect("session starts");
        guard
    });
    let socket = guard.socket_path().to_str().expect("UTF-8 socket");
    let mut process = Process::start(
        &[],
        &[
            ("LIBTMUX_SOCKET_PATH", socket),
            ("LIBTMUX_SOCKET", "name-that-loses"),
        ],
    );

    assert_eq!(session_names(&mut process), ["by-path"]);
    let report = capability_report(&mut process);
    assert_eq!(report["socket"]["selector"], format!("path:{socket}"));
    process.finish();
    runtime.block_on(async { guard.shutdown().await.expect("tmux stops") });
}

#[test]
fn a_flag_leaves_both_socket_variables_unread() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let guard = runtime.block_on(async {
        let guard = TestServer::new().await.expect("tmux starts");
        guard.session("by-flag").await.expect("session starts");
        guard
    });
    let socket = guard.socket_path().to_str().expect("UTF-8 socket");
    let mut process = Process::start(
        &["--socket", socket],
        &[
            (
                "LIBTMUX_SOCKET_PATH",
                "/tmp/libtmux-rs-test/never-read.sock",
            ),
            ("LIBTMUX_SOCKET", "never-read"),
        ],
    );

    assert_eq!(session_names(&mut process), ["by-flag"]);
    process.finish();
    runtime.block_on(async { guard.shutdown().await.expect("tmux stops") });
}

#[test]
fn inside_tmux_with_no_socket_chosen_the_server_tmux_names_is_driven() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (guard, pane) = runtime.block_on(async {
        let guard = TestServer::new().await.expect("tmux starts");
        let session = guard.session("inside").await.expect("session starts");
        let pane = session
            .active_window()
            .await
            .expect("window")
            .expect("active window")
            .active_pane()
            .await
            .expect("pane")
            .expect("active pane")
            .id()
            .to_string();
        (guard, pane)
    });
    let context = format!("{},{},0", guard.socket_path().display(), guard.daemon_pid());
    let mut process = Process::start(&[], &[("TMUX", &context), ("TMUX_PANE", &pane)]);

    assert_eq!(session_names(&mut process), ["inside"]);
    process.finish();
    runtime.block_on(async { guard.shutdown().await.expect("tmux stops") });
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
