#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used
)]

use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process, test_kill_process};

use super::actor::EVENT_QUEUE;
use super::{ControlMode, Event, PaneOutput};
use crate::internal::core::{BuildContext, Core, CoreConfiguration, SocketSelection};
use crate::{Command, ControlModeErrorKind, Error, ErrorKind, Server, SessionId, TmuxText};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(1);

/// How long a reply that never arrives is waited for.
///
/// Set on the sender rather than the server: attaching forks a process, and a
/// deadline short enough to fire on a missing reply does not bound that.
const REPLY_TIMEOUT: Duration = Duration::from_millis(100);

/// How long the opening handshake gets when its timing out is the assertion.
///
/// The fixture sends no opening block, so the handshake times out whatever
/// this is; it only has to outlast the fork. At 100 ms a loaded runner reaped
/// the process group before the shell published the PIDs the test then
/// asserts were reaped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(1);

fn fixture_root() -> PathBuf {
    let root = PathBuf::from("/tmp/libtmux-rs-test");
    fs::create_dir_all(&root).expect("fixture root is creatable");
    root
}

fn directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("control-owned-")
        .tempdir_in(fixture_root())
        .expect("fixture directory is creatable")
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn write_script(directory: &Path, body: &str) -> PathBuf {
    let path = directory.join("fake-tmux");
    let staging = directory.join(format!(".fake-tmux.{}.tmp", process::id()));
    let mut file = fs::File::create(&staging).expect("staged script is creatable");
    writeln!(
        file,
        "#!/bin/sh\nif [ \"${{1-}}\" = \"-V\" ]; then\n    printf 'tmux 3.5a\\n'\n    exit 0\nfi\nset -eu\n{body}"
    )
    .expect("script is writable");
    file.sync_all().expect("script contents are durable");
    drop(file);
    let mut permissions = fs::metadata(&staging)
        .expect("script metadata is readable")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&staging, permissions).expect("staged script is executable");
    fs::rename(&staging, &path).expect("staged script is installed atomically");

    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match process::Command::new(&path)
            .arg("-V")
            .stdout(process::Stdio::null())
            .status()
        {
            Ok(status) => {
                assert!(status.success(), "script readiness probe succeeds");
                break;
            }
            Err(source) if source.raw_os_error() == Some(Errno::TXTBSY.raw_os_error()) => {
                assert!(
                    Instant::now() < deadline,
                    "script remains busy past the readiness deadline"
                );
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(source) => panic!("script readiness probe failed: {source}"),
        }
    }
    path
}

fn basic_server(directory: &Path, executable: PathBuf, timeout: Duration) -> Server {
    server(
        directory,
        executable.into_os_string(),
        OsString::from("/usr/bin:/bin"),
        timeout,
        None,
        None,
    )
}

fn server(
    directory: &Path,
    executable: OsString,
    captured_path: OsString,
    timeout: Duration,
    config_file: Option<PathBuf>,
    colors: Option<u16>,
) -> Server {
    let socket = directory.join("socket;");
    let context = BuildContext::new(
        Some(directory.to_path_buf()),
        Some(captured_path),
        Some(OsString::from("inherited-tmux")),
        Some(OsString::from("%41")),
        None,
        Some(PathBuf::from("/tmp")),
        rustix::process::getuid().as_raw(),
    );
    let configuration = CoreConfiguration::resolve(
        &SocketSelection::Path(socket),
        config_file,
        colors,
        executable,
        timeout,
        context,
    )
    .expect("fake server configuration resolves");
    Server::from_core(Arc::new(Core::new(configuration)))
}

fn session() -> SessionId {
    "$1".parse().expect("fixture session id parses")
}

fn pid(value: u32) -> Pid {
    Pid::from_raw(i32::try_from(value).expect("test PID fits i32")).expect("test PID is nonzero")
}

