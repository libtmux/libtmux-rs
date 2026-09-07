//! Live checks for the retained MCP tool families.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Child, Stdio};
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

struct LoggedTmux {
    directory: PathBuf,
    executable: PathBuf,
    log: PathBuf,
}

struct DispatchBarrier {
    held: PathBuf,
    release: PathBuf,
    released: bool,
}

#[derive(Clone, Copy)]
enum GuardedDispatch {
    Send,
    Paste,
}

impl GuardedDispatch {
    fn name(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Paste => "paste",
        }
    }

    fn tmux_command(self) -> &'static str {
        match self {
            Self::Send => "send-keys",
            Self::Paste => "paste-buffer",
        }
    }

    fn start(
        self,
        tools: TmuxTools,
        pane: String,
    ) -> tokio::task::JoinHandle<Result<(), rmcp::model::ErrorData>> {
        tokio::spawn(async move {
            match self {
                Self::Send => tools
                    .send_keys(args(serde_json::json!({"pane": pane, "keys": ["C-l"]})))
                    .await
                    .map(|_| ()),
                Self::Paste => tools
                    .paste_text(args(serde_json::json!({"pane": pane, "text": "race"})))
                    .await
                    .map(|_| ()),
            }
        })
    }
}

impl DispatchBarrier {
    async fn wait(&self) {
        libtmux::test::retry_until(Duration::from_secs(2), async || self.held.exists())
            .await
            .expect("the selected dispatch reaches its barrier");
    }

    fn release(&mut self) {
        std::fs::write(&self.release, []).expect("the dispatch barrier releases");
        self.released = true;
    }
}

impl Drop for DispatchBarrier {
    fn drop(&mut self) {
        if !self.released {
            drop(std::fs::write(&self.release, []));
        }
    }
}

impl LoggedTmux {
    fn new() -> Self {
        let actual = Server::new()
            .expect("default server config")
            .resolved_tmux_executable()
            .expect("configured tmux resolves");
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce).expect("fixture nonce");
        let directory = PathBuf::from("/tmp/libtmux-rs-test")
            .join(format!("logged-{}", u64::from_ne_bytes(nonce)));
        std::fs::create_dir(&directory).expect("private fixture directory");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .expect("private fixture permissions");
        let executable = directory.join("tmux");
        let log = directory.join("argv.log");
        let script = format!(
            "#!/bin/sh\n\
             barrier_root={}\n\
             for argument do\n\
             \thold=\"$barrier_root/hold-$argument\"\n\
             \tif [ -f \"$hold\" ]; then\n\
             \t\trm -f \"$hold\"\n\
             \t\theld=\"$barrier_root/held-$argument\"\n\
             \t\trelease=\"$barrier_root/release-$argument\"\n\
             \t\t: > \"$held\"\n\
             \t\twhile [ ! -f \"$release\" ]; do sleep 0.01; done\n\
             \t\trm -f \"$held\" \"$release\"\n\
             \t\tbreak\n\
             \tfi\n\
             done\n\
             printf '%s\\n' \"$*\" >> {}\n\
             exec {} \"$@\"\n",
            shell_quote(directory.as_os_str()),
            shell_quote(log.as_os_str()),
            shell_quote(actual.as_os_str()),
        );
        std::fs::write(&executable, script).expect("logging fixture executable");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("fixture executable permissions");
        Self {
            directory,
            executable,
            log,
        }
    }

    fn clear(&self) {
        std::fs::write(&self.log, []).expect("fixture log clears");
    }

    fn hold_next(&self, command: &str) -> DispatchBarrier {
        let armed = self.directory.join(format!("hold-{command}"));
        let held = self.directory.join(format!("held-{command}"));
        let release = self.directory.join(format!("release-{command}"));
        std::fs::write(armed, []).expect("the next selected dispatch is held");
        DispatchBarrier {
            held,
            release,
            released: false,
        }
    }

    fn send_dispatches(&self) -> usize {
        self.command_dispatches("send-keys")
    }

    fn command_dispatches(&self, command: &str) -> usize {
        std::fs::read_to_string(&self.log)
            .expect("fixture log reads")
            .lines()
            .filter(|line| {
                line.split_ascii_whitespace()
                    .any(|argument| argument == command)
            })
            .count()
    }
}

impl Drop for LoggedTmux {
    fn drop(&mut self) {
        drop(std::fs::remove_file(&self.log));
        drop(std::fs::remove_file(&self.executable));
        drop(std::fs::remove_dir(&self.directory));
    }
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

async fn set_pane_input(server: &Server, pane: &str, enabled: bool) {
    let result = server
        .cmd(
            Command::new("select-pane")
                .arg(if enabled { "-e" } else { "-d" })
                .arg("-t")
                .arg(pane),
        )
        .await
        .expect("pane input setting command runs");
    assert!(result.success(), "pane input setting changes");
}

struct TerminalClient(Child);

impl Drop for TerminalClient {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}

fn shell_quote(value: &OsStr) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\"'\"'"))
}

