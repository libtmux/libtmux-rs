//! The shipped binary, as a client launches it: a process, over stdio.
//!
//! The other suites build the tool surface in-process and talk to it over an
//! in-memory duplex, which is a fair model of the protocol but not of what
//! ships. Nothing there exercises argument parsing, the socket the process
//! chooses, the tier it resolves from a flag or the environment, or whether
//! stdout stays clean enough to carry framed JSON. A server that answers
//! perfectly in a duplex and prints one stray line to stdout is broken for
//! every client, and only a real process shows it.

// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and this file has them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use libtmux::test::TestServer;
use serde_json::{Value, json};

/// The binary under test, as cargo built it for this run.
const BIN: &str = env!("CARGO_BIN_EXE_tmux-mcp");

/// A launched `tmux-mcp` process being spoken to over its stdio.
struct Process {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    seq: i64,
}

impl Process {
    /// Launch the binary against one tmux socket, at one tier.
    ///
    /// `TMUX` and `TMUX_PANE` are cleared because the suite may itself be run
    /// from inside tmux, and a caller identity inherited from the developer's
    /// terminal would make the guard's behaviour depend on who ran the tests.
    fn start(args: &[&str]) -> Self {
        let mut child = Command::new(BIN)
            .args(args)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("TMUX_MCP_SAFETY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the binary runs");
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let mut process = Self {
            child,
            stdin,
            stdout,
            seq: 0,
        };
        process.handshake();
        process
    }

    /// Send a request and read the answer to it.
    fn request(&mut self, method: &str, params: &Value) -> Value {
        self.seq += 1;
        let id = self.seq;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{message}").expect("the server is still listening");
        self.stdin.flush().expect("the request is sent");

        loop {
            let mut line = String::new();
            let read = self
                .stdout
                .read_line(&mut line)
                .expect("stdout is readable");
            assert_ne!(
                read, 0,
                "the server closed stdout while {method} was pending"
            );
            // Every line on stdout has to be one JSON-RPC message. A line that
            // does not parse is the failure this suite exists to catch, so it
            // is reported as itself rather than skipped over.
            let message: Value = serde_json::from_str(line.trim_end()).unwrap_or_else(|error| {
                panic!("stdout carried something that is not JSON-RPC: {line:?} ({error})")
            });
            if message.get("id") == Some(&json!(id)) {
                return message;
            }
        }
    }

    /// Send a request without waiting for its answer.
    fn notify_raw(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").expect("the server is still listening");
        self.stdin.flush().expect("the request is sent");
    }

    fn notify(&mut self, method: &str) {
        let message = json!({"jsonrpc": "2.0", "method": method});
        writeln!(self.stdin, "{message}").expect("the server is still listening");
        self.stdin.flush().expect("the notification is sent");
    }

