//! Live checks for the retained MCP tool families.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::time::Duration;

use libtmux::test::TestServer;
use libtmux::{Command, NewSessionOptions, NewWindowOptions, Server, SplitDirection, SplitOptions};
use serde_json::Value;
use tmux_mcp::{CallerIdentity, TmuxTools};
use tokio_util::sync::CancellationToken;

mod support;

use support::{args, bare_tools, call_tool, json, prompt_ready};

struct RawServerFiles {
    directory: PathBuf,
    bootstrap_executable: PathBuf,
    executable: PathBuf,
    socket: PathBuf,
    config: PathBuf,
    owner: PathBuf,
    running: bool,
    cleaned: bool,
}

impl RawServerFiles {
    fn create(executable_name: &[u8], socket_name: &[u8]) -> Self {
        let actual = Server::new()
            .expect("default server config")
            .resolved_tmux_executable()
            .expect("configured tmux resolves");
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce).expect("fixture nonce");
        let root = PathBuf::from("/tmp/libtmux-rs-test");
        std::fs::create_dir_all(&root).expect("owned fixture root");
        let directory = root.join(format!("raw-{}", u64::from_ne_bytes(nonce)));
        std::fs::create_dir(&directory).expect("private fixture directory");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .expect("private fixture permissions");
        let executable = directory.join(OsString::from_vec(executable_name.to_vec()));
        let socket = directory.join(OsString::from_vec(socket_name.to_vec()));
        let config = directory.join("tmux.conf");
        let owner = directory.join("owner");
        std::os::unix::fs::symlink(&actual, &executable).expect("raw executable symlink");
        std::fs::write(&config, []).expect("empty fixture config");
        std::fs::write(&owner, std::process::id().to_string()).expect("fixture owner record");
        Self {
            directory,
            bootstrap_executable: actual,
            executable,
            socket,
            config,
            owner,
            running: false,
            cleaned: false,
        }
    }

    async fn start(&mut self, name: &str) -> (Server, String) {
        let server = Server::builder()
            .tmux_executable(self.bootstrap_executable.clone())
            .socket_path(self.socket.clone())
            .config_file(self.config.clone())
            .build()
            .expect("raw server config");
        self.running = true;
        let session = server
            .new_session(NewSessionOptions::new(name).command("/bin/sh"))
            .await
            .expect("raw-path server starts");
        let pane = session
            .panes()
            .await
            .expect("panes list")
            .remove(0)
            .id()
            .to_string();
        prompt_ready(&server, &pane).await;
        (server, pane)
    }

    fn route(&self) -> Server {
        Server::builder()
            .tmux_executable(self.executable.clone())
            .socket_path(self.socket.clone())
            .config_file(self.config.clone())
            .build()
            .expect("raw route config")
    }

    async fn shutdown(&mut self, server: &Server) {
        server
            .cmd(Command::new("kill-server"))
            .await
            .expect("raw server stops");
        self.running = false;
        self.cleanup().expect("raw fixture files are removed");
        assert!(!self.directory.exists(), "raw fixture directory is gone");
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if self.running {
            let status = std::process::Command::new(&self.bootstrap_executable)
                .arg("-S")
                .arg(&self.socket)
                .arg("kill-server")
                .status()?;
            if !status.success() && self.socket.exists() {
                return Err(io::Error::other("raw-path tmux server did not stop"));
            }
            self.running = false;
        }
        for path in [&self.socket, &self.executable, &self.config, &self.owner] {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        std::fs::remove_dir(&self.directory)?;
        self.cleaned = true;
        Ok(())
    }
}

impl Drop for RawServerFiles {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = self.cleanup();
        }
    }
}

async fn fixture(name: &str) -> (TestServer, TmuxTools) {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": name})))
        .await
        .expect("session is created");
    (guard, tools)
}

async fn panes(tools: &TmuxTools) -> Vec<Value> {
    json(tools.list_panes().await.expect("panes"))["panes"]
        .as_array()
        .expect("pane rows")
        .clone()
}