fn read_pid(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

async fn wait_for_pid(path: &Path) -> u32 {
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if let Some(value) = read_pid(path) {
                return value;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("child publishes its PID before the test deadline")
}

fn process_exists(value: u32) -> bool {
    !matches!(test_kill_process(pid(value)), Err(Errno::SRCH))
}

async fn assert_process_gone(value: u32) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        while process_exists(value) {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("process disappears before the test deadline");
}

struct ProcessGuard {
    paths: Vec<PathBuf>,
}

impl ProcessGuard {
    fn new(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            paths: paths.into_iter().collect(),
        }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        for value in self.paths.iter().filter_map(|path| read_pid(path)) {
            let _ = kill_process(pid(value), Signal::KILL);
        }
    }
}

fn process_script(parent: &Path, descendant: &Path, prefix: &str) -> String {
    format!(
        "printf '%s\\n' \"$$\" > {parent}\n/bin/sleep 86400 &\nprintf '%s\\n' \"$!\" > {descendant}\n{prefix}\nwait",
        parent = shell_quote(parent),
        descendant = shell_quote(descendant),
    )
}

fn opening_success() -> &'static str {
    "printf '%%begin 0 1 0\\n%%end 0 1 0\\n'"
}

async fn attach(server: &Server) -> Result<ControlMode, Error> {
    ControlMode::attach(server, &session()).await
}

#[tokio::test]
async fn attach_uses_the_cores_captured_launch_context() {
    let fixture = directory();
    let record = fixture.path().join("context");
    let executable = write_script(
        fixture.path(),
        &format!(
            "{{\n    pwd\n    printf '%s\\n' \"$PATH\"\n    for argument in \"$@\"; do printf '<%s>\\n' \"$argument\"; done\n}} > {}\n{}",
            shell_quote(&record),
            opening_success(),
        ),
    );
    let server = server(
        fixture.path(),
        executable.into_os_string(),
        OsString::from("/captured/path"),
        Duration::from_secs(1),
        Some(PathBuf::from("config;")),
        Some(256),
    );

    let control = attach(&server).await.expect("control mode attaches");
    let captured = fs::read_to_string(&record).expect("launch context is recorded");
    let physical_directory = fixture
        .path()
        .canonicalize()
        .expect("fixture directory resolves");
    let expected = format!(
        "{}\n/captured/path\n<-S>\n<{}>\n<-f>\n<{}>\n<-2>\n<-u>\n<-C>\n<attach>\n<-E>\n<-t>\n<$1>\n",
        physical_directory.display(),
        fixture.path().join("socket;").display(),
        fixture.path().join("config;").display(),
    );
    assert_eq!(captured, expected);

    control.shutdown().await.expect("control mode shuts down");
    server.shutdown().await.expect("server shuts down");
}

#[tokio::test]
async fn opening_handshake_times_out_and_reaps_the_process_group() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    let executable = write_script(fixture.path(), &process_script(&parent, &descendant, ""));
    let server = basic_server(fixture.path(), executable, HANDSHAKE_TIMEOUT);

    let error = tokio::time::timeout(TEST_TIMEOUT, attach(&server))
        .await
        .expect("the configured deadline ends attach")
        .expect_err("an absent opening block times out");
    assert_eq!(error.kind(), ErrorKind::Timeout);
    let parent_pid = wait_for_pid(&parent).await;
    let descendant_pid = wait_for_pid(&descendant).await;
    assert_process_gone(parent_pid).await;
    assert_process_gone(descendant_pid).await;

    server.shutdown().await.expect("server shuts down");
}