    fn handshake(&mut self) -> Value {
        let answer = self.request(
            "initialize",
            &json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "binary-suite", "version": "0"},
            }),
        );
        self.notify("notifications/initialized");
        answer
    }

    fn call(&mut self, name: &str, arguments: &Value) -> Value {
        self.request("tools/call", &json!({"name": name, "arguments": arguments}))
    }

    /// Close stdin and collect what the process said to a person.
    fn finish(self) -> String {
        drop(self.stdin);
        let output = self
            .child
            .wait_with_output()
            .expect("the process exits when stdin closes");
        assert!(
            output.status.success(),
            "the process exited with {:?}",
            output.status,
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
}

async fn wait_for_prompt(pane: &libtmux::Pane) {
    for _ in 0..600 {
        let lines = pane.capture().await.expect("pane captures");
        if lines
            .iter()
            .any(|line| matches!(line.as_bytes().last(), Some(b'$' | b'#')))
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the pane never drew a prompt");
}

#[test]
fn the_binary_serves_the_socket_it_was_pointed_at() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime starts");
    let guard =
        runtime.block_on(async { TestServer::builder().start().await.expect("tmux starts") });
    let socket = guard.socket_path().to_owned();
    runtime.block_on(async {
        guard
            .server()
            .new_session("binary")
            .await
            .expect("a session is created");
    });

    let mut process = Process::start(&["--socket", socket.to_str().expect("a utf-8 socket path")]);

    // The process found the socket, not tmux's default one.
    let answer = process.call("list_sessions", &json!({}));
    let sessions = answer["result"]["structuredContent"]["sessions"]
        .as_array()
        .unwrap_or_else(|| panic!("list_sessions answered with {answer}"));
    assert!(
        sessions.iter().any(|session| session["name"] == "binary"),
        "the session created on that socket is the one reported: {answer}",
    );

    let logged = process.finish();
    assert!(
        logged.contains("tmux-mcp"),
        "the process names itself on stderr: {logged:?}",
    );

    runtime.block_on(async { guard.shutdown().await.expect("tmux fixture shuts down") });
}

#[test]
fn a_job_handle_from_a_previous_process_is_stale() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime starts");
    let guard =
        runtime.block_on(async { TestServer::builder().start().await.expect("tmux starts") });
    let pane = runtime.block_on(async {
        let session = guard
            .server()
            .new_session("job-identity")
            .await
            .expect("a session is created");
        let pane = session.panes().await.expect("panes list").remove(0);
        wait_for_prompt(&pane).await;
        pane.id().to_string()
    });
    let socket = guard
        .socket_path()
        .to_str()
        .expect("a utf-8 socket path")
        .to_owned();

    let mut first = Process::start(&["--socket", &socket]);
    let answer = first.call(
        "start_command",
        &json!({"pane": pane, "command": "printf first"}),
    );
    let old_job = answer["result"]["structuredContent"]["job"]
        .as_str()
        .unwrap_or_else(|| panic!("start_command answered with {answer}"))
        .to_owned();
    first.finish();

    let mut second = Process::start(&["--socket", &socket]);
    let answer = second.call(
        "start_command",
        &json!({"pane": pane, "command": "printf second"}),
    );
    let new_job = answer["result"]["structuredContent"]["job"]
        .as_str()
        .unwrap_or_else(|| panic!("start_command answered with {answer}"));
    let stale = second.call("job_status", &json!({"job": old_job}));

    assert_ne!(old_job, new_job, "separate processes reused a job handle");
    assert_eq!(stale["error"]["data"]["kind"], "object_gone", "{stale}");

    second.finish();
    runtime.block_on(async { guard.shutdown().await.expect("tmux fixture shuts down") });
}

/// The tool names a process launched with these arguments advertises.
fn offered(args: &[&str]) -> Vec<String> {
    let mut process = Process::start(args);
    let answer = process.request("tools/list", &json!({}));
    let names = answer["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list answered with {answer}"))
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    process.finish();
    names
}

#[test]
fn the_tier_reaches_the_running_process() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime starts");
    let guard =
        runtime.block_on(async { TestServer::builder().start().await.expect("tmux starts") });
    let socket = guard
        .socket_path()
        .to_str()
        .expect("a utf-8 path")
        .to_owned();

    let readonly = offered(&["--socket", &socket, "--safety", "readonly"]);
    let default = offered(&["--socket", &socket]);
    let destructive = offered(&["--socket", &socket, "--safety", "destructive"]);

    assert!(
        readonly.len() < default.len() && default.len() < destructive.len(),
        "each tier offers more than the last: {} < {} < {}",
        readonly.len(),
        default.len(),
        destructive.len(),
    );
    // The point of a tier is that the tools above it cannot be chosen, which
    // is a property of the listing rather than of what a call would answer.
    assert!(
        !default.iter().any(|name| name == "kill_server"),
        "the default tier withholds the tool that ends every session",
    );
    assert!(
        destructive.iter().any(|name| name == "kill_server"),
        "the destructive tier offers it",
    );
    assert!(
        readonly.iter().all(|name| !name.starts_with("kill_")),
        "the read-only tier withholds every kill tool: {readonly:?}",
    );

    runtime.block_on(async { guard.shutdown().await.expect("tmux fixture shuts down") });
}