async fn typing_fixture(name: &str) -> (TestServer, TmuxTools, String) {
    let (guard, tools) = fixture(name).await;
    let pane = panes(&tools).await[0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    prompt_ready(guard.server(), &pane).await;
    (guard, tools, pane)
}

async fn split(server: &Server, pane: &str) -> String {
    let pane = server
        .panes()
        .await
        .expect("panes list")
        .into_iter()
        .find(|candidate| candidate.id().to_string() == pane)
        .expect("pane exists");
    pane.split(SplitOptions::new(SplitDirection::Below))
        .await
        .expect("pane splits")
        .id()
        .to_string()
}

async fn pane_handle(server: &Server, pane: &str) -> libtmux::Pane {
    server
        .panes()
        .await
        .expect("panes list")
        .into_iter()
        .find(|candidate| candidate.id().to_string() == pane)
        .expect("pane exists")
}

async fn set_window_synchronized(server: &Server, pane: &str, enabled: bool) {
    let pane = pane_handle(server, pane).await;
    server
        .window_by_id(pane.window_id())
        .await
        .expect("window lookup")
        .expect("source window exists")
        .set_option("synchronize-panes", if enabled { "on" } else { "off" })
        .await
        .expect("window synchronization changes");
}

async fn synchronized_fixture(name: &str) -> (TestServer, TmuxTools, String, String) {
    let (guard, tools, source) = typing_fixture(name).await;
    let peer = split(guard.server(), &source).await;
    prompt_ready(guard.server(), &peer).await;
    set_window_synchronized(guard.server(), &source, true).await;
    (guard, tools, source, peer)
}

async fn socket_of(server: &Server) -> String {
    server
        .cmd(
            Command::new("display-message")
                .arg("-p")
                .arg("#{socket_path}"),
        )
        .await
        .expect("tmux reports its socket")
        .stdout_lossy()
        .trim()
        .to_owned()
}

async fn identity_for(server: &Server, pane: &str) -> CallerIdentity {
    CallerIdentity::from_values(
        Some(format!("{},1,$0", socket_of(server).await).into()),
        Some(pane.into()),
    )
    .expect("caller identity")
}

async fn caller_tools(server: &Server, pane: &str) -> TmuxTools {
    TmuxTools::builder(server.clone())
        .caller(Some(identity_for(server, pane).await))
        .build()
}

fn assert_self_protection(error: rmcp::model::ErrorData, pane: &str) {
    assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(error.message.contains(pane), "{}", error.message);
    assert_eq!(
        error.data.expect("typed refusal")["kind"],
        "self_protection"
    );
}

async fn client_count(server: &Server) -> usize {
    server.clients().await.map_or(0, |clients| clients.len())
}

async fn clients_settle(server: &Server, wanted: usize) -> usize {
    let mut seen = client_count(server).await;
    for _ in 0..200 {
        if seen == wanted {
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
        seen = client_count(server).await;
    }
    seen
}

async fn run_view(tools: &TmuxTools, pane: &str, command: &str) -> Value {
    json(
        tools
            .run_command(
                args(serde_json::json!({
                    "pane": pane,
                    "command": command,
                    "seconds": 5
                })),
                CancellationToken::new(),
                tmux_mcp::Reporter::none(),
            )
            .await
            .expect("command run answers"),
    )
}

async fn run_error(tools: &TmuxTools, pane: &str, command: &str) -> rmcp::model::ErrorData {
    tools
        .run_command(
            args(serde_json::json!({
                "pane": pane,
                "command": command,
                "seconds": 2
            })),
            CancellationToken::new(),
            tmux_mcp::Reporter::none(),
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("guarded run is refused: {command}"))
}

async fn assert_channel_quiet(server: &Server, channel: &str) {
    assert_eq!(
        server
            .wait_for_channel(channel, Duration::from_millis(250))
            .await
            .expect("channel wait answers"),
        libtmux::ChannelWait::TimedOut,
        "refused input signalled {channel}"
    );
}

async fn pane_screen(tools: &TmuxTools, pane: &str) -> String {
    json(
        tools
            .capture_pane(args(serde_json::json!({"pane": pane})))
            .await
            .expect("pane capture answers"),
    )["text"]
        .as_str()
        .expect("capture text")
        .to_owned()
}

async fn assert_paste_refused_unchanged(
    tools: &TmuxTools,
    server: &Server,
    pane: &str,
    kind: &str,
) {
    let buffers = server.buffer_names().await.expect("buffers list");
    let screen = pane_screen(tools, pane).await;
    let error = tools
        .paste_text(args(serde_json::json!({"pane": pane, "text": "guarded"})))
        .await
        .err()
        .expect("paste is refused");
    assert_eq!(error.data.expect("typed refusal")["kind"], kind);
    assert_eq!(server.buffer_names().await.expect("buffers list"), buffers);
    assert_eq!(pane_screen(tools, pane).await, screen);
}

async fn send_and_wait(
    tools: &TmuxTools,
    server: &Server,
    pane: &str,
    command: &str,
    channel: &str,
) {
    let foreground = pane_handle(server, pane)
        .await
        .current_command()
        .cloned()
        .expect("fixture shell reports its foreground command");
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": format!("{command}; tmux wait-for -S {channel}"),
            "enter": true
        })))
        .await
        .expect("fixture shell input is sent");
    assert_eq!(
        server
            .wait_for_channel(channel, Duration::from_secs(2))
            .await
            .expect("fixture signal is observed"),
        libtmux::ChannelWait::Signalled
    );
    libtmux::test::retry_until(Duration::from_secs(2), async || {
        pane_handle(server, pane).await.current_command() == Some(&foreground)
    })
    .await
    .expect("fixture shell regains the foreground after signalling");
}