#[tokio::test]
async fn a_short_reply_deadline_does_not_bound_attaching() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    // The opening block lands well after the reply deadline would have fired,
    // so an attach still spending that deadline could not finish.
    let prefix = format!("/bin/sleep 0.3\n{}", opening_success());
    let executable = write_script(
        fixture.path(),
        &process_script(&parent, &descendant, &prefix),
    );
    let server = basic_server(fixture.path(), executable, TEST_TIMEOUT);
    let (commands, events) = attach(&server)
        .await
        .expect("attaching outlasts the reply deadline")
        .reply_timeout(REPLY_TIMEOUT)
        .split();

    let error = commands
        .send(Command::new("display-message"))
        .await
        .expect_err("the command keeps the shorter deadline");
    assert_eq!(error.kind(), ErrorKind::Timeout);
    let shutdown = events
        .shutdown()
        .await
        .expect_err("the actor reports timeout");
    assert_eq!(shutdown.kind(), ErrorKind::Timeout);

    server.shutdown().await.expect("server shuts down");
}

#[tokio::test]
async fn cancelling_attach_reaps_the_process_group() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    let executable = write_script(fixture.path(), &process_script(&parent, &descendant, ""));
    let server = basic_server(fixture.path(), executable, Duration::from_secs(30));
    let attached = server.clone();
    let task = tokio::spawn(async move { attach(&attached).await });
    let parent_pid = wait_for_pid(&parent).await;
    let descendant_pid = wait_for_pid(&descendant).await;

    task.abort();
    let _ = task.await;
    assert_process_gone(parent_pid).await;
    assert_process_gone(descendant_pid).await;

    server.shutdown().await.expect("server shuts down");
}

#[tokio::test]
async fn dropping_both_halves_reaps_the_process_group() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    let executable = write_script(
        fixture.path(),
        &process_script(&parent, &descendant, opening_success()),
    );
    let server = basic_server(fixture.path(), executable, Duration::from_secs(30));
    let control = attach(&server).await.expect("control mode attaches");
    let parent_pid = wait_for_pid(&parent).await;
    let descendant_pid = wait_for_pid(&descendant).await;

    drop(control);
    assert_process_gone(parent_pid).await;
    assert_process_gone(descendant_pid).await;

    server.shutdown().await.expect("server shuts down");
}

#[tokio::test]
async fn an_open_response_block_has_one_deadline() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let marker = fixture.path().join("command-started");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    let prefix = format!(
        "{}\nIFS= read -r _line\n: > {}\nprintf '%%begin 0 2 0\\n'",
        opening_success(),
        shell_quote(&marker),
    );
    let executable = write_script(
        fixture.path(),
        &process_script(&parent, &descendant, &prefix),
    );
    let server = basic_server(fixture.path(), executable, TEST_TIMEOUT);
    let (commands, events) = attach(&server)
        .await
        .expect("control mode attaches")
        .reply_timeout(REPLY_TIMEOUT)
        .split();

    let error = tokio::time::timeout(
        Duration::from_secs(2),
        commands.send(Command::new("display-message")),
    )
    .await
    .expect("the open block reaches its deadline")
    .expect_err("the open block times out");
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(
        !error.is_transient(),
        "a response timeout terminates this connection",
    );
    assert!(marker.exists(), "the command reached the fake client");
    let closed = commands
        .send(Command::new("display-message"))
        .await
        .expect_err("the timed out sender remains closed");
    assert!(
        !closed.is_transient(),
        "waiting cannot reopen the timed out sender",
    );
    let shutdown = events
        .shutdown()
        .await
        .expect_err("the actor reports timeout");
    assert_eq!(shutdown.kind(), ErrorKind::Timeout);
    assert_process_gone(wait_for_pid(&parent).await).await;
    assert_process_gone(wait_for_pid(&descendant).await).await;

    let replacement = attach(&server)
        .await
        .expect("a new connection can attach through the same server");
    replacement
        .shutdown()
        .await
        .expect("the replacement connection shuts down");
    server.shutdown().await.expect("server shuts down");
}

