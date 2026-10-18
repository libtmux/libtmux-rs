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

use super::channel::EVENT_QUEUE;
use super::{ControlMode, Event, PaneOutput};
use crate::internal::core::{BuildContext, Core, CoreConfiguration, SocketSelection};
use crate::{Command, ControlModeErrorKind, Error, ErrorKind, Server, SessionId, TmuxText};

macro_rules! timed {
    ($limit:expr, $body:expr $(,)?) => {{
        let started = Instant::now();
        let outcome = $body;
        if started.elapsed() <= $limit {
            Ok(outcome)
        } else {
            Err("operation exceeded deadline")
        }
    }};
}

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
///
/// This widens that race rather than closing it: a fork slower than a second
/// fails the same way. Closing it needs the child's PID from the spawn itself
/// instead of from a file the child writes, which `internal::process` does not
/// expose the way `internal::subprocess` does for its own tests.
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
    write_script_with_version(directory, body, "printf 'tmux 3.5a\\n'")
}

fn write_script_with_version(directory: &Path, body: &str, version: &str) -> PathBuf {
    let path = directory.join("fake-tmux");
    let staging = directory.join(format!(".fake-tmux.{}.tmp", process::id()));
    let mut file = fs::File::create(&staging).expect("staged script is creatable");
    writeln!(
        file,
        "#!/bin/sh\nif [ \"${{1-}}\" = \"-V\" ]; then\n    {version}\n    exit 0\nfi\nset -eu\n{body}"
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

fn wait_for_pid(path: &Path) -> u32 {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if let Some(value) = read_pid(path) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "child publishes PID before deadline"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn process_exists(value: u32) -> bool {
    !matches!(test_kill_process(pid(value)), Err(Errno::SRCH))
}

fn assert_process_gone(value: u32) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while process_exists(value) {
        assert!(
            Instant::now() < deadline,
            "process {value} disappears before deadline"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
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

/// The discarded opening block, followed by a success reply to the
/// `refresh-client -f new-layouts` request `ControlMode::attach` now sends
/// before returning -- block 1 is the opening handshake, block 2 answers that
/// request, so a test's own first command after attaching is block 3.
fn opening_success() -> &'static str {
    r#"reply_fence() {
    IFS= read -r _fence
    printf '%%begin 0 900 1\nparse error: unknown command: %s\n%%error 0 900 1\n' "$_fence"
}
printf '%%begin 0 1 0\n%%end 0 1 0\n'
IFS= read -r _new_layouts
printf '%%begin 0 2 1\n%%end 0 2 1\n'
reply_fence"#
}

fn attach(server: &Server) -> Result<ControlMode, Error> {
    ControlMode::attach(server, &session())
}

#[test]
fn attach_uses_the_cores_captured_launch_context() {
    let fixture = directory();
    let record = fixture.path().join("context");
    let executable = write_script(
        fixture.path(),
        &format!(
            "{{\n    pwd\n    printf '%s\\n' \"$PATH\"\n    for argument in \"$@\"; do printf '<%s>\\n' \"$argument\"; done\n}} > {}\n{}\nprintf '%%exit done\\n'",
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

    let control = attach(&server).expect("control mode attaches");
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

    control.shutdown().expect("control mode shuts down");
    server.shutdown().expect("server shuts down");
}

#[test]
fn opening_handshake_times_out_and_reaps_the_process_group() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    let executable = write_script(fixture.path(), &process_script(&parent, &descendant, ""));
    let server = basic_server(fixture.path(), executable, HANDSHAKE_TIMEOUT);

    let error = timed!(TEST_TIMEOUT, attach(&server))
        .expect("the configured deadline ends attach")
        .expect_err("an absent opening block times out");
    assert_eq!(error.kind(), ErrorKind::Timeout);
    let parent_pid = wait_for_pid(&parent);
    let descendant_pid = wait_for_pid(&descendant);
    assert_process_gone(parent_pid);
    assert_process_gone(descendant_pid);

    server.shutdown().expect("server shuts down");
}

#[test]
fn a_short_reply_deadline_does_not_bound_attaching() {
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
        .expect("attaching outlasts the reply deadline")
        .reply_timeout(REPLY_TIMEOUT)
        .split();

    // Bounded well under the server's deadline, so a `reply_timeout` that did
    // not take effect fails here rather than passing five seconds later on the
    // connection's own default.
    let error = timed!(
        Duration::from_secs(2),
        commands.send(Command::new("display-message")),
    )
    .expect("the command keeps the shorter deadline")
    .expect_err("the command times out");
    assert_eq!(error.kind(), ErrorKind::Timeout);
    let shutdown = events.shutdown().expect_err("the actor reports timeout");
    assert_eq!(shutdown.kind(), ErrorKind::Timeout);

    server.shutdown().expect("server shuts down");
}

#[test]
fn cancelling_attach_reaps_the_process_group() {
    for negotiate_layouts in [false, true] {
        let fixture = directory();
        let parent = fixture.path().join("parent.pid");
        let descendant = fixture.path().join("descendant.pid");
        let layouts = fixture.path().join("layouts.pid");
        let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
        let prefix = if negotiate_layouts {
            format!(
                "printf '%%begin 0 1 0\\n%%end 0 1 0\\n'\nIFS= read -r layouts\nprintf '%s\\n' \"$$\" > {}",
                shell_quote(&layouts),
            )
        } else {
            String::new()
        };
        let executable = write_script(
            fixture.path(),
            &process_script(&parent, &descendant, &prefix),
        );
        let server = basic_server(fixture.path(), executable, Duration::from_secs(30));
        let cancelled = server.cancellation_token().clone();
        let attached = server.clone();
        let task = std::thread::spawn(move || attach(&attached));
        let parent_pid = wait_for_pid(&parent);
        let descendant_pid = wait_for_pid(&descendant);
        if negotiate_layouts {
            assert_eq!(wait_for_pid(&layouts), parent_pid);
        }

        cancelled.cancel();
        let error = task
            .join()
            .expect("attach thread joins")
            .expect_err("attach is cancelled");
        assert_eq!(error.kind(), ErrorKind::Cancelled);
        assert_process_gone(parent_pid);
        assert_process_gone(descendant_pid);

        server.shutdown().expect("server shuts down");
    }
}

#[test]
fn dropping_both_halves_reaps_the_process_group() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    let executable = write_script(
        fixture.path(),
        &process_script(&parent, &descendant, opening_success()),
    );
    let server = basic_server(fixture.path(), executable, Duration::from_secs(30));
    let control = attach(&server).expect("control mode attaches");
    let parent_pid = wait_for_pid(&parent);
    let descendant_pid = wait_for_pid(&descendant);

    drop(control);
    assert_process_gone(parent_pid);
    assert_process_gone(descendant_pid);

    server.shutdown().expect("server shuts down");
}

#[test]
fn an_open_response_block_has_one_deadline() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let marker = fixture.path().join("command-started");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    let prefix = format!(
        "{}\nIFS= read -r _line\n: > {}\nprintf '%%begin 0 3 1\\n'",
        opening_success(),
        shell_quote(&marker),
    );
    let executable = write_script(
        fixture.path(),
        &process_script(&parent, &descendant, &prefix),
    );
    let server = basic_server(fixture.path(), executable, TEST_TIMEOUT);
    let (commands, events) = attach(&server)
        .expect("control mode attaches")
        .reply_timeout(REPLY_TIMEOUT)
        .split();

    let error = timed!(
        Duration::from_secs(2),
        commands.send(Command::new("display-message")),
    )
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
        .expect_err("the timed out sender remains closed");
    assert!(
        !closed.is_transient(),
        "waiting cannot reopen the timed out sender",
    );
    let shutdown = events.shutdown().expect_err("the actor reports timeout");
    assert_eq!(shutdown.kind(), ErrorKind::Timeout);
    assert_process_gone(wait_for_pid(&parent));
    assert_process_gone(wait_for_pid(&descendant));

    let replacement = attach(&server).expect("a new connection can attach through the same server");
    replacement
        .shutdown()
        .expect("the replacement connection shuts down");
    server.shutdown().expect("server shuts down");
}

#[test]
fn a_reply_deadline_starts_before_begin() {
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
        .expect("control mode attaches")
        .reply_timeout(REPLY_TIMEOUT)
        .split();

    let error = timed!(
        Duration::from_secs(2),
        commands.send(Command::new("display-message")),
    )
    .expect("the reply deadline does not wait for begin")
    .expect_err("a missing begin times out");
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(marker.exists(), "the command reached the fake client");
    let shutdown = events.shutdown().expect_err("the actor reports timeout");
    assert_eq!(shutdown.kind(), ErrorKind::Timeout);

    server.shutdown().expect("server shuts down");
}

#[test]
fn the_earliest_committed_deadline_ends_the_connection() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let first_seen = fixture.path().join("first-command");
    let second_seen = fixture.path().join("second-command");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    let prefix = format!(
        "{}\nIFS= read -r _first\n: > {}\nIFS= read -r _first_fence\nIFS= read -r _second\n: > {}",
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
        .expect("control mode attaches")
        .reply_timeout(Duration::from_secs(2))
        .split();
    let earlier = later.clone().reply_timeout(REPLY_TIMEOUT);

    let first = std::thread::spawn(move || later.send(Command::new("list-sessions")));
    timed!(TEST_TIMEOUT, {
        while !first_seen.exists() {
            std::thread::sleep(POLL_INTERVAL);
        }
    })
    .expect("the first command reaches the client");

    let second = std::thread::spawn(move || earlier.send(Command::new("list-windows")));
    timed!(TEST_TIMEOUT, {
        while !second_seen.exists() {
            std::thread::sleep(POLL_INTERVAL);
        }
    })
    .expect("the second command reaches the client");

    let second_error = timed!(Duration::from_secs(1), second.join())
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
        .join()
        .expect("the first sender task joins")
        .expect_err("the shared connection ends");
    assert_eq!(first_error.kind(), ErrorKind::Timeout);
    let shutdown = events
        .shutdown()
        .expect_err("the actor reports its deadline");
    assert_eq!(shutdown.kind(), ErrorKind::Timeout);
    assert_process_gone(wait_for_pid(&parent));
    assert_process_gone(wait_for_pid(&descendant));

    server.shutdown().expect("server shuts down");
}

#[test]
fn a_blocked_write_uses_the_reply_deadline() {
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
        .expect("control mode attaches")
        .reply_timeout(REPLY_TIMEOUT)
        .split();

    let error = timed!(
        Duration::from_secs(2),
        // Sized between two bounds rather than to be asserted on: past the
        // pipe buffer, and short of the 2 MiB that spends this deadline being
        // serialized before the request can commit.
        commands.send(Command::new("display-message").arg("x".repeat(256 * 1024))),
    )
    .expect("the blocked write reaches its deadline")
    .expect_err("the blocked write times out");
    assert_eq!(error.kind(), ErrorKind::Timeout);
    let shutdown = events.shutdown().expect_err("the actor reports timeout");
    assert_eq!(shutdown.kind(), ErrorKind::Timeout);

    server.shutdown().expect("server shuts down");
}

#[test]
fn watcher_shutdown_interrupts_an_open_response_block() {
    let fixture = directory();
    let parent = fixture.path().join("parent.pid");
    let descendant = fixture.path().join("descendant.pid");
    let marker = fixture.path().join("command-started");
    let _guard = ProcessGuard::new([parent.clone(), descendant.clone()]);
    let prefix = format!(
        "{}\nIFS= read -r _line\n: > {}\nprintf '%%begin 0 3 1\\n'",
        opening_success(),
        shell_quote(&marker),
    );
    let executable = write_script(
        fixture.path(),
        &process_script(&parent, &descendant, &prefix),
    );
    let server = basic_server(fixture.path(), executable, Duration::from_secs(30));
    let (commands, events) = attach(&server).expect("control mode attaches").split();
    let sender = commands.clone();
    let sending = std::thread::spawn(move || sender.send(Command::new("display-message")));
    timed!(TEST_TIMEOUT, {
        while !marker.exists() {
            std::thread::sleep(POLL_INTERVAL);
        }
    })
    .expect("the fake client opens the response block");

    timed!(Duration::from_secs(2), events.shutdown())
        .expect("watcher shutdown interrupts the read")
        .expect("explicit shutdown is clean");
    let send_error = sending
        .join()
        .expect("sender task joins")
        .expect_err("the interrupted command closes");
    assert!(matches!(
        &send_error,
        Error::ControlMode {
            kind: ControlModeErrorKind::Closed,
            ..
        }
    ));
    assert!(
        !send_error.is_transient(),
        "the stopped sender cannot reopen its connection",
    );
    let same_sender = commands
        .send(Command::new("display-message"))
        .expect_err("the same sender remains closed");
    assert!(
        !same_sender.is_transient(),
        "waiting cannot reopen a closed sender",
    );
    assert_process_gone(wait_for_pid(&parent));
    assert_process_gone(wait_for_pid(&descendant));

    drop(commands);
    let replacement = attach(&server).expect("a new connection can attach through the same server");
    replacement
        .shutdown()
        .expect("the replacement connection shuts down");
    server.shutdown().expect("server shuts down");
}

#[test]
fn opening_notifications_stop_at_the_receiver_bound() {
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

    let result = timed!(Duration::from_secs(10), attach(&server))
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

    server.shutdown().expect("server shuts down");
}

#[test]
fn terminal_notifications_drain_after_exit_and_eof() {
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
        let (commands, mut events) = attach(&server).expect("control mode attaches").split();

        timed!(TEST_TIMEOUT, {
            while !commands.is_closed() {
                std::thread::yield_now();
            }
        })
        .expect("the terminal condition closes command admission");

        let mut sessions_changed = 0;
        let mut exits = 0;
        let mut errors = 0;
        timed!(TEST_TIMEOUT, {
            while let Some(event) = events.next_event() {
                match event {
                    Ok(Event::SessionsChanged) => sessions_changed += 1,
                    Ok(Event::Exit { .. }) => exits += 1,
                    Err(Error::ControlMode {
                        kind: ControlModeErrorKind::Closed,
                        ..
                    }) => {
                        assert_eq!(sessions_changed, EVENT_QUEUE + 1);
                        errors += 1;
                    }
                    other => panic!("unexpected terminal fixture event: {other:?}"),
                }
            }
        })
        .expect("every parsed event reaches the receiver");

        assert_eq!(sessions_changed, EVENT_QUEUE + 1);
        assert_eq!(exits, expected_exits);
        assert_eq!(errors, usize::from(expected_exits == 0));
        assert!(events.next_event().is_none());
        events.shutdown().expect("connection shuts down");
        server.shutdown().expect("server shuts down");
    }
}

#[test]
fn terminal_notifications_drain_after_eof_inside_a_reply() {
    let fixture = directory();
    let executable = write_script(
        fixture.path(),
        &format!(
            "{}\nIFS= read -r _command\nIFS= read -r _completion_fence\nindex=0\nwhile [ \"$index\" -lt {} ]; do\n    printf '%%sessions-changed\\n'\n    index=$((index + 1))\ndone\nprintf '%%begin 0 3 1\\npartial\\n'",
            opening_success(),
            EVENT_QUEUE + 1,
        ),
    );
    let server = basic_server(fixture.path(), executable, Duration::from_secs(1));
    let (commands, mut events) = attach(&server).expect("control mode attaches").split();

    let error = commands
        .send(Command::new("display-message").arg("unanswered"))
        .expect_err("EOF cannot finish an open reply");
    assert!(matches!(
        error,
        Error::ControlMode {
            kind: ControlModeErrorKind::Closed,
            ..
        }
    ));

    let mut sessions_changed = 0;
    let mut terminal_error = None;
    while let Some(event) = events.next_event() {
        match event {
            Ok(Event::SessionsChanged) => sessions_changed += 1,
            Err(error) => {
                assert!(terminal_error.replace(error).is_none());
                assert_eq!(sessions_changed, EVENT_QUEUE + 1);
            }
            other => panic!("unexpected terminal fixture event: {other:?}"),
        }
    }
    assert_eq!(
        sessions_changed,
        EVENT_QUEUE + 1,
        "a malformed final reply must not discard already parsed notifications"
    );

    let error = terminal_error.expect("the incomplete reply remains the terminal cause");
    assert!(matches!(
        error,
        Error::ControlMode {
            kind: ControlModeErrorKind::Closed,
            ..
        }
    ));
    events.shutdown().expect("the terminal error was delivered");
    server.shutdown().expect("server shuts down");
}

#[test]
fn terminal_drain_releases_a_committed_command_and_server_shutdown() {
    let fixture = directory();
    let executable = write_script(
        fixture.path(),
        &format!(
            "{}\nIFS= read -r _command\nIFS= read -r _completion_fence\nindex=0\nwhile [ \"$index\" -lt {} ]; do\n    printf '%%sessions-changed\\n'\n    index=$((index + 1))\ndone",
            opening_success(),
            EVENT_QUEUE + 1,
        ),
    );
    let server = basic_server(fixture.path(), executable, Duration::from_secs(1));
    let (commands, events) = attach(&server).expect("control mode attaches").split();

    let command = std::thread::spawn({
        let commands = commands.clone();
        move || commands.send(Command::new("display-message").arg("unanswered"))
    });
    timed!(TEST_TIMEOUT, {
        while !commands.is_closed() {
            std::thread::yield_now();
        }
    })
    .expect("EOF closes command admission");

    let error = timed!(TEST_TIMEOUT, command.join())
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

    timed!(TEST_TIMEOUT, server.shutdown())
        .expect("server shutdown interrupts terminal draining")
        .expect("server shuts down");
    drop(events);
}

#[test]
fn pane_snapshot_separates_output_at_the_capture_block() {
    let fixture = directory();
    let executable = write_script(
        fixture.path(),
        &format!(
            "{}\nIFS= read -r _command\nprintf '%%output %%1 before\\n'\nprintf '%%begin 0 3 1\\nvisible\\n%%end 0 3 1\\n'\nprintf '%%output %%1 after\\n'\nreply_fence\nprintf '%%exit done\\n'",
            opening_success(),
        ),
    );
    let server = basic_server(fixture.path(), executable, Duration::from_secs(1));
    let (commands, events) = attach(&server).expect("control mode attaches").split();
    let mut output = PaneOutput::new("%1".parse().expect("a pane id"), events, commands);

    let mut preceding = Vec::new();
    let visible = output
        .snapshot(|bytes| preceding.extend_from_slice(bytes))
        .expect("the pane is captured");

    assert_eq!(visible, [TmuxText::from("visible")]);
    assert_eq!(preceding, b"before");
    assert_eq!(output.next_chunk().as_deref(), Some(b"after".as_slice()));
    output.shutdown().expect("connection shuts down");
    server.shutdown().expect("server shuts down");
}

#[test]
fn an_error_opening_block_is_not_ready() {
    let fixture = directory();
    let executable = write_script(fixture.path(), "printf '%%begin 0 1 0\\n%%error 0 1 0\\n'");
    let server = basic_server(fixture.path(), executable, Duration::from_secs(1));

    let error = attach(&server).expect_err("an opening error is not a successful attach");
    assert!(matches!(
        error,
        Error::ControlMode {
            kind: ControlModeErrorKind::Closed,
            ..
        }
    ));

    server.shutdown().expect("server shuts down");
}

#[test]
fn server_shutdown_owns_active_control_clients() {
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
    let (commands, events) = attach(&server).expect("control mode attaches").split();
    let parent_pid = wait_for_pid(&parent);
    let descendant_pid = wait_for_pid(&descendant);
    let sender = commands.clone();
    let sending = std::thread::spawn(move || sender.send(Command::new("display-message")));
    timed!(TEST_TIMEOUT, {
        while !marker.exists() {
            std::thread::sleep(POLL_INTERVAL);
        }
    })
    .expect("the request becomes active before shutdown");

    timed!(Duration::from_secs(2), server.shutdown())
        .expect("server shutdown waits for control cleanup")
        .expect("server shuts down");
    assert_process_gone(parent_pid);
    assert_process_gone(descendant_pid);
    let send_error = sending
        .join()
        .expect("sender task joins")
        .expect_err("Core shutdown cancels the active request");
    assert!(matches!(send_error, Error::ExecutorShutdown { .. }));
    let connection_error = events
        .shutdown()
        .expect_err("Core shutdown is reported by the connection");
    assert!(matches!(connection_error, Error::ExecutorShutdown { .. }));
    drop(commands);
}

#[test]
fn attach_after_server_shutdown_is_rejected() {
    let fixture = directory();
    let executable = write_script(fixture.path(), opening_success());
    let server = basic_server(fixture.path(), executable, Duration::from_secs(1));
    server.capabilities().expect("capabilities are cached");
    server.shutdown().expect("server shuts down");

    let error = attach(&server).expect_err("shutdown closes persistent-client admission");
    assert!(matches!(error, Error::ExecutorShutdown { .. }));
}

#[test]
fn frame_error_during_bootstrap_remains_specific() {
    for opening in ["", "%begin 0 1 0\n%end 0 1 0\n"] {
        let fixture = directory();
        let payload = fixture.path().join("payload");
        fs::write(&payload, format!("{opening}{}\n", "x".repeat(256)))
            .expect("payload is writable");
        let executable = write_script(
            fixture.path(),
            &format!(
                "/bin/cat {}\nwhile IFS= read -r line; do :; done",
                shell_quote(&payload)
            ),
        );
        let server = basic_server(fixture.path(), executable, TEST_TIMEOUT);
        let error = ControlMode::attach_with_limits(
            &server,
            &session(),
            crate::ControlLimits::default().max_line_bytes(64),
        )
        .expect_err("the oversized frame is rejected");
        server.shutdown().expect("server shuts down");
        assert!(
            matches!(
                error,
                Error::ControlModeFrameTooLarge {
                    frame: "line",
                    limit: 64
                }
            ),
            "opening={opening:?}: got {error:?}",
        );
    }
}

#[cfg(feature = "test-support")]
#[test]
fn real_tmux_bootstrap_cleanup_preserves_frame_failure() {
    let guard = crate::test::TestServer::builder()
        .start()
        .expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("frame-closed").expect("session starts");
    let pane = session.panes().expect("panes list").remove(0);
    let (sender, events) = ControlMode::attach_with_limits(
        server,
        session.id(),
        crate::ControlLimits::default().max_line_bytes(512),
    )
    .expect("a quiet connection attaches")
    .split();
    let produced = server
        .cmd(
            Command::new("respawn-pane")
                .arg("-k")
                .arg("-t")
                .arg(pane.id().as_ref())
                .arg("head -c 4096 /dev/zero | tr '\\0' A; exec cat"),
        )
        .expect("producer starts");
    assert!(produced.success(), "producer is accepted: {produced:?}");
    timed!(Duration::from_secs(1), {
        while !sender.is_closed() {
            std::thread::yield_now();
        }
    })
    .expect("oversized output closes admission");
    let send_error = sender.watch_only(&[]).expect_err("narrowing is refused");
    drop(sender);
    assert!(matches!(
        send_error,
        Error::ControlMode {
            kind: ControlModeErrorKind::Closed,
            ..
        }
    ));
    let error = events.shutdown_after_error(send_error);
    guard.shutdown().expect("fixture shuts down");
    assert!(
        matches!(
            error,
            Error::ControlModeFrameTooLarge {
                frame: "line",
                limit: 512
            }
        ),
        "got {error:?}"
    );
}

#[cfg(feature = "test-support")]
#[test]
fn initial_watch_failure_preserves_the_connection_error() {
    let fixture = directory();
    let payload = fixture.path().join("payload");
    fs::write(
        &payload,
        format!("%begin 0 2 1\n%end 0 2 1\n{}\n", "x".repeat(256)),
    )
    .expect("payload is writable");
    let guard = crate::test::TestServer::builder()
        .start()
        .expect("tmux starts");
    let session = guard
        .server()
        .new_session("initial-watch")
        .expect("session starts");
    let pane = session.panes().expect("panes list").remove(0);
    let tmux = shell_quote(Path::new(guard.server().tmux_executable()));
    let executable = write_script_with_version(
        fixture.path(),
        &format!(
            "for argument in \"$@\"; do\nif [ \"$argument\" = '-C' ]; then\nprintf '%%begin 0 1 0\\n%%end 0 1 0\\n'\nIFS= read -r layouts\n/bin/cat {}\nwhile IFS= read -r line; do :; done\nexit\nfi\ndone\nexec {tmux} \"$@\"",
            shell_quote(&payload),
        ),
        &format!("exec {tmux} -V"),
    );
    let wrapped = Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(executable)
        .build()
        .expect("wrapper server resolves");
    let observed = wrapped
        .panes()
        .expect("pane lookup succeeds")
        .into_iter()
        .find(|candidate| candidate.id() == pane.id())
        .expect("pane exists");
    let error = observed
        .stream_output_with_limits(crate::ControlLimits::default().max_line_bytes(64))
        .expect_err("initial narrowing sees the frame violation");
    wrapped.shutdown().expect("wrapper shuts down");
    guard.shutdown().expect("fixture shuts down");
    assert!(
        matches!(
            error,
            Error::ControlModeFrameTooLarge {
                frame: "line",
                limit: 64
            }
        ),
        "got {error:?}"
    );
}

#[test]
fn failed_control_cleanup_remains_visible_to_repeated_server_shutdown() {
    let fixture = directory();
    let marker = fixture.path().join("fail-signal");
    let parent = fixture.path().join("parent.pid");
    let _guard = ProcessGuard::new([parent.clone()]);
    let executable = write_script(
        fixture.path(),
        &format!(
            "printf '%s\\n' \"$$\" > {}\n{}\nwhile IFS= read -r line; do :; done",
            shell_quote(&parent),
            opening_success(),
        ),
    );
    let helper = fixture.path().join("signal-group");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nif [ -e {} ]; then exit 1; fi\nexec /usr/bin/kill \"$@\"\n",
            shell_quote(&marker),
        ),
    )
    .expect("signal helper is written");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper executable");
    let server = Server::builder()
        .tmux_executable(executable)
        .socket_path(fixture.path().join("socket"))
        .group_signal_helper(helper)
        .default_timeout(TEST_TIMEOUT)
        .build()
        .expect("server builds");
    let control = attach(&server).expect("control attaches before failure");
    let parent = wait_for_pid(&parent);
    fs::write(marker, []).expect("inject helper failure");
    let error = control
        .shutdown()
        .expect_err("connection reports cleanup failure");
    let (request_id, summary, detail) = match error {
        Error::WaitChild {
            request_id,
            command,
            source,
        } => {
            let detail = source.to_string();
            assert_eq!(detail, "process-group signal: Other");
            (request_id, command, detail)
        }
        other => panic!("expected cleanup failure, got {other:?}"),
    };
    assert!(summary.to_string().starts_with("\"attach\" "));
    assert_process_gone(parent);
    for _ in 0..2 {
        match server
            .shutdown()
            .expect_err("shutdown retains cleanup failure")
        {
            Error::WaitChild {
                request_id: observed,
                command,
                source,
            } => {
                assert_eq!(observed, request_id);
                assert_eq!(command, summary);
                assert_eq!(source.to_string(), detail);
            }
            other => panic!("expected retained cleanup failure, got {other:?}"),
        }
    }
}