#[tokio::test]
async fn send_keys_reports_synchronized_target_expansion() {
    let (guard, tools) = fixture("synchronized-targets").await;
    let first = panes(&tools).await[0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    split(guard.server(), &first).await;
    let listed = panes(&tools).await;
    let window = listed[0]["window_id"].as_str().expect("window id");
    let expected: BTreeSet<_> = listed
        .iter()
        .map(|pane| pane["id"].as_str().expect("pane id"))
        .collect();
    guard
        .server()
        .windows()
        .await
        .expect("windows list")
        .into_iter()
        .find(|candidate| candidate.id().to_string() == window)
        .expect("window exists")
        .set_option("synchronize-panes", "on")
        .await
        .expect("synchronized input is enabled");

    let result = json(
        tools
            .send_keys(args(serde_json::json!({"pane": first, "keys": ["C-l"]})))
            .await
            .expect("keys are sent"),
    );
    let actual: BTreeSet<_> = result["panes"]
        .as_array()
        .expect("resolved panes")
        .iter()
        .map(|pane| pane.as_str().expect("pane id"))
        .collect();

    assert_eq!(actual, expected);
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn send_keys_uses_the_effective_per_pane_cohort() {
    let (guard, tools, source) = typing_fixture("effective-cohort").await;
    let peer = split(guard.server(), &source).await;
    let excluded = split(guard.server(), &source).await;
    for pane in [&peer, &excluded] {
        prompt_ready(guard.server(), pane).await;
    }
    let source_handle = pane_handle(guard.server(), &source).await;
    let window = guard
        .server()
        .window_by_id(source_handle.window_id())
        .await
        .expect("window lookup")
        .expect("source window exists");
    window
        .set_option("synchronize-panes", "on")
        .await
        .expect("window synchronization is enabled");
    pane_handle(guard.server(), &excluded)
        .await
        .set_option("synchronize-panes", "off")
        .await
        .expect("one peer opts out");

    let linked = guard
        .server()
        .new_session("effective-cohort-link")
        .await
        .expect("linked session starts");
    window
        .link_to(&linked, None)
        .await
        .expect("source window is linked");
    let other = linked
        .active_window()
        .await
        .expect("active window lookup")
        .expect("linked session has a window");
    other
        .set_option("synchronize-panes", "on")
        .await
        .expect("other window synchronizes independently");

    let result = json(
        tools
            .send_keys(args(serde_json::json!({"pane": source, "keys": ["C-l"]})))
            .await
            .expect("keys are sent"),
    );
    let actual: Vec<_> = result["panes"]
        .as_array()
        .expect("configured cohort")
        .iter()
        .map(|pane| pane.as_str().expect("pane id").to_owned())
        .collect();
    let mut expected = vec![source, peer];
    expected.sort_unstable();

    assert_eq!(actual, expected);
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn send_keys_refuses_modal_and_dead_configured_members_before_input() {
    let (guard, tools, source, peer) = synchronized_fixture("cohort-refusal").await;

    let peer_handle = pane_handle(guard.server(), &peer).await;
    peer_handle
        .copy_mode()
        .await
        .expect("fixture enters copy mode");
    let modal_channel = "mcp-modal-refusal";
    let modal_error = tools
        .send_keys(args(serde_json::json!({
            "pane": source,
            "text": format!("tmux wait-for -S {modal_channel}"),
            "enter": true
        })))
        .await
        .err()
        .expect("a modal configured peer refuses the whole input");
    assert_eq!(
        modal_error.data.expect("typed refusal")["kind"],
        "invalid_input"
    );
    assert_channel_quiet(guard.server(), modal_channel).await;

    peer_handle
        .exit_mode()
        .await
        .expect("fixture leaves copy mode");
    peer_handle
        .set_option("remain-on-exit", "on")
        .await
        .expect("fixture retains a dead pane");
    let mut peer_handle = peer_handle;
    peer_handle
        .respawn(Some("exit 0"), true)
        .await
        .expect("fixture command exits");
    libtmux::test::retry_until(Duration::from_secs(2), async || {
        pane_handle(guard.server(), &peer).await.is_dead()
    })
    .await
    .expect("peer becomes dead");
    let dead_channel = "mcp-dead-refusal";
    tools
        .send_keys(args(serde_json::json!({
            "pane": source,
            "text": format!("tmux wait-for -S {dead_channel}"),
            "enter": true
        })))
        .await
        .err()
        .expect("a dead configured peer refuses the whole input");
    assert_channel_quiet(guard.server(), dead_channel).await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn an_unsynchronized_source_ignores_unreached_modal_peers() {
    let (guard, tools, source, peer) = synchronized_fixture("unsynchronized-source").await;
    let source_handle = pane_handle(guard.server(), &source).await;
    source_handle
        .set_option("synchronize-panes", "off")
        .await
        .expect("source opts out");
    pane_handle(guard.server(), &peer)
        .await
        .copy_mode()
        .await
        .expect("unreached peer enters copy mode");

    let sent = json(
        tools
            .send_keys(args(serde_json::json!({"pane": source, "keys": ["C-l"]})))
            .await
            .expect("source-only input succeeds"),
    );
    assert_eq!(sent["panes"], serde_json::json!([source]));
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn pane_input_and_batch_protect_only_reached_caller_panes() {
    let (guard, _, source) = typing_fixture("input-caller").await;
    let direct = caller_tools(guard.server(), &source).await;
    let direct_channel = "mcp-input-direct-caller";
    let error = direct
        .send_keys(args(serde_json::json!({
            "pane": source,
            "text": format!("tmux wait-for -S {direct_channel}"),
            "enter": true
        })))
        .await
        .err()
        .expect("direct caller input is refused");
    assert_self_protection(error, &source);
    assert_channel_quiet(guard.server(), direct_channel).await;

    let peer = split(guard.server(), &source).await;
    prompt_ready(guard.server(), &peer).await;
    set_window_synchronized(guard.server(), &source, true).await;
    let peer_caller = caller_tools(guard.server(), &peer).await;
    let peer_channel = "mcp-input-synchronized-caller";
    let error = peer_caller
        .send_keys(args(serde_json::json!({
            "pane": source,
            "text": format!("tmux wait-for -S {peer_channel}"),
            "enter": true
        })))
        .await
        .err()
        .expect("a synchronized caller peer refuses the whole input");
    assert_self_protection(error, &peer);
    assert_channel_quiet(guard.server(), peer_channel).await;

    pane_handle(guard.server(), &source)
        .await
        .set_option("synchronize-panes", "off")
        .await
        .expect("source opts out");
    send_and_wait(
        &peer_caller,
        guard.server(),
        &source,
        "true",
        "mcp-input-unreached-caller",
    )
    .await;

    let batch_channel = "mcp-input-batch-caller";
    let response = call_tool(
        peer_caller,
        "send_keys_batch",
        serde_json::json!({
            "operations": [
                {"pane": source, "keys": ["C-l"], "enter": false},
                {
                    "pane": peer,
                    "text": format!("tmux wait-for -S {batch_channel}"),
                    "enter": true
                },
                {"pane": source, "keys": ["C-l"], "enter": false}
            ],
            "on_error": "continue"
        }),
    )
    .await;
    let result = response
        .structured_content
        .as_ref()
        .unwrap_or_else(|| panic!("batch has structured content: {response:?}"));
    assert_eq!(result["succeeded"], 2, "{result}");
    assert_eq!(result["failed"], 1, "{result}");
    assert_eq!(result["results"][0]["success"], true, "{result}");
    assert_eq!(
        result["results"][1]["error"]["data"]["kind"], "self_protection",
        "{result}"
    );
    assert_eq!(result["results"][2]["success"], true, "{result}");
    assert_channel_quiet(guard.server(), batch_channel).await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn paste_text_is_target_only_and_guards_before_buffer_creation() {
    let (guard, _, source, peer) = synchronized_fixture("paste-preflight").await;
    let tools = caller_tools(guard.server(), &peer).await;
    let peer_screen = pane_screen(&tools, &peer).await;
    tools
        .paste_text(args(serde_json::json!({
            "pane": source,
            "text": "MCP-PASTE-TARGET"
        })))
        .await
        .expect("target-only paste succeeds");
    assert!(
        pane_screen(&tools, &source)
            .await
            .contains("MCP-PASTE-TARGET")
    );
    assert_eq!(pane_screen(&tools, &peer).await, peer_screen);

    let mut source_handle = pane_handle(guard.server(), &source).await;
    source_handle
        .copy_mode()
        .await
        .expect("fixture enters copy mode");
    assert_paste_refused_unchanged(&tools, guard.server(), &source, "invalid_input").await;

    source_handle
        .exit_mode()
        .await
        .expect("fixture leaves copy mode");
    source_handle
        .set_option("remain-on-exit", "on")
        .await
        .expect("fixture retains a dead pane");
    source_handle
        .respawn(Some("exit 0"), true)
        .await
        .expect("fixture command exits");
    libtmux::test::retry_until(Duration::from_secs(2), async || {
        pane_handle(guard.server(), &source).await.is_dead()
    })
    .await
    .expect("source becomes dead");
    assert_paste_refused_unchanged(&tools, guard.server(), &source, "invalid_input").await;
    assert_paste_refused_unchanged(&tools, guard.server(), &peer, "self_protection").await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn send_keys_batch_preflights_each_executed_row() {
    let (guard, tools, source, peer) = synchronized_fixture("batch-preflight").await;
    let target = guard
        .server()
        .new_session("batch-target")
        .await
        .expect("independent target session starts")
        .active_window()
        .await
        .expect("active window lookup")
        .expect("target window exists")
        .active_pane()
        .await
        .expect("active pane lookup")
        .expect("target pane exists")
        .id()
        .to_string();
    prompt_ready(guard.server(), &target).await;
    guard
        .server()
        .set_hook("after-send-keys", format!("copy-mode -t {peer}"))
        .await
        .expect("row transition hook is installed");

    for on_error in ["continue", "stop"] {
        pane_handle(guard.server(), &peer)
            .await
            .exit_mode()
            .await
            .expect("peer mode is reset between cases");
        let channel = format!("mcp-batch-{on_error}");
        let response = call_tool(
            tools.clone(),
            "send_keys_batch",
            serde_json::json!({
                "operations": [
                    {"pane": source, "keys": ["C-l"], "enter": false},
                    {"pane": source, "keys": ["C-l"], "enter": false},
                    {
                        "pane": target,
                        "text": format!("tmux wait-for -S {channel}"),
                        "enter": true
                    }
                ],
                "on_error": on_error
            }),
        )
        .await;
        let result = response
            .structured_content
            .as_ref()
            .unwrap_or_else(|| panic!("batch has structured content: {response:?}"));
        assert_eq!(result["failed"], 1, "{on_error}: {result}");
        assert_eq!(
            result["succeeded"],
            if on_error == "continue" { 2 } else { 1 }
        );
        if on_error == "continue" {
            assert_eq!(
                guard
                    .server()
                    .wait_for_channel(&channel, Duration::from_secs(2))
                    .await
                    .expect("continued row signals"),
                libtmux::ChannelWait::Signalled
            );
        } else {
            assert_channel_quiet(guard.server(), &channel).await;
        }
    }
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn teardown_refuses_the_inherited_caller_pane() {
    let (guard, bare) = fixture("caller-guard").await;
    let own = panes(&bare).await[0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    let tools = TmuxTools::builder(guard.server().clone())
        .caller(Some(identity_for(guard.server(), &own).await))
        .build();

    let error = tools
        .kill_pane(args(serde_json::json!({"pane": own})))
        .await
        .map(|_| ())
        .expect_err("caller pane is protected");

    assert!(error.message.contains(&own), "{}", error.message);
    assert_eq!(panes(&tools).await.len(), 1);
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn real_tmux_compat_run_shell_command_reports_output_status_and_cancellation() {
    let (guard, tools, pane) = typing_fixture("run").await;
    let baseline_clients = client_count(guard.server()).await;
    let finished = json(
        tools
            .run_command(
                args(serde_json::json!({
                    "pane": pane,
                    "command": "printf retained-output; exit 3",
                    "seconds": 20
                })),
                CancellationToken::new(),
                tmux_mcp::Reporter::none(),
            )
            .await
            .expect("command runs"),
    );
    assert_eq!(finished["outcome"], "completed");
    assert_eq!(finished["exit_status"], 3);
    assert!(
        finished["output"]
            .as_str()
            .expect("output")
            .contains("retained-output")
    );

    let cancelled = CancellationToken::new();
    let request = tokio::spawn({
        let tools = tools.clone();
        let pane = pane.clone();
        let cancelled = cancelled.clone();
        async move {
            tools
                .run_command(
                    args(serde_json::json!({
                        "pane": pane,
                        "command": "sleep 30",
                        "seconds": 60
                    })),
                    cancelled,
                    tmux_mcp::Reporter::none(),
                )
                .await
        }
    });
    assert_eq!(
        clients_settle(guard.server(), baseline_clients + 1).await,
        baseline_clients + 1,
        "the request owns one live output client while the command runs"
    );
    cancelled.cancel();
    let stopped = json(request.await.expect("request joins").expect("run answers"));
    assert_eq!(stopped["outcome"], "cancelled");
    assert_eq!(
        clients_settle(guard.server(), baseline_clients).await,
        baseline_clients,
        "cancellation must close the request-owned output client"
    );
    assert!(
        !json(tools.list_panes().await.expect("server still answers"))["panes"]
            .as_array()
            .expect("pane list")
            .is_empty()
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn run_refuses_initial_input_before_watcher_setup() {
    for boundary in ["cohort", "caller"] {
        let (guard, bare, source) = typing_fixture(&format!("run-initial-{boundary}")).await;
        let (tools, expected_kind) = if boundary == "cohort" {
            let peer = split(guard.server(), &source).await;
            prompt_ready(guard.server(), &peer).await;
            set_window_synchronized(guard.server(), &source, true).await;
            (bare, "invalid_input")
        } else {
            (
                caller_tools(guard.server(), &source).await,
                "self_protection",
            )
        };
        guard
            .server()
            .set_hook(
                "client-attached",
                "set-option -g @mcp-initial-watcher attached",
            )
            .await
            .expect("watcher hook is installed");
        let baseline_clients = client_count(guard.server()).await;
        let channel = format!("mcp-initial-run-{boundary}");

        let error = run_error(&tools, &source, &format!("tmux wait-for -S {channel}")).await;

        assert_eq!(
            error.data.expect("typed refusal")["kind"],
            expected_kind,
            "{boundary}"
        );
        assert_eq!(client_count(guard.server()).await, baseline_clients);
        assert_eq!(
            guard
                .server()
                .get_global_option("@mcp-initial-watcher")
                .await
                .expect("hook record is read"),
            None,
            "{boundary}: the initial guard runs before watcher attachment"
        );
        assert_channel_quiet(guard.server(), &channel).await;
        guard.shutdown().await.expect("tmux fixture shuts down");
    }
}

#[tokio::test]
async fn run_rechecks_state_immediately_before_dispatch() {
    for transition in ["cohort", "mode"] {
        let name = format!("run-final-{transition}");
        let (guard, tools, source) = typing_fixture(&name).await;
        let source_handle = pane_handle(guard.server(), &source).await;
        let hook = match transition {
            "cohort" => {
                let peer = split(guard.server(), &source).await;
                prompt_ready(guard.server(), &peer).await;
                format!(
                    "set-option -w -t {} synchronize-panes on",
                    source_handle.window_id()
                )
            }
            "mode" => format!("copy-mode -t {source}"),
            _ => unreachable!(),
        };
        guard
            .server()
            .set_hook("client-attached", hook)
            .await
            .expect("transition hook is installed");
        let baseline_clients = client_count(guard.server()).await;
        let channel = format!("mcp-final-{transition}");

        let error = run_error(&tools, &source, &format!("tmux wait-for -S {channel}")).await;

        assert_eq!(
            error.data.expect("typed refusal")["kind"],
            "invalid_input",
            "{transition}"
        );
        assert_eq!(
            clients_settle(guard.server(), baseline_clients).await,
            baseline_clients,
            "{transition}: final refusal closes the watcher"
        );
        assert_channel_quiet(guard.server(), &channel).await;
        guard.shutdown().await.expect("tmux fixture shuts down");
    }
}

#[tokio::test]
async fn run_reports_phase_aware_source_disappearance() {
    let (guard, tools, source) = typing_fixture("run-disappearance").await;
    split(guard.server(), &source).await;
    guard
        .server()
        .set_hook("client-attached", format!("kill-pane -t {source}"))
        .await
        .expect("transition hook is installed");
    let baseline_clients = client_count(guard.server()).await;

    let final_error = tools
        .run_command(
            args(serde_json::json!({
                "pane": source,
                "command": "printf should-not-run",
                "seconds": 2
            })),
            CancellationToken::new(),
            tmux_mcp::Reporter::none(),
        )
        .await
        .err()
        .expect("a pane killed after the initial checkpoint is refused");
    let final_detail = final_error.data.expect("transition detail");
    assert_eq!(final_error.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
    assert_eq!(final_detail["kind"], "object_gone");
    assert_eq!(final_detail["retryable"], false);
    assert_eq!(final_detail["stale"], true);
    assert_eq!(
        clients_settle(guard.server(), baseline_clients).await,
        baseline_clients,
        "the prepared watcher is closed"
    );

    let initial_error = tools
        .run_command(
            args(serde_json::json!({
                "pane": "%4294967295",
                "command": "printf unknown",
                "seconds": 2
            })),
            CancellationToken::new(),
            tmux_mcp::Reporter::none(),
        )
        .await
        .err()
        .expect("an initially unknown pane is caller input");
    assert_eq!(initial_error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        initial_error.data.expect("caller detail")["kind"],
        "object_gone"
    );
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn run_transport_preserves_raw_executable_and_socket_paths() {
    let mut files = RawServerFiles::create(b"tmux-\'\xff", b"socket-\'\xfe");
    let (bootstrap, pane) = files.start("raw-transport").await;
    let route = files.route();
    let result = run_view(&bare_tools(&route), &pane, "printf RAW-TRANSPORT; false").await;

    assert_eq!(
        route
            .resolved_tmux_executable()
            .expect("raw executable resolves")
            .as_os_str()
            .as_bytes(),
        files.executable.as_os_str().as_bytes()
    );
    assert_eq!(
        route.socket_path().as_os_str().as_bytes(),
        files.socket.as_os_str().as_bytes()
    );
    assert_eq!(result["exit_status"], 1, "{result}");
    assert!(
        result["output"]
            .as_str()
            .expect("output")
            .contains("RAW-TRANSPORT")
    );
    files.shutdown(&bootstrap).await;
}

async fn assert_terminal_control_route_is_preflight_failure(
    executable_name: &[u8],
    socket_name: &[u8],
    case: &str,
) {
    let mut files = RawServerFiles::create(executable_name, socket_name);
    let (bootstrap, pane) = files.start(&format!("route-control-{case}")).await;
    let tools = bare_tools(&files.route());
    bootstrap
        .set_hook(
            "client-attached",
            "set-option -g @mcp-route-watcher attached",
        )
        .await
        .expect("watcher attachment is observable");
    bootstrap
        .set_hook(
            "after-display-message",
            "set-option -g @mcp-route-display seen",
        )
        .await
        .expect("display transport is observable");
    let baseline_clients = client_count(&bootstrap).await;
    let bootstrap_tools = bare_tools(&bootstrap);
    let screen = pane_screen(&bootstrap_tools, &pane).await;
    let channel = format!("mcp-route-control-{case}");

    let error = run_error(&tools, &pane, &format!("tmux wait-for -S {channel}")).await;

    assert_eq!(error.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
    assert_eq!(error.data.expect("typed refusal")["kind"], "unreachable");
    assert_eq!(client_count(&bootstrap).await, baseline_clients);
    assert_eq!(
        bootstrap
            .get_global_option("@mcp-route-watcher")
            .await
            .expect("watcher record is read"),
        None,
        "{case}: route validation precedes watcher attachment"
    );
    assert_eq!(
        bootstrap
            .get_global_option("@mcp-route-display")
            .await
            .expect("display record is read"),
        None,
        "{case}: no completion display payload ran"
    );
    assert_eq!(pane_screen(&bootstrap_tools, &pane).await, screen);
    assert_channel_quiet(&bootstrap, &channel).await;
    files.shutdown(&bootstrap).await;
}

#[tokio::test]
async fn run_rejects_terminal_control_in_the_executable_route() {
    assert_terminal_control_route_is_preflight_failure(b"tmux-\x03", b"socket", "executable").await;
}

#[tokio::test]
async fn run_rejects_terminal_control_in_the_socket_route() {
    assert_terminal_control_route_is_preflight_failure(b"tmux", b"socket-\x03", "socket").await;
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one pane must retain state across the framing cases"
)]
async fn run_framing_preserves_parent_shell_state_and_status() {
    let (guard, tools, pane) = typing_fixture("run-frame-state").await;
    send_and_wait(
        &tools,
        guard.server(),
        &pane,
        "set -- original-positional; readonly __tmux_mcp=parent; readonly __tmux_mcp_status=parent; readonly MCP_PARENT_PWD=$PWD",
        "mcp-frame-state-ready",
    )
    .await;

    let changed = run_view(
        &tools,
        &pane,
        "printf 'POS:%s' \"$1\"; cd /; export MCP_FRAME_CHILD=changed; false",
    )
    .await;
    assert_eq!(changed["outcome"], "completed");
    assert_eq!(changed["exit_status"], 1);
    assert!(
        changed["output"]
            .as_str()
            .expect("output")
            .contains("POS:original-positional")
    );

    let parent = run_view(
        &tools,
        &pane,
        "test \"$PWD\" = \"$MCP_PARENT_PWD\"; printf 'STATE:%s:%s:%s' \"$1\" \"$__tmux_mcp\" \"${MCP_FRAME_CHILD-unset}\"",
    )
    .await;
    assert_eq!(parent["exit_status"], 0);
    assert!(
        parent["output"]
            .as_str()
            .expect("output")
            .contains("STATE:original-positional:parent:unset")
    );

    let exited = run_view(&tools, &pane, "exit 7").await;
    assert_eq!(exited["outcome"], "completed");
    assert_eq!(exited["exit_status"], 7);
    let trailing = run_view(&tools, &pane, "printf COMMENT # valid trailing comment").await;
    assert_eq!(trailing["exit_status"], 0);
    assert!(
        trailing["output"]
            .as_str()
            .expect("output")
            .contains("COMMENT")
    );
    let invalid = run_view(&tools, &pane, "if then").await;
    assert_eq!(invalid["outcome"], "completed");
    assert_ne!(invalid["exit_status"], 0);
    let defined = run_view(&tools, &pane, "printf() { :; }; trap ':' 0; false").await;
    assert_eq!(defined["exit_status"], 1);

    send_and_wait(
        &tools,
        guard.server(),
        &pane,
        "stty -onlcr",
        "mcp-frame-lf-ready",
    )
    .await;
    let lf = run_view(&tools, &pane, "printf LF").await;
    assert_eq!(lf["exit_status"], 0);
    assert!(lf["output"].as_str().expect("output").contains("LF"));
    send_and_wait(
        &tools,
        guard.server(),
        &pane,
        "stty onlcr",
        "mcp-frame-crlf-ready",
    )
    .await;

    let executable = guard
        .server()
        .resolved_tmux_executable()
        .expect("fixture tmux resolves");
    let executable = executable
        .to_str()
        .expect("fixture path supports a shell alias");
    send_and_wait(
        &tools,
        guard.server(),
        &pane,
        &format!("printf() {{ :; }}; command() {{ :; }}; alias printf=: command=: {executable}=:"),
        "mcp-frame-shadows-ready",
    )
    .await;
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "cd /; PATH=/libtmux-mcp-missing; echo MCP-DRIFT-READY",
            "enter": true
        })))
        .await
        .expect("pane launch context is changed");
    libtmux::test::retry_until(Duration::from_secs(2), async || {
        pane_screen(&tools, &pane).await.contains("MCP-DRIFT-READY")
    })
    .await
    .expect("pane launch-context drift is visible");
    let shadowed = run_view(&tools, &pane, "/usr/bin/printf BODY; false").await;
    assert_eq!(shadowed["outcome"], "completed");
    assert_eq!(shadowed["exit_status"], 1);
    assert!(
        shadowed["output"]
            .as_str()
            .expect("output")
            .contains("BODY")
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn run_frame_preserves_inherited_xtrace_without_frame_trace() {
    let (guard, tools, pane) = typing_fixture("run-frame-xtrace").await;
    let executable = guard
        .server()
        .resolved_tmux_executable()
        .expect("fixture tmux resolves");
    let executable = executable.as_os_str().to_string_lossy().into_owned();
    let socket = guard
        .server()
        .socket_path()
        .as_os_str()
        .to_string_lossy()
        .into_owned();

    for (name, enable_errexit) in [("x", false), ("xe", true)] {
        let options = if enable_errexit {
            "set -ex"
        } else {
            "set +e; set -x"
        };
        send_and_wait(
            &tools,
            guard.server(),
            &pane,
            &format!("PS4='MCP-USER-TRACE-{name}:'; {options}"),
            &format!("mcp-frame-{name}-ready"),
        )
        .await;
        let command = if enable_errexit {
            "case $- in *x*) printf XSEEN;; *) printf XLOST;; esac; printf BODY; false; printf AFTER"
        } else {
            "case $- in *x*) printf XSEEN;; *) printf XLOST;; esac; printf BODY; false"
        };
        let result = run_view(&tools, &pane, command).await;
        let output = result["output"].as_str().expect("output");
        assert_eq!(result["outcome"], "completed", "{name}: {result}");
        assert_eq!(result["exit_status"], 1, "{name}: {result}");
        assert!(output.contains("MCP-USER-TRACE"), "{name}: {output:?}");
        assert!(output.contains("XSEEN"), "{name}: {output:?}");
        assert!(output.contains("BODY"), "{name}: {output:?}");
        if enable_errexit {
            assert!(!output.contains("AFTER"), "{name}: {output:?}");
        }
        for forbidden in [
            "set +e",
            "set -e",
            "set --",
            "run-shell",
            "eval ",
            executable.as_str(),
            socket.as_str(),
        ] {
            assert!(
                !output.contains(forbidden),
                "{name}: leaked {forbidden:?} in {output:?}"
            );
        }

        send_and_wait(
            &tools,
            guard.server(),
            &pane,
            "case $- in *x*) :;; *) false;; esac; set +ex",
            &format!("mcp-frame-{name}-parent-x"),
        )
        .await;
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn wait_and_cursor_tools_observe_live_output() {
    let (guard, tools, pane) = typing_fixture("observe").await;
    let opened = json(
        tools
            .capture_since(args(serde_json::json!({"pane": pane})))
            .await
            .expect("tail opens"),
    );
    let cursor = opened["cursor"].as_str().expect("cursor").to_owned();
    let waiting = tokio::spawn({
        let tools = tools.clone();
        let pane = pane.clone();
        async move {
            tools
                .wait_for_text(
                    args(serde_json::json!({
                        "pane": pane,
                        "patterns": ["live-marker"],
                        "seconds": 20
                    })),
                    CancellationToken::new(),
                    tmux_mcp::Reporter::none(),
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "printf live-marker",
            "enter": true
        })))
        .await
        .expect("input is sent");

    let waited = json(waiting.await.expect("wait joins").expect("wait answers"));
    assert_eq!(waited["outcome"], "matched");
    let mut since = Value::Null;
    for _ in 0..40 {
        since = json(
            tools
                .capture_since(args(serde_json::json!({"pane": pane, "cursor": cursor})))
                .await
                .expect("tail reads"),
        );
        if since["text"]
            .as_str()
            .is_some_and(|text| text.contains("live-marker"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(since["text"].as_str().unwrap().contains("live-marker"));

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn search_snapshot_and_configuration_reads_are_structured() {
    let (guard, tools, pane) = typing_fixture("inspect").await;
    tools
        .run_command(
            args(serde_json::json!({
                "pane": pane,
                "command": "echo searchable-marker",
                "seconds": 20
            })),
            CancellationToken::new(),
            tmux_mcp::Reporter::none(),
        )
        .await
        .expect("marker prints");
    guard
        .server()
        .set_global_option("@probe", "configured")
        .await
        .expect("fixture option is set");
    guard
        .server()
        .set_environment("TMUX_MCP_PROBE", "secret-like")
        .await
        .expect("fixture environment is set");

    let found = json(
        tools
            .search_panes(args(serde_json::json!({"pattern": "searchable-marker"})))
            .await
            .expect("search runs"),
    );
    assert!(!found["matches"].as_array().unwrap().is_empty());
    let snapshot = json(
        tools
            .snapshot_pane(args(serde_json::json!({"pane": pane, "max_lines": 5})))
            .await
            .expect("snapshot reads"),
    );
    assert_eq!(snapshot["pane"]["id"], pane);
    let variables = json(
        tools
            .get_tmux_variables(args(serde_json::json!({
                "names": ["pane_id", "session_name"],
                "pane": pane
            })))
            .await
            .expect("tmux variables read"),
    );
    assert_eq!(variables["values"]["pane_id"], pane);
    assert_eq!(variables["values"]["session_name"], "inspect");
    let option = json(
        tools
            .show_option(args(serde_json::json!({
                "name": "@probe",
                "scope": "global-session"
            })))
            .await
            .expect("option reads"),
    );
    assert_eq!(option["value"], "configured");
    let environment = json(
        tools
            .show_environment(args(serde_json::json!({})))
            .await
            .expect("environment reads"),
    );
    assert!(
        environment["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| { entry["name"] == "TMUX_MCP_PROBE" && entry["value"] == "secret-like" })
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn selection_paste_and_channel_handlers_change_tmux() {
    let (guard, tools, first) = typing_fixture("manage").await;
    let second = split(guard.server(), &first).await;
    let selected = json(
        tools
            .select_pane(args(serde_json::json!({"pane": second})))
            .await
            .expect("pane selects"),
    );
    assert_eq!(selected["id"], second);

    tools
        .paste_text(args(serde_json::json!({
            "pane": first,
            "text": "printf pasted-marker\n"
        })))
        .await
        .expect("text pastes");
    libtmux::test::retry_until(Duration::from_secs(2), async || {
        tools
            .capture_pane(args(serde_json::json!({"pane": first})))
            .await
            .ok()
            .is_some_and(|capture| {
                json(capture)["text"]
                    .as_str()
                    .unwrap()
                    .contains("pasted-marker")
            })
    })
    .await
    .expect("pasted text reaches the pane");
    assert!(
        guard
            .server()
            .buffer_names()
            .await
            .expect("temporary buffers are listed")
            .is_empty(),
        "paste_text must delete its temporary buffer"
    );

    let waiting = tokio::spawn({
        let tools = tools.clone();
        async move {
            tools
                .wait_for_channel(args(serde_json::json!({
                    "channel": "retained-channel",
                    "seconds": 20
                })))
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    tools
        .signal_channel(args(serde_json::json!({"channel": "retained-channel"})))
        .await
        .expect("channel signals");
    let released = json(waiting.await.expect("wait joins").expect("wait answers"));
    assert_eq!(released["outcome"], "signalled");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn window_selection_uses_core_fixture_setup() {
    let (guard, tools) = fixture("windows").await;
    let session = guard
        .server()
        .sessions()
        .await
        .expect("sessions list")
        .remove(0);
    let second = session
        .new_window(NewWindowOptions::new("second"))
        .await
        .expect("window starts")
        .id()
        .to_string();

    let selected = json(
        tools
            .select_window(args(serde_json::json!({"window": second})))
            .await
            .expect("window selects"),
    );
    assert!(
        selected["windows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|window| window["id"] == second && window["active"] == true)
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}