#[tokio::test]
async fn a_reply_deadline_starts_before_begin() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let marker = fixture.path().join("command-started");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    let prefix = format!(
        "{}\nIFS= read -r _line\n: > {}",
        opening_success(),
        shell_quote(&marker),
    );
    let executable = write_script(
        fixture.path(),
        &process_script(&parent, &descendant, &prefix),
    );
    let server = basic_server(fixture.path(), executable, TEST_TIMEOUT);
    let (commands, events) = attach(&server)
        .await
        .expect("control mode attaches")
        .reply_timeout(REPLY_TIMEOUT)
        .split();

    let error = tokio::time::timeout(
        Duration::from_secs(2),
        commands.send(Command::new("display-message")),
    )
    .await
    .expect("the reply deadline does not wait for begin")
    .expect_err("a missing begin times out");
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(marker.exists(), "the command reached the fake client");
    let shutdown = events
        .shutdown()
        .await
        .expect_err("the actor reports timeout");
    assert_eq!(shutdown.kind(), ErrorKind::Timeout);

    server.shutdown().await.expect("server shuts down");
}

#[tokio::test]
async fn the_earliest_committed_deadline_ends_the_connection() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let first_seen = fixture.path().join("first-command");
    let second_seen = fixture.path().join("second-command");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    let prefix = format!(
        "{}\nIFS= read -r _first\n: > {}\nIFS= read -r _second\n: > {}",
        opening_success(),
        shell_quote(&first_seen),
        shell_quote(&second_seen),
    );
    let executable = write_script(
        fixture.path(),
        &process_script(&parent, &descendant, &prefix),
    );
    let server = basic_server(fixture.path(), executable, Duration::from_secs(2));
    let (later, events) = attach(&server)
        .await
        .expect("control mode attaches")
        .reply_timeout(Duration::from_secs(2))
        .split();
    let earlier = later.clone().reply_timeout(REPLY_TIMEOUT);

    let first = tokio::spawn(async move { later.send(Command::new("list-sessions")).await });
    tokio::time::timeout(TEST_TIMEOUT, async {
        while !first_seen.exists() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("the first command reaches the client");

    let second = tokio::spawn(async move { earlier.send(Command::new("list-windows")).await });
    tokio::time::timeout(TEST_TIMEOUT, async {
        while !second_seen.exists() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("the second command reaches the client");

    let second_error = tokio::time::timeout(Duration::from_secs(1), second)
        .await
        .expect("the earlier deadline ends the actor")
        .expect("the second sender task joins")
        .expect_err("the second command times out");
    assert_eq!(second_error.kind(), ErrorKind::Timeout);
    assert!(matches!(
        &second_error,
        Error::ControlMode {
            kind: ControlModeErrorKind::TimedOut,
            ..
        }
    ));
    assert!(!second_error.is_transient());
    let first_error = first
        .await
        .expect("the first sender task joins")
        .expect_err("the shared connection ends");
    assert_eq!(first_error.kind(), ErrorKind::Timeout);
    let shutdown = events
        .shutdown()
        .await
        .expect_err("the actor reports its deadline");
    assert_eq!(shutdown.kind(), ErrorKind::Timeout);
    assert_process_gone(wait_for_pid(&parent).await).await;
    assert_process_gone(wait_for_pid(&descendant).await).await;

    server.shutdown().await.expect("server shuts down");
}

#[tokio::test]
async fn a_blocked_write_uses_the_reply_deadline() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    let executable = write_script(
        fixture.path(),
        &process_script(&parent, &descendant, opening_success()),
    );
    let server = basic_server(fixture.path(), executable, TEST_TIMEOUT);
    let (commands, events) = attach(&server)
        .await
        .expect("control mode attaches")
        .reply_timeout(REPLY_TIMEOUT)
        .split();

    let error = tokio::time::timeout(
        Duration::from_secs(2),
        // Sized between two bounds rather than to be asserted on: past the
        // pipe buffer, and short of the 2 MiB that spends this deadline being
        // serialized before the request can commit.
        commands.send(Command::new("display-message").arg("x".repeat(256 * 1024))),
    )
    .await
    .expect("the blocked write reaches its deadline")
    .expect_err("the blocked write times out");
    assert_eq!(error.kind(), ErrorKind::Timeout);
    let shutdown = events
        .shutdown()
        .await
        .expect_err("the actor reports timeout");
    assert_eq!(shutdown.kind(), ErrorKind::Timeout);

    server.shutdown().await.expect("server shuts down");
}

#[tokio::test]
async fn watcher_shutdown_interrupts_an_open_response_block() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let marker = fixture.path().join("command-started");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    let prefix = format!(
        "{}\nIFS= read -r _line\n: > {}\nprintf '%%begin 0 2 0\\n'",
        opening_success(),
        shell_quote(&marker),
    );
    let executable = write_script(
        fixture.path(),
        &process_script(&parent, &descendant, &prefix),
    );
    let server = basic_server(fixture.path(), executable, Duration::from_secs(30));
    let (commands, events) = attach(&server)
        .await
        .expect("control mode attaches")
        .split();
    let sender = commands.clone();
    let sending = tokio::spawn(async move { sender.send(Command::new("display-message")).await });
    tokio::time::timeout(TEST_TIMEOUT, async {
        while !marker.exists() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("the fake client opens the response block");

    tokio::time::timeout(Duration::from_secs(2), events.shutdown())
        .await
        .expect("watcher shutdown interrupts the read")
        .expect("explicit shutdown is clean");
    let send_error = sending
        .await
        .expect("sender task joins")
        .expect_err("the interrupted command closes");
    assert_eq!(send_error.kind(), ErrorKind::Transport);
    assert!(
        !send_error.is_transient(),
        "the stopped sender cannot reopen its connection",
    );
    let same_sender = commands
        .send(Command::new("display-message"))
        .await
        .expect_err("the same sender remains closed");
    assert!(
        !same_sender.is_transient(),
        "waiting cannot reopen a closed sender",
    );
    assert_process_gone(wait_for_pid(&parent).await).await;
    assert_process_gone(wait_for_pid(&descendant).await).await;

    drop(commands);
    let replacement = attach(&server)
        .await
        .expect("a new connection can attach through the same server");
    replacement
        .shutdown()
        .await
        .expect("the replacement connection shuts down");
    server.shutdown().await.expect("server shuts down");
}

#[tokio::test]
async fn opening_notifications_stop_at_the_receiver_bound() {
    let fixture = directory();
    let prelude = fixture.path().join("prelude");
    let marker = fixture.path().join("prelude-drained");
    fs::write(&prelude, "%sessions-changed\n".repeat(200_000)).expect("large prelude is writable");
    let executable = write_script(
        fixture.path(),
        &format!(
            "/bin/cat {}\n: > {}\n{}",
            shell_quote(&prelude),
            shell_quote(&marker),
            opening_success(),
        ),
    );
    let server = basic_server(fixture.path(), executable, Duration::from_millis(100));

    let result = tokio::time::timeout(Duration::from_secs(10), attach(&server))
        .await
        .expect("bounded prelude reaches its configured deadline");
    let error = match result {
        Err(error) => error,
        Ok(control) => {
            drop(control);
            panic!("an opening prelude must not grow beyond the event receiver")
        }
    };
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(
        !marker.exists(),
        "the child is stopped before draining the prelude"
    );

    server.shutdown().await.expect("server shuts down");
}

#[tokio::test]
async fn terminal_notifications_drain_after_exit_and_eof() {
    for (terminal, expected_exits) in [("printf '%%exit done\\n'", 1), ("", 0)] {
        let fixture = directory();
        let executable = write_script(
            fixture.path(),
            &format!(
                "{}\nindex=0\nwhile [ \"$index\" -lt {} ]; do\n    printf '%%sessions-changed\\n'\n    index=$((index + 1))\ndone\n{terminal}",
                opening_success(),
                EVENT_QUEUE + 1,
            ),
        );
        let server = basic_server(fixture.path(), executable, Duration::from_secs(1));
        let (commands, mut events) = attach(&server)
            .await
            .expect("control mode attaches")
            .split();

        tokio::time::timeout(TEST_TIMEOUT, async {
            while !commands.is_closed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the terminal condition closes command admission");

        let mut sessions_changed = 0;
        let mut exits = 0;
        tokio::time::timeout(TEST_TIMEOUT, async {
            while let Some(event) = events.next_event().await {
                match event {
                    Event::SessionsChanged => sessions_changed += 1,
                    Event::Exit { .. } => exits += 1,
                    other => panic!("unexpected terminal fixture event: {other:?}"),
                }
            }
        })
        .await
        .expect("every parsed event reaches the receiver");

        assert_eq!(sessions_changed, EVENT_QUEUE + 1);
        assert_eq!(exits, expected_exits);
        events.shutdown().await.expect("connection shuts down");
        server.shutdown().await.expect("server shuts down");
    }
}

#[tokio::test]
async fn terminal_notifications_drain_after_eof_inside_a_reply() {
    let fixture = directory();
    let executable = write_script(
        fixture.path(),
        &format!(
            "{}\nIFS= read -r _command\nindex=0\nwhile [ \"$index\" -lt {} ]; do\n    printf '%%sessions-changed\\n'\n    index=$((index + 1))\ndone\nprintf '%%begin 0 2 0\\npartial\\n'",
            opening_success(),
            EVENT_QUEUE + 1,
        ),
    );
    let server = basic_server(fixture.path(), executable, Duration::from_secs(1));
    let (commands, mut events) = attach(&server)
        .await
        .expect("control mode attaches")
        .split();

    let error = commands
        .send(Command::new("display-message").arg("unanswered"))
        .await
        .expect_err("EOF cannot finish an open reply");
    assert!(matches!(
        error,
        Error::ControlMode {
            kind: ControlModeErrorKind::Closed,
            ..
        }
    ));

    let mut sessions_changed = 0;
    while let Some(event) = events.next_event().await {
        match event {
            Event::SessionsChanged => sessions_changed += 1,
            other => panic!("unexpected terminal fixture event: {other:?}"),
        }
    }
    assert_eq!(
        sessions_changed,
        EVENT_QUEUE + 1,
        "a malformed final reply must not discard already parsed notifications"
    );

    let error = events
        .shutdown()
        .await
        .expect_err("the incomplete reply remains the terminal cause");
    assert!(matches!(
        error,
        Error::ControlMode {
            kind: ControlModeErrorKind::Closed,
            ..
        }
    ));
    server.shutdown().await.expect("server shuts down");
}

#[tokio::test]
async fn terminal_drain_releases_a_committed_command_and_server_shutdown() {
    let fixture = directory();
    let executable = write_script(
        fixture.path(),
        &format!(
            "{}\nIFS= read -r _command\nindex=0\nwhile [ \"$index\" -lt {} ]; do\n    printf '%%sessions-changed\\n'\n    index=$((index + 1))\ndone",
            opening_success(),
            EVENT_QUEUE + 1,
        ),
    );
    let server = basic_server(fixture.path(), executable, Duration::from_secs(1));
    let (commands, events) = attach(&server)
        .await
        .expect("control mode attaches")
        .split();

    let command = tokio::spawn({
        let commands = commands.clone();
        async move {
            commands
                .send(Command::new("display-message").arg("unanswered"))
                .await
        }
    });
    tokio::time::timeout(TEST_TIMEOUT, async {
        while !commands.is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("EOF closes command admission");

    let error = tokio::time::timeout(TEST_TIMEOUT, command)
        .await
        .expect("EOF releases the committed command")
        .expect("the command task joins")
        .expect_err("EOF cannot answer the committed command");
    assert!(matches!(
        error,
        Error::ControlMode {
            kind: ControlModeErrorKind::Closed,
            ..
        }
    ));

    tokio::time::timeout(TEST_TIMEOUT, server.shutdown())
        .await
        .expect("server shutdown interrupts terminal draining")
        .expect("server shuts down");
    drop(events);
}

#[tokio::test]
async fn pane_snapshot_separates_output_at_the_capture_block() {
    let fixture = directory();
    let executable = write_script(
        fixture.path(),
        &format!(
            "{}\nIFS= read -r _command\nprintf '%%output %%1 before\\n'\nprintf '%%begin 0 2 0\\nvisible\\n%%end 0 2 0\\n'\nprintf '%%output %%1 after\\n'",
            opening_success(),
        ),
    );
    let server = basic_server(fixture.path(), executable, Duration::from_secs(1));
    let (commands, events) = attach(&server)
        .await
        .expect("control mode attaches")
        .split();
    let mut output = PaneOutput::new("%1".parse().expect("a pane id"), events, commands);

    let mut preceding = Vec::new();
    let visible = output
        .snapshot(|bytes| preceding.extend_from_slice(bytes))
        .await
        .expect("the pane is captured");

    assert_eq!(visible, [TmuxText::from("visible")]);
    assert_eq!(preceding, b"before");
    assert_eq!(
        output.next_chunk().await.as_deref(),
        Some(b"after".as_slice())
    );
    output.shutdown().await.expect("connection shuts down");
    server.shutdown().await.expect("server shuts down");
}

#[tokio::test]
async fn an_error_opening_block_is_not_ready() {
    let fixture = directory();
    let executable = write_script(fixture.path(), "printf '%%begin 0 1 0\\n%%error 0 1 0\\n'");
    let server = basic_server(fixture.path(), executable, Duration::from_secs(1));

    let error = attach(&server)
        .await
        .expect_err("an opening error is not a successful attach");
    assert_eq!(error.kind(), ErrorKind::Transport);

    server.shutdown().await.expect("server shuts down");
}

#[tokio::test]
async fn server_shutdown_owns_active_control_clients() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    let marker = fixture.path().join("command-started");
    let prefix = format!(
        "{}\nIFS= read -r _line\n: > {}",
        opening_success(),
        shell_quote(&marker),
    );
    let executable = write_script(
        fixture.path(),
        &process_script(&parent, &descendant, &prefix),
    );
    let server = basic_server(fixture.path(), executable, Duration::from_secs(30));
    let (commands, events) = attach(&server)
        .await
        .expect("control mode attaches")
        .split();
    let parent_pid = wait_for_pid(&parent).await;
    let descendant_pid = wait_for_pid(&descendant).await;
    let sender = commands.clone();
    let sending = tokio::spawn(async move { sender.send(Command::new("display-message")).await });
    tokio::time::timeout(TEST_TIMEOUT, async {
        while !marker.exists() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("the request becomes active before shutdown");

    tokio::time::timeout(Duration::from_secs(2), server.shutdown())
        .await
        .expect("server shutdown waits for control cleanup")
        .expect("server shuts down");
    assert_process_gone(parent_pid).await;
    assert_process_gone(descendant_pid).await;
    let send_error = sending
        .await
        .expect("sender task joins")
        .expect_err("Core shutdown cancels the active request");
    assert!(matches!(send_error, Error::ExecutorShutdown { .. }));
    let connection_error = events
        .shutdown()
        .await
        .expect_err("Core shutdown is reported by the connection");
    assert!(matches!(connection_error, Error::ExecutorShutdown { .. }));
    drop(commands);
}

#[tokio::test]
async fn attach_after_server_shutdown_is_rejected() {
    let fixture = directory();
    let executable = write_script(fixture.path(), opening_success());
    let server = basic_server(fixture.path(), executable, Duration::from_secs(1));
    server
        .capabilities()
        .await
        .expect("capabilities are cached");
    server.shutdown().await.expect("server shuts down");

    let error = attach(&server)
        .await
        .expect_err("shutdown closes persistent-client admission");
    assert!(matches!(error, Error::ExecutorShutdown { .. }));
}