async fn attach_terminal_client(server: &Server, pane: &str) -> TerminalClient {
    let pane = pane_handle(server, pane).await;
    let executable = server
        .resolved_tmux_executable()
        .expect("fixture tmux resolves");
    let command = format!(
        "{} -S {} attach-session -t {}",
        shell_quote(executable.as_os_str()),
        shell_quote(server.socket_path().as_os_str()),
        shell_quote(OsStr::new(&pane.session_id().to_string())),
    );
    // util-linux takes the command behind `-c` and then the typescript file.
    // BSD `script`, which is what macOS ships, has no `-c` at all: the file
    // comes first and the command is the remaining arguments.
    let mut builder = std::process::Command::new("script");
    if cfg!(target_os = "macos") {
        builder
            .arg("-q")
            .arg("/dev/null")
            .arg("/bin/sh")
            .arg("-c")
            .arg(command);
    } else {
        builder.arg("-q").arg("-c").arg(command).arg("/dev/null");
    }
    let child = builder
        .env("TERM", "xterm")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("terminal client starts");
    libtmux::test::retry_until(Duration::from_secs(10), async || {
        server
            .clients()
            .await
            .is_ok_and(|clients| clients.iter().any(|client| !client.is_control_mode()))
    })
    .await
    .expect("terminal client attaches");
    TerminalClient(child)
}