#[test]
fn a_failure_from_a_real_process_carries_its_classification() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime starts");
    let guard =
        runtime.block_on(async { TestServer::builder().start().await.expect("tmux starts") });
    let socket = guard
        .socket_path()
        .to_str()
        .expect("a utf-8 path")
        .to_owned();

    let mut process = Process::start(&["--socket", &socket]);
    let answer = process.call("capture_pane", &json!({"pane": "%9999"}));
    let data = &answer["error"]["data"];
    assert_eq!(data["kind"], "object_gone", "{answer}");
    assert_eq!(data["stale"], true, "{answer}");
    assert_eq!(data["retryable"], false, "{answer}");
    process.finish();

    runtime.block_on(async { guard.shutdown().await.expect("tmux fixture shuts down") });
}

#[test]
fn the_instructions_reach_a_client_that_only_ever_initialises() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime starts");
    let guard =
        runtime.block_on(async { TestServer::builder().start().await.expect("tmux starts") });
    let socket = guard
        .socket_path()
        .to_str()
        .expect("a utf-8 path")
        .to_owned();

    // An agent reads these once, before it has called anything, so they are
    // the only thing steering the first call it makes.
    let mut process = Process::start(&["--socket", &socket]);
    let answer = process.handshake();
    let instructions = answer["result"]["instructions"]
        .as_str()
        .unwrap_or_else(|| panic!("initialize answered with {answer}"));
    for expected in [
        "TRIGGERS",
        "NAMES ARE NOT CONTENTS",
        "WAIT, DO NOT POLL",
        "repeating the same call unchanged is safe",
        "partial_effect",
    ] {
        assert!(
            instructions.contains(expected),
            "the instructions still steer with {expected}",
        );
    }
    process.finish();

    runtime.block_on(async { guard.shutdown().await.expect("tmux fixture shuts down") });
}

#[test]
fn the_command_line_answers_for_itself() {
    let version = Command::new(BIN)
        .arg("--version")
        .output()
        .expect("the binary runs");
    assert!(version.status.success());
    let text = String::from_utf8_lossy(&version.stdout);
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "--version reports the crate version: {text:?}",
    );

    let help = Command::new(BIN)
        .arg("--help")
        .output()
        .expect("the binary runs");
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    for flag in ["--socket", "--socket-name", "--safety"] {
        assert!(text.contains(flag), "--help documents {flag}: {text:?}");
    }

    // A misspelled tier is refused rather than quietly widening or narrowing
    // the surface, and the refusal goes to stderr so stdout stays a protocol
    // channel even when the process is about to give up.
    let misuse = Command::new(BIN)
        .args(["--safety", "destructiv"])
        .output()
        .expect("the binary runs");
    assert!(!misuse.status.success(), "a misspelled tier is refused");
    assert!(
        misuse.stdout.is_empty(),
        "nothing goes to stdout: {:?}",
        String::from_utf8_lossy(&misuse.stdout),
    );
    assert!(
        String::from_utf8_lossy(&misuse.stderr).contains("safety"),
        "the refusal says what was wrong",
    );
}

/// Count the control-mode clients this test's socket has.
///
/// Scoped by socket path, which is unique per test, so a suite running in
/// parallel cannot see another test's connections. `ps` is used because a
/// control-mode client does not appear in tmux's own `list-clients`.
fn control_clients(socket: &str) -> usize {
    let output = Command::new("ps")
        .args(["-eo", "args"])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(socket) && line.contains(" -C"))
        .count()
}