async fn detach_terminal_clients(server: &Server) {
    for client in server.clients().await.expect("clients list") {
        if !client.is_control_mode() {
            client.detach().await.expect("terminal client detaches");
        }
    }
    libtmux::test::retry_until(Duration::from_secs(10), async || {
        server
            .clients()
            .await
            .is_ok_and(|clients| clients.iter().all(libtmux::Client::is_control_mode))
    })
    .await
    .expect("terminal client leaves the listing");
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
    let generation = server.generation().await.expect("server generation");
    let session_id = pane_handle(server, pane).await.session_id().to_string();
    let session = session_id
        .strip_prefix('$')
        .expect("tmux session ID has its canonical prefix");
    CallerIdentity::from_values(
        Some(
            format!(
                "{},{},{}",
                socket_of(server).await,
                generation.pid(),
                session
            )
            .into(),
        ),
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

type RunTask = tokio::task::JoinHandle<
    Result<rmcp::handler::server::wrapper::Json<tmux_mcp::RunView>, rmcp::model::ErrorData>,
>;

async fn waiting_run(
    server: &Server,
    tools: &TmuxTools,
    pane: &str,
    prefix: &str,
    seconds: u64,
    cancelled: CancellationToken,
) -> (RunTask, String) {
    let started = format!("{prefix}-started");
    let release = format!("{prefix}-release");
    let request = tokio::spawn({
        let tools = tools.clone();
        let pane = pane.to_owned();
        let started = started.clone();
        let release = release.clone();
        async move {
            tools
                .run_command(
                    args(serde_json::json!({
                        "pane": pane,
                        "command": format!(
                            "tmux wait-for -S {started}; tmux wait-for {release}"
                        ),
                        "seconds": seconds
                    })),
                    cancelled,
                    tmux_mcp::Reporter::none(),
                )
                .await
        }
    });
    await_channel(server, &started).await;
    (request, release)
}

fn assert_active_run(error: &rmcp::model::ErrorData, operation: &str) {
    assert_eq!(
        error.data.as_ref().expect("typed active-run refusal")["kind"],
        "active_run",
        "{operation}: {error}"
    );
}

async fn await_channel(server: &Server, channel: &str) {
    assert_eq!(
        server
            .wait_for_channel(channel, Duration::from_secs(2))
            .await
            .expect("channel wait answers"),
        libtmux::ChannelWait::Signalled,
        "channel was not signalled: {channel}"
    );
}

async fn signal_channel(server: &Server, channel: &str) {
    server
        .signal_channel(channel)
        .await
        .unwrap_or_else(|error| panic!("channel signal failed for {channel}: {error}"));
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
async fn pane_input_refuses_input_disabled_configured_members() {
    let (guard, tools, source, peer) = synchronized_fixture("input-disabled").await;

    set_pane_input(guard.server(), &peer, false).await;
    assert!(pane_handle(guard.server(), &peer).await.is_input_disabled());
    let channel = "mcp-input-disabled-peer";
    let error = tools
        .send_keys(args(serde_json::json!({
            "pane": source,
            "text": format!("tmux wait-for -S {channel}"),
            "enter": true
        })))
        .await
        .err()
        .expect("an input-disabled configured peer refuses the whole input");
    assert_eq!(error.data.expect("typed refusal")["kind"], "invalid_input");
    assert_channel_quiet(guard.server(), channel).await;

    set_pane_input(guard.server(), &peer, true).await;
    assert!(!pane_handle(guard.server(), &peer).await.is_input_disabled());
    set_pane_input(guard.server(), &source, false).await;
    assert_paste_refused_unchanged(&tools, guard.server(), &source, "invalid_input").await;
    set_pane_input(guard.server(), &source, true).await;

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn pane_input_refuses_terminal_attention_but_not_control_clients() {
    let (guard, tools, source, peer) = synchronized_fixture("attended-input").await;
    let mut source_handle = pane_handle(guard.server(), &source).await;
    source_handle
        .select()
        .await
        .expect("source pane becomes active");
    source_handle
        .toggle_zoom()
        .await
        .expect("source window zooms");
    let terminal = attach_terminal_client(guard.server(), &source).await;

    let source_channel = "mcp-attended-source";
    let error = tools
        .send_keys(args(serde_json::json!({
            "pane": source,
            "text": format!("tmux wait-for -S {source_channel}"),
            "enter": true
        })))
        .await
        .err()
        .expect("the attended active pane refuses input");
    assert_eq!(error.data.expect("typed refusal")["kind"], "invalid_input");
    assert_channel_quiet(guard.server(), source_channel).await;
    assert_paste_refused_unchanged(&tools, guard.server(), &source, "invalid_input").await;

    let caller = caller_tools(guard.server(), &source).await;
    let error = caller
        .send_keys(args(serde_json::json!({"pane": source, "keys": ["C-l"]})))
        .await
        .err()
        .expect("caller protection still takes precedence");
    assert_self_protection(error, &source);

    pane_handle(guard.server(), &source)
        .await
        .set_option("synchronize-panes", "off")
        .await
        .expect("active source opts out");
    send_and_wait(
        &tools,
        guard.server(),
        &peer,
        "true",
        "mcp-zoom-hidden-peer",
    )
    .await;

    source_handle
        .toggle_zoom()
        .await
        .expect("source window unzooms");
    let peer_channel = "mcp-attended-visible-peer";
    let error = tools
        .send_keys(args(serde_json::json!({
            "pane": peer,
            "text": format!("tmux wait-for -S {peer_channel}"),
            "enter": true
        })))
        .await
        .err()
        .expect("every pane visible to a terminal client refuses input");
    assert_eq!(error.data.expect("typed refusal")["kind"], "invalid_input");
    assert_channel_quiet(guard.server(), peer_channel).await;

    detach_terminal_clients(guard.server()).await;
    drop(terminal);
    let control = libtmux::control::ControlMode::attach(
        guard.server(),
        pane_handle(guard.server(), &source).await.session_id(),
    )
    .await
    .expect("control-mode client attaches");
    tools
        .send_keys(args(serde_json::json!({"pane": source, "keys": ["C-l"]})))
        .await
        .expect("control-mode clients do not make a pane attended");
    control
        .shutdown()
        .await
        .expect("control-mode client shuts down");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn send_and_batch_use_one_dispatch_per_operation() {
    let logged = LoggedTmux::new();
    let guard = TestServer::builder()
        .tmux_executable(&logged.executable)
        .start()
        .await
        .expect("logging tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": "one-send-dispatch"})))
        .await
        .expect("session is created");
    let pane = panes(&tools).await[0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    prompt_ready(guard.server(), &pane).await;

    logged.clear();
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "true",
            "keys": ["C-l"],
            "enter": true
        })))
        .await
        .expect("the whole input operation succeeds");
    assert_eq!(
        logged.send_dispatches(),
        1,
        "one operation crosses one tmux process boundary"
    );

    logged.clear();
    let response = call_tool(
        tools,
        "send_keys_batch",
        serde_json::json!({
            "operations": [
                {"pane": pane, "text": "true", "keys": ["C-l"], "enter": true},
                {"pane": pane, "text": "true", "keys": ["C-l"], "enter": true}
            ],
            "on_error": "stop"
        }),
    )
    .await;
    let result = response
        .structured_content
        .expect("the batch has structured content");
    assert_eq!(result["succeeded"], 2, "{result}");
    assert_eq!(result["failed"], 0, "{result}");
    assert_eq!(
        logged.send_dispatches(),
        2,
        "each batch operation crosses one tmux process boundary"
    );

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
async fn pane_input_fails_closed_on_incomplete_or_inconsistent_caller_context() {
    let (guard, _, source, peer) = synchronized_fixture("input-caller-context").await;
    pane_handle(guard.server(), &source)
        .await
        .set_option("synchronize-panes", "off")
        .await
        .expect("source opts out");
    let socket = socket_of(guard.server()).await;
    let generation = guard
        .server()
        .generation()
        .await
        .expect("server generation");
    let session_id = pane_handle(guard.server(), &peer)
        .await
        .session_id()
        .to_string();
    let session = session_id
        .strip_prefix('$')
        .expect("tmux session ID has its canonical prefix");
    let wrong_pid = generation.pid().wrapping_add(1).max(1);
    let contexts = [
        ("missing TMUX", None, Some(peer.clone().into())),
        (
            "missing TMUX_PANE",
            Some(format!("{},{},{}", socket, generation.pid(), session).into()),
            None,
        ),
        (
            "truncated TMUX",
            Some(socket.clone().into()),
            Some(peer.clone().into()),
        ),
        (
            "wrong server pid",
            Some(format!("{socket},{wrong_pid},{session}").into()),
            Some(peer.clone().into()),
        ),
        (
            "wrong session",
            Some(format!("{socket},{},999999", generation.pid()).into()),
            Some(peer.clone().into()),
        ),
        (
            "missing claimed pane",
            Some(format!("{socket},{},{}", generation.pid(), session).into()),
            Some("%999999".into()),
        ),
    ];

    for (case, tmux, pane) in contexts {
        let caller = CallerIdentity::from_values(tmux, pane)
            .unwrap_or_else(|| panic!("{case} remains a non-detached caller context"));
        let tools = TmuxTools::builder(guard.server().clone())
            .caller(Some(caller))
            .build();
        let error = tools
            .paste_text(args(serde_json::json!({"pane": source, "text": ""})))
            .await
            .err()
            .unwrap_or_else(|| panic!("{case} must fail pane input closed"));
        assert_eq!(
            error.data.as_ref().expect("typed caller refusal")["kind"],
            "self_protection",
            "{case}: {error:?}"
        );
    }

    let foreign_guard = TestServer::builder()
        .start()
        .await
        .expect("foreign tmux starts");
    let foreign_pane = foreign_guard
        .server()
        .new_session("foreign-caller")
        .await
        .expect("foreign session starts")
        .panes()
        .await
        .expect("foreign panes list")
        .remove(0)
        .id()
        .to_string();
    let foreign = identity_for(foreign_guard.server(), &foreign_pane).await;
    let foreign_tools = TmuxTools::builder(guard.server().clone())
        .caller(Some(foreign))
        .build();
    foreign_tools
        .paste_text(args(serde_json::json!({"pane": source, "text": ""})))
        .await
        .expect("a complete caller on another socket is not selected");

    foreign_guard
        .shutdown()
        .await
        .expect("foreign tmux shuts down");
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
async fn paste_text_keeps_empty_input_buffer_free_and_cleans_late_refusal() {
    let (guard, tools, pane) = typing_fixture("paste-recheck").await;
    let buffers = guard.server().buffer_names().await.expect("buffers list");
    guard
        .server()
        .set_hook("after-set-buffer", format!("copy-mode -t {pane}"))
        .await
        .expect("late transition hook is installed");

    let empty = json(
        tools
            .paste_text(args(serde_json::json!({"pane": pane, "text": ""})))
            .await
            .expect("empty guarded paste is a no-op"),
    );
    assert_eq!(empty["bytes"], 0);
    assert_eq!(
        guard.server().buffer_names().await.expect("buffers list"),
        buffers
    );
    assert!(
        !pane_handle(guard.server(), &pane).await.is_in_mode(),
        "an empty paste never creates a buffer or fires its setup hook"
    );

    let screen = pane_screen(&tools, &pane).await;
    let error = tools
        .paste_text(args(serde_json::json!({"pane": pane, "text": "late"})))
        .await
        .err()
        .expect("a transition after setup refuses the paste");
    assert_eq!(error.data.expect("typed refusal")["kind"], "invalid_input");
    assert!(pane_handle(guard.server(), &pane).await.is_in_mode());
    assert_eq!(
        guard.server().buffer_names().await.expect("buffers list"),
        buffers
    );
    assert_eq!(pane_screen(&tools, &pane).await, screen);

    let pane_handle = pane_handle(guard.server(), &pane).await;
    pane_handle
        .exit_mode()
        .await
        .expect("fixture leaves copy mode");
    guard
        .server()
        .unset_hook("after-set-buffer")
        .await
        .expect("transition hook is removed");
    set_pane_input(guard.server(), &pane, false).await;
    let error = tools
        .paste_text(args(serde_json::json!({"pane": pane, "text": ""})))
        .await
        .err()
        .expect("even an empty paste performs its target guard");
    assert_eq!(error.data.expect("typed refusal")["kind"], "invalid_input");
    assert_eq!(
        guard.server().buffer_names().await.expect("buffers list"),
        buffers
    );
    set_pane_input(guard.server(), &pane, true).await;

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn paste_text_appends_enter_in_the_same_target_only_buffer() {
    let (guard, tools, source, peer) = synchronized_fixture("paste-enter").await;
    let buffers = guard.server().buffer_names().await.expect("buffers list");
    let peer_screen = pane_screen(&tools, &peer).await;
    let channel = "mcp-paste-enter";
    let command = format!("printf paste-enter-marker; tmux wait-for -S {channel}");

    let pasted = json(
        tools
            .paste_text(args(serde_json::json!({
                "pane": source,
                "text": command,
                "enter": true
            })))
            .await
            .expect("the pasted line is submitted"),
    );
    assert_eq!(pasted["bytes"], command.len());
    assert_eq!(
        guard
            .server()
            .wait_for_channel(channel, Duration::from_secs(2))
            .await
            .expect("pasted command signal is observed"),
        libtmux::ChannelWait::Signalled
    );
    assert!(
        pane_screen(&tools, &source)
            .await
            .contains("paste-enter-marker")
    );
    assert_eq!(pane_screen(&tools, &peer).await, peer_screen);
    assert_eq!(
        guard.server().buffer_names().await.expect("buffers list"),
        buffers
    );

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

    for (outcome, seconds, cancel) in [("cancelled", 60, true), ("deadline", 1, false)] {
        let cancelled = CancellationToken::new();
        let (request, release) = waiting_run(
            guard.server(),
            &tools,
            &pane,
            &format!("mcp-run-{outcome}"),
            seconds,
            cancelled.clone(),
        )
        .await;
        if cancel {
            cancelled.cancel();
        }
        let stopped = json(request.await.expect("request joins").expect("run answers"));
        assert_eq!(stopped["outcome"], outcome);
        assert_eq!(
            clients_settle(guard.server(), baseline_clients + 1).await,
            baseline_clients + 1,
            "{outcome} keeps the watcher until completion proof"
        );
        let refusal = tools
            .paste_text(args(serde_json::json!({"pane": pane, "text": ""})))
            .await
            .err()
            .expect("the interrupted command still reserves the pane");
        assert_active_run(&refusal, outcome);
        signal_channel(guard.server(), &release).await;
        assert_eq!(
            clients_settle(guard.server(), baseline_clients).await,
            baseline_clients,
            "{outcome} reaps the watcher before reservation release"
        );
        prompt_ready(guard.server(), &pane).await;
    }
    assert_eq!(run_view(&tools, &pane, "true").await["exit_status"], 0);
    assert!(
        !json(tools.list_panes().await.expect("server still answers"))["panes"]
            .as_array()
            .expect("pane list")
            .is_empty()
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn real_tmux_compat_dead_pane_settles_an_interrupted_run() {
    let (guard, tools, pane) = typing_fixture("run-dead-settlement").await;
    let baseline_clients = client_count(guard.server()).await;
    let configured = guard
        .server()
        .cmd(
            Command::new("set-option")
                .arg("-p")
                .arg("-t")
                .arg(&pane)
                .arg("remain-on-exit")
                .arg("on"),
        )
        .await
        .expect("remain-on-exit setting runs");
    assert!(configured.success(), "remain-on-exit is enabled");

    let stopped = json(
        tools
            .run_command(
                // The budget has to outlast the shell acknowledgement, not
                // just the command. `kill -KILL $$` leaves no completion
                // marker either way, so the deadline still fires; a budget
                // too tight for the handshake reports `no_shell` instead,
                // which is a true answer to a different question.
                args(serde_json::json!({
                    "pane": pane,
                    "command": "kill -KILL $$",
                    "seconds": 5
                })),
                CancellationToken::new(),
                tmux_mcp::Reporter::none(),
            )
            .await
            .expect("the interrupted run answers"),
    );
    // Both answers are true here and which one arrives is a race, measured
    // at about one run in ten against tmux 3.2a. `remain-on-exit` keeps the
    // pane, so the budget can expire first; the shell is gone, so the pane
    // has "stopped writing for good" and `pane_closed` is equally correct.
    // What this test is for is that an interrupted run settles rather than
    // hanging, and that the pane survives to be inspected -- both asserted
    // below. `no_shell` is the answer that would mean the budget never
    // covered the handshake, and it is still refused.
    let outcome = stopped["outcome"].as_str().expect("run outcome");
    assert!(
        matches!(outcome, "deadline" | "pane_closed"),
        "an interrupted run must settle, got {outcome}"
    );
    libtmux::test::retry_until(Duration::from_secs(3), async || {
        pane_handle(guard.server(), &pane).await.is_dead()
    })
    .await
    .expect("the retained pane reports its dead process");

    libtmux::test::retry_until(Duration::from_secs(3), async || {
        tools
            .paste_text(args(serde_json::json!({"pane": pane, "text": ""})))
            .await
            .err()
            .and_then(|error| error.data)
            .is_some_and(|data| data["kind"] == "invalid_input")
    })
    .await
    .expect("dead-pane proof releases the active-run reservation");
    assert_eq!(
        clients_settle(guard.server(), baseline_clients).await,
        baseline_clients,
        "dead-pane proof drops the stalled watcher"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn run_requires_a_known_posix_shell_before_watcher_setup() {
    let (guard, tools, pane) = typing_fixture("run-known-shell").await;
    let mut target = pane_handle(guard.server(), &pane).await;
    target
        .respawn(Some("exec cat"), true)
        .await
        .expect("the pane enters an input-reading non-shell");
    libtmux::test::retry_until(Duration::from_secs(2), async || {
        target.refresh().await.is_ok_and(|pane| {
            pane.current_command()
                .is_some_and(|value| value.as_bytes().ends_with(b"cat"))
        })
    })
    .await
    .expect("cat becomes the foreground program");
    let baseline_clients = client_count(guard.server()).await;

    let error = run_error(&tools, &pane, "printf should-not-run").await;

    assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        error.data.expect("typed shell refusal")["kind"],
        "invalid_input"
    );
    assert!(
        error.message.contains("POSIX-compatible"),
        "{}",
        error.message
    );
    assert_eq!(client_count(guard.server()).await, baseline_clients);
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn active_run_reservation_is_process_wide_and_guards_all_input() {
    let (guard, first, pane) = typing_fixture("run-reservation").await;
    let second = bare_tools(guard.server());
    let (running, release) = waiting_run(
        guard.server(),
        &first,
        &pane,
        "mcp-run-reservation",
        10,
        CancellationToken::new(),
    )
    .await;

    let sent = second
        .send_keys(args(serde_json::json!({"pane": pane, "text": ""})))
        .await;
    let pasted = second
        .paste_text(args(serde_json::json!({"pane": pane, "text": ""})))
        .await;
    let overlapping = second
        .run_command(
            args(serde_json::json!({"pane": pane, "command": "true", "seconds": 1})),
            CancellationToken::new(),
            tmux_mcp::Reporter::none(),
        )
        .await;

    signal_channel(guard.server(), &release).await;
    let first_view = json(
        running
            .await
            .expect("first request joins")
            .expect("first run answers"),
    );
    assert_eq!(first_view["exit_status"], 0);

    for (operation, result) in [
        ("send", sent.map(|_| ())),
        ("paste", pasted.map(|_| ())),
        ("run", overlapping.map(|_| ())),
    ] {
        assert_active_run(
            &result.expect_err("active run guards every pane-input route"),
            operation,
        );
    }

    prompt_ready(guard.server(), &pane).await;
    assert_eq!(run_view(&second, &pane, "true").await["exit_status"], 0);
    guard.shutdown().await.expect("tmux fixture shuts down");
}

async fn assert_input_reservation_blocks_run(operation: GuardedDispatch) {
    let logged = LoggedTmux::new();
    let guard = TestServer::builder()
        .tmux_executable(&logged.executable)
        .start()
        .await
        .expect("logging tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({
            "name": format!("{}-run-race", operation.name())
        })))
        .await
        .expect("session is created");
    let pane = panes(&tools).await[0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    prompt_ready(guard.server(), &pane).await;
    let mut barrier = logged.hold_next(operation.tmux_command());
    let run_started = format!("mcp-{}-race-run-started", operation.name());
    let release_run = format!("mcp-{}-race-run-release", operation.name());

    let input = operation.start(tools.clone(), pane.clone());
    barrier.wait().await;
    let running = tokio::spawn({
        let tools = tools.clone();
        let pane = pane.clone();
        let run_started = run_started.clone();
        let release_run = release_run.clone();
        async move {
            tools
                .run_command(
                    args(serde_json::json!({
                        "pane": pane,
                        "command": format!(
                            "tmux wait-for -S {run_started}; tmux wait-for {release_run}"
                        ),
                        "seconds": 10
                    })),
                    CancellationToken::new(),
                    tmux_mcp::Reporter::none(),
                )
                .await
        }
    });
    let raced = guard
        .server()
        .wait_for_channel(&run_started, Duration::from_millis(500))
        .await
        .expect("run-start channel wait answers")
        == libtmux::ChannelWait::Signalled;

    barrier.release();
    signal_channel(guard.server(), &release_run).await;
    let input = input.await.expect("input request joins");
    let run = running.await.expect("run request joins");
    input.expect("the reserved input completes");

    assert!(
        !raced,
        "the racing run reached the {}-owned pane",
        operation.name()
    );
    assert_active_run(
        &run.err()
            .expect("the input reservation refuses the racing run"),
        &format!("run racing a {}", operation.name()),
    );
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn send_reservation_blocks_a_run_until_dispatch() {
    assert_input_reservation_blocks_run(GuardedDispatch::Send).await;
}

#[tokio::test]
async fn paste_reservation_blocks_a_run_until_dispatch() {
    assert_input_reservation_blocks_run(GuardedDispatch::Paste).await;
}

#[tokio::test]
async fn run_uses_exactly_two_complete_input_checkpoints() {
    let logged = LoggedTmux::new();
    let guard = TestServer::builder()
        .tmux_executable(&logged.executable)
        .start()
        .await
        .expect("logging tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": "two-run-checkpoints"})))
        .await
        .expect("session is created");
    let pane = panes(&tools).await[0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    prompt_ready(guard.server(), &pane).await;
    logged.clear();

    assert_eq!(run_view(&tools, &pane, "true").await["exit_status"], 0);

    assert_eq!(
        logged.command_dispatches("list-clients"),
        2,
        "the operation performs exactly two complete attention checkpoints"
    );
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn uncertain_dispatch_keeps_the_run_reserved_until_proven() {
    let (guard, normal, pane) = typing_fixture("run-uncertain-reservation").await;
    let accepted = "mcp-run-uncertain-accepted";
    let acknowledge = "mcp-run-uncertain-acknowledge";
    let started = "mcp-run-uncertain-started";
    let release = "mcp-run-uncertain-release";
    guard
        .server()
        .set_hook(
            "after-send-keys",
            format!(
                "if-shell -F '#{{==:#{{hook_flag_l}},1}}' \
                 'wait-for -S {accepted}; wait-for {acknowledge}'"
            ),
        )
        .await
        .expect("the dispatch acknowledgement is held");
    let executable = guard
        .server()
        .resolved_tmux_executable()
        .expect("fixture tmux resolves");
    let short = Server::builder()
        .tmux_executable(executable)
        .socket_path(guard.server().socket_path())
        .default_timeout(Duration::from_millis(500))
        .build()
        .expect("short-timeout route builds");
    let short_tools = bare_tools(&short);
    let baseline_clients = client_count(guard.server()).await;

    let error = short_tools
        .run_command(
            args(serde_json::json!({
                "pane": pane,
                "command": format!(
                    "tmux wait-for -S {started}; tmux wait-for {release}"
                ),
                "seconds": 5
            })),
            CancellationToken::new(),
            tmux_mcp::Reporter::none(),
        )
        .await
        .err()
        .expect("the accepted send times out before acknowledgement");
    assert_eq!(
        error.data.expect("typed uncertain dispatch")["kind"],
        "dispatch_unknown"
    );
    await_channel(guard.server(), accepted).await;
    signal_channel(guard.server(), acknowledge).await;
    await_channel(guard.server(), started).await;
    let refusal = normal
        .paste_text(args(serde_json::json!({"pane": pane, "text": ""})))
        .await
        .err()
        .expect("uncertain delivery reserves the pane");
    assert_active_run(&refusal, "paste after uncertain dispatch");

    signal_channel(guard.server(), release).await;
    libtmux::test::retry_until(Duration::from_secs(3), async || {
        normal
            .paste_text(args(serde_json::json!({"pane": pane, "text": ""})))
            .await
            .is_ok()
    })
    .await
    .expect("the completion proof releases the pane");
    assert_eq!(client_count(guard.server()).await, baseline_clients);
    guard
        .server()
        .unset_hook("after-send-keys")
        .await
        .expect("the dispatch hook is removed");
    drop(short_tools);
    short
        .shutdown()
        .await
        .expect("short-timeout route shuts down");
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
            error.data.as_ref().expect("typed refusal")["kind"],
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
    for transition in ["cohort", "mode", "shell", "placement"] {
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
            "shell" => format!("respawn-pane -k -t {source} 'exec cat'"),
            "placement" => format!(
                "move-window -s {} -t {}:7",
                source_handle.window_id(),
                source_handle.session_id()
            ),
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
            error.data.as_ref().expect("typed refusal")["kind"],
            "invalid_input",
            "{transition}"
        );
        if transition == "shell" {
            assert!(error.message.contains("POSIX-compatible"), "{error}");
        }
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

/// The raw bytes here cannot be a filename on macOS.
///
/// APFS and HFS+ reject a name that is not valid UTF-8, so the fixture fails
/// with `EILSEQ` before the transport is reached. A control byte would be
/// portable but would change the assertion: those are exactly what
/// `pane_input_rejects_terminal_control_in_the_socket_route` requires the
/// route to refuse, and this test requires it to succeed. So the case needs
/// a byte that is neither ASCII-control nor valid UTF-8, which is the one
/// macOS will not store.
#[cfg(not(target_os = "macos"))]
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
async fn pane_input_rejects_terminal_control_in_the_socket_route() {
    let mut files = RawServerFiles::create(b"tmux", b"socket-\x03");
    let (bootstrap, pane) = files.start("input-route-control").await;
    let bootstrap_tools = bare_tools(&bootstrap);
    let before = pane_screen(&bootstrap_tools, &pane).await;

    let error = bare_tools(&files.route())
        .paste_text(args(serde_json::json!({
            "pane": pane,
            "text": "blocked-route"
        })))
        .await
        .err()
        .expect("pane input rejects a terminal-control socket");

    assert_eq!(error.data.expect("typed refusal")["kind"], "decode");
    assert_eq!(pane_screen(&bootstrap_tools, &pane).await, before);
    assert!(
        bootstrap
            .buffer_names()
            .await
            .expect("buffers list")
            .is_empty()
    );
    files.shutdown(&bootstrap).await;
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
#[allow(
    clippy::too_many_lines,
    reason = "one shell must retain traps and options across every command outcome"
)]
async fn run_frame_preserves_inherited_error_and_debug_traps() {
    for (shell, flags) in [("/bin/bash", "--noprofile --norc"), ("/bin/zsh", "-f")] {
        if !std::path::Path::new(shell).is_file() {
            continue;
        }
        let shell_name = shell.rsplit('/').next().expect("shell basename");
        let (guard, tools, pane) = typing_fixture(&format!("run-frame-traps-{shell_name}")).await;
        pane_handle(guard.server(), &pane)
            .await
            .respawn(Some(&format!("exec {shell} {flags}")), true)
            .await
            .expect("fixture pane changes shell");
        libtmux::test::retry_until(Duration::from_secs(2), async || {
            pane_handle(guard.server(), &pane)
                .await
                .current_command()
                .is_some_and(|command| command.as_bytes() == shell_name.as_bytes())
        })
        .await
        .unwrap_or_else(|_| panic!("{shell_name} becomes the foreground shell"));
        prompt_ready(guard.server(), &pane).await;

        let debug_action = r#"
if ( : >&8 ) 2>/dev/null; then
    /usr/bin/printf 'MCP-CAPTURE-DEBUG-OUT\n'
    /usr/bin/printf 'MCP-CAPTURE-DEBUG-ERR\n' >&2
fi
/usr/bin/printf 'MCP-DEBUG-OUT:%s:"quoted"\n' "$MCP_TRAP_PHASE"
/usr/bin/printf "MCP-DEBUG-ERR:%s:'quoted'\n" "$MCP_TRAP_PHASE" >&2
"#;
        let error_action = r#"
/usr/bin/printf 'MCP-ERR-OUT:%s:"quoted"\n' "$MCP_TRAP_PHASE"
/usr/bin/printf "MCP-ERR-ERR:%s:'quoted'\n" "$MCP_TRAP_PHASE" >&2
"#;
        let exit_channel = format!("mcp-frame-traps-{shell_name}-exit");
        let exit_action = format!(
            "/usr/bin/printf 'MCP-PARENT-EXIT:{shell_name}\\n'; tmux wait-for -S {exit_channel}"
        );
        send_and_wait(
            &tools,
            guard.server(),
            &pane,
            &format!(
                "MCP_TRAP_PHASE=run; trap {} DEBUG; trap {} ERR; trap {} EXIT; set -e; set -x; set -f",
                shell_quote(OsStr::new(debug_action)),
                shell_quote(OsStr::new(error_action)),
                shell_quote(OsStr::new(&exit_action)),
            ),
            &format!("mcp-frame-traps-{shell_name}-ready"),
        )
        .await;

        for (case, command, expected_status) in [
            (
                "success",
                "if ( : >&8 ) 2>/dev/null || ( : <&9 ) 2>/dev/null; then /usr/bin/printf 'MCP-FD-LEAK\\n'; fi; /usr/bin/printf 'MCP-COMMAND-SUCCESS\\n'",
                Some(0),
            ),
            (
                "failure",
                "false; /usr/bin/printf 'MCP-UNREACHABLE\\n'",
                None,
            ),
            ("syntax", "if then", None),
            ("exit", "exit 23", Some(23)),
        ] {
            let result = run_view(&tools, &pane, command).await;
            let output = result["output"].as_str().expect("run output");
            if let Some(status) = expected_status {
                assert_eq!(result["exit_status"], status, "{shell_name}/{case}");
            } else {
                assert_ne!(result["exit_status"], 0, "{shell_name}/{case}");
            }
            if case == "success" {
                for marker in [
                    "MCP-DEBUG-OUT:run:\"quoted\"",
                    "MCP-DEBUG-ERR:run:'quoted'",
                    "MCP-COMMAND-SUCCESS",
                ] {
                    assert!(
                        output.contains(marker),
                        "{shell_name}: {marker}: {output:?}"
                    );
                }
                assert!(!output.contains("MCP-FD-LEAK"), "{shell_name}: {output:?}");
            }
            if case == "failure" {
                for marker in ["MCP-ERR-OUT:run:\"quoted\"", "MCP-ERR-ERR:run:'quoted'"] {
                    assert!(
                        output.contains(marker),
                        "{shell_name}: {marker}: {output:?}"
                    );
                }
                assert!(
                    !output.contains("MCP-UNREACHABLE"),
                    "{shell_name}: {output:?}"
                );
            }
            assert!(
                !output.contains("MCP-PARENT-EXIT"),
                "{shell_name}/{case}: parent EXIT leaked into child: {output:?}"
            );
            for capture_only in [
                "MCP-CAPTURE-DEBUG-OUT",
                "MCP-CAPTURE-DEBUG-ERR",
                "libtmux-mcp-traps-",
            ] {
                assert!(
                    !output.contains(capture_only),
                    "{shell_name}/{case}: capture output leaked: {output:?}"
                );
            }
            assert_channel_quiet(guard.server(), &exit_channel).await;

            let flags = run_view(
                &tools,
                &pane,
                "case $- in *e*) :;; *) exit 90;; esac; case $- in *x*) :;; *) exit 91;; esac; case $- in *f*) :;; *) exit 92;; esac",
            )
            .await;
            assert_eq!(flags["exit_status"], 0, "{shell_name}/{case}: {flags}");
        }

        let opened = json(
            tools
                .capture_since(args(serde_json::json!({"pane": pane})))
                .await
                .expect("parent-trap tail opens"),
        );
        let cursor = opened["cursor"].as_str().expect("tail cursor").to_owned();
        send_and_wait(
            &tools,
            guard.server(),
            &pane,
            "MCP_TRAP_PHASE=parent; set +e; false; set -e",
            &format!("mcp-frame-traps-{shell_name}-parent"),
        )
        .await;
        let parent = json(
            tools
                .capture_since(args(serde_json::json!({"pane": pane, "cursor": cursor})))
                .await
                .expect("parent-trap tail reads"),
        );
        let parent = parent["text"].as_str().expect("parent-trap text");
        for marker in [
            "MCP-DEBUG-OUT:parent:\"quoted\"",
            "MCP-DEBUG-ERR:parent:'quoted'",
            "MCP-ERR-OUT:parent:\"quoted\"",
            "MCP-ERR-ERR:parent:'quoted'",
        ] {
            assert!(
                parent.contains(marker),
                "{shell_name}: parent trap lost {marker}: {parent:?}"
            );
        }
        tools
            .send_keys(args(serde_json::json!({
                "pane": pane,
                "text": "exit",
                "enter": true
            })))
            .await
            .expect("the parent shell exits");
        await_channel(guard.server(), &exit_channel).await;
        guard.shutdown().await.expect("tmux fixture shuts down");
    }
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
async fn mcp_flag_shaped_metadata_operands_stay_literal() {
    let (guard, tools) = fixture("literal-operands").await;
    let session = guard
        .server()
        .sessions()
        .await
        .expect("sessions list")
        .remove(0);
    let window = session
        .active_window()
        .await
        .expect("active window resolves")
        .expect("a window exists");

    let renamed_session = call_tool(
        tools.clone(),
        "rename_session",
        serde_json::json!({
            "session": "literal-operands",
            "name": "-mcp-session"
        }),
    )
    .await
    .structured_content
    .expect("flag-shaped session name stays literal");
    assert_eq!(renamed_session["name"], "-mcp-session");

    let renamed_window = call_tool(
        tools.clone(),
        "rename_window",
        serde_json::json!({
            "window": window.id().to_string(),
            "name": "-mcp-window"
        }),
    )
    .await
    .structured_content
    .expect("flag-shaped window name stays literal");
    assert_eq!(renamed_window["name"], "-mcp-window");

    let Err(layout_error) = tools
        .select_layout(args(serde_json::json!({
            "window": window.id().to_string(),
            "layout": "-E"
        })))
        .await
    else {
        panic!("a flag-shaped invalid layout was executed as an option");
    };
    assert!(
        !layout_error.message.contains("unknown flag"),
        "the layout reached tmux as an operand: {layout_error:?}"
    );

    tools
        .signal_channel(args(serde_json::json!({"channel": "-mcp-channel"})))
        .await
        .expect("flag-shaped MCP channel signals");
    let waited = json(
        tools
            .wait_for_channel(args(serde_json::json!({
                "channel": "-mcp-channel",
                "seconds": 5
            })))
            .await
            .expect("flag-shaped MCP channel waits"),
    );
    assert_eq!(waited["outcome"], "signalled");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn mcp_flag_shaped_input_operands_stay_literal() {
    let (guard, tools, pane) = typing_fixture("literal-input").await;

    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "-mcp-text",
            "enter": true
        })))
        .await
        .expect("flag-shaped MCP text stays literal");
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "keys": ["-mcp-key"],
            "enter": true
        })))
        .await
        .expect("flag-shaped MCP key names stay operands");
    tools
        .paste_text(args(serde_json::json!({
            "pane": pane,
            "text": "-mcp-paste\n"
        })))
        .await
        .expect("flag-shaped MCP paste text stays literal");

    let batch = call_tool(
        tools.clone(),
        "send_keys_batch",
        serde_json::json!({
            "operations": [{
                "pane": pane,
                "text": "-mcp-batch",
                "enter": true
            }]
        }),
    )
    .await;
    let batch = batch
        .structured_content
        .expect("batch has structured content");
    assert_eq!(batch["succeeded"], 1, "{batch}");
    assert_eq!(batch["failed"], 0, "{batch}");

    libtmux::test::retry_until(Duration::from_secs(5), async || {
        let screen = pane_screen(&tools, &pane).await;
        ["-mcp-text", "-mcp-key", "-mcp-paste", "-mcp-batch"]
            .iter()
            .all(|needle| screen.contains(needle))
    })
    .await
    .expect("all MCP literal input reaches the pane");

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