#[test]
fn a_wait_that_is_abandoned_takes_its_connection_with_it() {
    // A wait holds a control-mode connection open. If a client goes away
    // mid-wait and that connection is not reaped, a long-lived agent session
    // leaks one tmux process per abandoned wait until the machine notices.
    let runtime = tokio::runtime::Runtime::new().expect("a runtime starts");
    let guard =
        runtime.block_on(async { TestServer::builder().start().await.expect("tmux starts") });
    let socket = guard
        .socket_path()
        .to_str()
        .expect("a utf-8 path")
        .to_owned();
    runtime.block_on(async {
        guard
            .server()
            .new_session("abandoned")
            .await
            .expect("a session is created");
    });

    let mut process = Process::start(&["--socket", &socket]);
    let panes = process.call("list_panes", &json!({}));
    let pane = panes["result"]["structuredContent"]["panes"][0]["id"]
        .as_str()
        .expect("a pane id")
        .to_owned();

    assert_eq!(control_clients(&socket), 0, "nothing is connected yet");

    // Ask for something that will not arrive, and do not wait for the answer.
    process.notify_raw(&json!({
        "jsonrpc": "2.0",
        "id": 9000,
        "method": "tools/call",
        "params": {
            "name": "wait_for_text",
            "arguments": {"pane": pane, "patterns": ["never-appears"], "seconds": 120},
        },
    }));
    for _ in 0..80 {
        if control_clients(&socket) > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        control_clients(&socket),
        1,
        "the wait opened one connection"
    );

    // Cancel that one request while the server keeps running. Doing this
    // rather than closing stdin is the whole point: when the process exits,
    // its children lose their pipes and tmux reaps them regardless of what
    // this crate does, so a test that ends the process proves nothing about
    // whether a cancelled wait releases anything.
    process.notify_raw(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 9000, "reason": "the client went away"},
    }));
    for _ in 0..120 {
        if control_clients(&socket) == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        control_clients(&socket),
        0,
        "a cancelled wait kept its control-mode client while the server ran on",
    );

    // And the server is still usable afterwards, so cancelling one request
    // did not take the connection down with it.
    let after = process.call("list_panes", &json!({}));
    assert!(
        after["result"]["structuredContent"]["panes"].is_array(),
        "the server still answers after a cancellation: {after}",
    );
    process.finish();

    runtime.block_on(async { guard.shutdown().await.expect("tmux fixture shuts down") });
}

#[test]
fn tailing_many_panes_stays_within_its_connection_budget() {
    // Each tailed pane holds a control-mode connection, and the registry caps
    // how many it keeps. The cap is only worth having if evicting a tail
    // actually releases the connection: a tokio JoinHandle that is dropped
    // rather than aborted detaches, and the reader would go on holding tmux
    // open with nothing left pointing at it.
    let runtime = tokio::runtime::Runtime::new().expect("a runtime starts");
    let guard =
        runtime.block_on(async { TestServer::builder().start().await.expect("tmux starts") });
    let socket = guard
        .socket_path()
        .to_str()
        .expect("a utf-8 path")
        .to_owned();

    // One pane per window, because a window runs out of room to split long
    // before this many panes exist.
    let panes: Vec<String> = runtime.block_on(async {
        let server = guard.server();
        let session = server.new_session("budget").await.expect("a session");
        let mut ids = Vec::new();
        for index in 0..10 {
            let window = session
                .new_window(libtmux::NewWindowOptions::new(format!("w{index}")))
                .await
                .expect("a window");
            let pane = window.panes().await.expect("panes").remove(0);
            ids.push(pane.id().to_string());
        }
        ids
    });
    assert_eq!(panes.len(), 10, "the fixture built more panes than the cap");

    let mut process = Process::start(&["--socket", &socket]);
    for pane in &panes {
        let answer = process.call("capture_since", &json!({"pane": pane}));
        assert!(
            answer["result"]["structuredContent"]["cursor"].is_string(),
            "capture_since answered with a cursor for {pane}: {answer}",
        );
    }

    // Twelve panes tailed, and the registry keeps eight.
    let held = control_clients(&socket);
    assert!(
        held <= 8,
        "tailing 10 panes left {held} connections open; the cap is 8",
    );

    process.finish();
    runtime.block_on(async { guard.shutdown().await.expect("tmux fixture shuts down") });
}
