#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::too_many_lines
)]

use std::ffi::OsString;
use std::fs;
use std::io::{BufRead as _, Write as _};
#[cfg(feature = "plan")]
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::time::Duration;

use libtmux_core::control::{ControlFault, ControlNotice};
use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process, test_kill_process};

use super::ControlMode;
use crate::ControlModeErrorKind;
use crate::internal::core::{BuildContext, Core, CoreConfiguration, SocketSelection};
use crate::{Command, Error, ErrorKind, Server, SessionId};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(1);
const HELPER: &str = "control::lifecycle_tests::control_process_helper";

fn session() -> SessionId {
    "$1".parse().unwrap()
}
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}
fn pid(value: u32) -> Pid {
    Pid::from_raw(i32::try_from(value).unwrap()).unwrap()
}

struct Fixture {
    server: Server,
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new(mode: &str, version: &str, timeout: Duration) -> Self {
        let root = Path::new("/tmp/libtmux-rs-test");
        fs::create_dir_all(root).unwrap();
        let directory = tempfile::Builder::new()
            .prefix("control-wire-")
            .tempdir_in(root)
            .unwrap();
        let socket = directory.path().join("socket;");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let executable = directory.path().join("wire-client");
        let template = r#"#!/bin/sh
set -eu
if [ "${1-}" = '-V' ]; then printf 'tmux @@VERSION@@\n'; exit 0; fi
for argument do
    case "$argument" in
    libtmux-control-challenge:*)
        printf 'unknown command: %s@@VERSION@@,123,456,$1\n' "${argument%%#*}" >&2
        exit 1
        ;;
    libtmux-create:*)
        nonce=${argument#libtmux-create:}
        nonce=${nonce%% *}
        printf 'libtmux-create:%s 123 456 %%7 $2 @9 :end\n' "$nonce"
        printf 'native\n' >> @@ROOT@@/native-creations
        printf '%s\0' "$@" > @@ROOT@@/native-argv
        exit 0
        ;;
    esac
done
export LIBTMUX_CONTROL_WIRE_MODE=@@MODE@@
export LIBTMUX_CONTROL_WIRE_ROOT=@@ROOT@@
printf '%s\n' "$$" > "$LIBTMUX_CONTROL_WIRE_ROOT/pid"
/bin/sleep 86400 &
printf '%s\n' "$!" > "$LIBTMUX_CONTROL_WIRE_ROOT/descendant"
{ pwd; printf '%s\n' "$PATH" "${TMUX-unset}" "${TMUX_PANE-unset}" "$0"; printf '<%s>\n' "$@"; } > "$LIBTMUX_CONTROL_WIRE_ROOT/context"
exec @@EXE@@ --exact @@HELPER@@ --nocapture -- "$@"
"#;
        let script = template
            .replace("@@VERSION@@", version)
            .replace("@@MODE@@", mode)
            .replace("@@ROOT@@", &shell_quote(directory.path()))
            .replace("@@EXE@@", &shell_quote(&std::env::current_exe().unwrap()))
            .replace("@@HELPER@@", HELPER);
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let context = BuildContext::new(
            Some(directory.path().to_owned()),
            Some(OsString::from("/usr/bin:/bin")),
            Some("inherited".into()),
            Some("%41".into()),
            None,
            Some("/tmp".into()),
            rustix::process::getuid().as_raw(),
        );
        let configuration = CoreConfiguration::resolve(
            &SocketSelection::Path(socket),
            None,
            None,
            executable.into_os_string(),
            timeout,
            context,
        )
        .unwrap();
        Self {
            server: Server::from_core(Arc::new(Core::new(configuration))),
            directory,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }
    async fn attach(&self) -> Result<ControlMode, Error> {
        ControlMode::attach(&self.server, &session()).await
    }
    async fn child(&self) -> u32 {
        wait_pid(&self.path("pid")).await
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for name in ["pid", "descendant"] {
            if let Ok(value) = fs::read_to_string(self.path(name)) {
                if let Ok(value) = value.trim().parse() {
                    let _ = kill_process(pid(value), Signal::KILL);
                }
            }
        }
    }
}

async fn wait_pid(path: &Path) -> u32 {
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if let Ok(value) = fs::read_to_string(path) {
                if let Ok(pid) = value.trim().parse() {
                    return pid;
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("wire child publishes its PID")
}

async fn gone(value: u32) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        while !matches!(test_kill_process(pid(value)), Err(Errno::SRCH)) {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .expect("owned process disappears");
}

fn marker(line: &str, prefix: &str) -> Option<String> {
    let start = line.find(prefix)? + prefix.len();
    let nonce = line.get(start..start + 32)?;
    nonce
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit())
        .then(|| nonce.to_owned())
}

fn block(number: &mut u64, body: &str, succeeded: bool, flags: u8) {
    *number += 1;
    let terminator = if succeeded { "end" } else { "error" };
    print!("%begin 1 {number} {flags}\n{body}%{terminator} 1 {number} {flags}\n");
    std::io::stdout().flush().unwrap();
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "the helper terminates only its own fixture process"
)]
fn control_process_helper() {
    let Ok(mode) = std::env::var("LIBTMUX_CONTROL_WIRE_MODE") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("LIBTMUX_CONTROL_WIRE_ROOT").unwrap());
    if mode == "opening-wait" {
        loop {
            std::thread::park();
        }
    }
    let mut number = 0;
    block(&mut number, "", true, 0);
    println!(
        "%session-changed {} fixture",
        if mode == "wrong-session" { "$9" } else { "$1" }
    );
    std::io::stdout().flush().unwrap();
    let mut fences = 0;
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        if let Some(nonce) = marker(&line, "libtmux-control-challenge:") {
            block(&mut number, "", true, 1);
            let generation = if mode == "wrong-challenge" {
                "999"
            } else {
                "123"
            };
            println!("unknown command: libtmux-control-challenge:{nonce}:3.8,{generation},456,$1");
            if mode == "duplicate-challenge" {
                println!(
                    "unknown command: libtmux-control-challenge:{nonce}:3.8,{generation},456,$1"
                );
            }
        } else if let Some(nonce) = marker(&line, "libtmux-control-fence:") {
            block(&mut number, "", true, 1);
            println!("unknown command: libtmux-control-fence:{nonce}");
            std::io::stdout().flush().unwrap();
            fences += 1;
            if fences == 2 {
                if mode == "never-read" {
                    loop {
                        std::thread::park();
                    }
                }
                if mode == "idle-flood" {
                    for _ in 0..1000 {
                        println!("%output %3 noisy");
                    }
                }
            }
        } else if fences < 2 {
            block(&mut number, "", true, 1);
        } else if mode.starts_with("creation-") && line.contains("new-session") {
            if mode == "creation-before-receipt" {
                number += 1;
                println!("%begin 1 {number} 1\n123 456 %7 $2 @9");
                std::io::stdout().flush().unwrap();
                loop {
                    std::thread::park();
                }
            }
            block(
                &mut number,
                if mode == "creation-wrong-generation" {
                    "999 456 %7 $2 @9\n"
                } else {
                    "123 456 %7 $2 @9\n"
                },
                true,
                1,
            );
            match mode.as_str() {
                "creation-input-failure" => block(&mut number, "private-later-failure\n", false, 1),
                "creation-hook-failure" => block(&mut number, "private-hook-failure\n", false, 0),
                "creation-bad-tuple" => {
                    number += 1;
                    println!("%begin 1 {number} 1\n%end 2 {number} 1");
                }
                "creation-timeout" | "creation-wrong-generation" => {
                    std::io::stdout().flush().unwrap();
                    loop {
                        std::thread::park();
                    }
                }
                _ => {}
            }
        } else if mode == "bad-tuple" {
            number += 1;
            println!("%begin 1 {number} 1\nprivate-payload\n%end 2 {number} 1");
        } else if mode == "incomplete" {
            number += 1;
            println!("%begin 1 {number} 1\npartial");
            std::io::stdout().flush().unwrap();
            loop {
                std::thread::park();
            }
        } else if mode == "nested" {
            block(&mut number, "first\n", true, 1);
            block(&mut number, "private-hook\n", true, 0);
            block(&mut number, "nested\n", true, 1);
        } else if mode == "exit" {
            println!("%output %3 retained\n%exit fixture");
            std::io::stdout().flush().unwrap();
            process::exit(0);
        } else {
            block(
                &mut number,
                if line.contains("first") {
                    "first\n"
                } else {
                    "second\n"
                },
                true,
                1,
            );
            if mode == "hold" && line.contains("first") {
                fs::write(root.join("active"), b"ready").unwrap();
                while !root.join("release").exists() {
                    std::thread::sleep(POLL_INTERVAL);
                }
            }
        }
        std::io::stdout().flush().unwrap();
    }
    process::exit(0);
}

#[tokio::test]
async fn acquisition_uses_the_captured_context_and_pinned_client() {
    let fixture = Fixture::new("quiet", "3.8", TEST_TIMEOUT);
    let control = fixture.attach().await.unwrap();
    let context = fs::read_to_string(fixture.path("context")).unwrap();
    let lines: Vec<_> = context.lines().collect();
    assert_eq!(lines[0], fixture.directory.path().to_str().unwrap());
    assert_eq!(lines[1], "/usr/bin:/bin");
    assert_eq!(&lines[2..4], &["unset", "unset"]);
    assert_ne!(lines[4], fixture.server.tmux_executable().to_str().unwrap());
    assert!(context.contains("<attach>"));
    fs::write(fixture.server.tmux_executable(), b"broken replacement").unwrap();
    assert!(
        control
            .send(Command::new("display-message").arg("-p").arg("second"))
            .await
            .unwrap()
            .succeeded()
    );
    let child = fixture.child().await;
    control.shutdown().await.unwrap();
    gone(child).await;
    fixture.server.shutdown().await.unwrap();
}

fn hold_connection_owner(
    events: &mut super::ControlEvents,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let connection = events.connection.take().unwrap();
    let (release, hold) = tokio::sync::oneshot::channel();
    let (finished, witness) = tokio::sync::oneshot::channel();
    events.connection = Some(tokio::spawn(async move {
        let result = connection.await.unwrap();
        hold.await.unwrap();
        finished.send(()).unwrap();
        result
    }));
    (release, witness)
}

#[tokio::test]
async fn explicit_shutdown_returns_while_the_connection_owner_finishes() {
    let fixture = Fixture::new("quiet", "3.8", TEST_TIMEOUT);
    let (_sender, mut events) = fixture.attach().await.unwrap().split();
    let (release, witness) = hold_connection_owner(&mut events);
    let error = tokio::time::timeout(TEST_TIMEOUT, events.shutdown())
        .await
        .expect("return while cleanup is pending")
        .expect_err("the connection owner is still running");
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(matches!(error, Error::ShutdownPending));
    release.send(()).unwrap();
    tokio::time::timeout(TEST_TIMEOUT, witness)
        .await
        .unwrap()
        .expect("returning early leaves the connection owner running");
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn pending_control_cleanup_preserves_the_creation_receipt() {
    let fixture = Fixture::new("quiet", "3.8", TEST_TIMEOUT);
    let (_sender, mut events) = fixture.attach().await.unwrap().split();
    let (release, witness) = hold_connection_owner(&mut events);
    let receipt = libtmux_core::NewSession::new("work")
        .decode(fixture.server.socket_path(), b"123 456 %7 $8 @9\n")
        .unwrap();
    let primary = Error::control_mode_closed().after_creation(receipt.clone());
    let error = tokio::time::timeout(TEST_TIMEOUT, events.shutdown_after_error(primary))
        .await
        .unwrap();
    assert!(matches!(error, Error::OperationCleanupPending { .. }));
    assert_eq!(error.kind(), ErrorKind::PartialEffect);
    assert_eq!(error.creation_receipt(), Some(&receipt));
    assert!(!error.is_transient());
    release.send(()).unwrap();
    tokio::time::timeout(TEST_TIMEOUT, witness)
        .await
        .unwrap()
        .unwrap();
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn metadata_and_attached_challenges_refuse_unqualified_connections() {
    for version in ["3.6a", "next-3.7", "3.7-rc1"] {
        let fixture = Fixture::new("quiet", version, TEST_TIMEOUT);
        let error = fixture.attach().await.unwrap_err();
        assert_eq!(
            error.kind(),
            ErrorKind::UnsupportedVersion,
            "{version}: {error:?}"
        );
        assert!(
            !fixture.path("pid").exists(),
            "no control process before a checked daemon floor"
        );
    }
    for mode in ["wrong-challenge", "duplicate-challenge", "wrong-session"] {
        let fixture = Fixture::new(mode, "3.8", TEST_TIMEOUT);
        assert!(matches!(
            fixture.attach().await,
            Err(Error::ControlProtocol { .. })
        ));
        gone(fixture.child().await).await;
        fixture.server.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn cancelling_attach_reaps_the_owned_process_group() {
    let fixture = Fixture::new("opening-wait", "3.8", TEST_TIMEOUT);
    let server = fixture.server.clone();
    let attach = tokio::spawn(async move { ControlMode::attach(&server, &session()).await });
    let child = fixture.child().await;
    let descendant = wait_pid(&fixture.path("descendant")).await;
    attach.abort();
    let _ = attach.await;
    fixture.server.shutdown().await.unwrap();
    gone(child).await;
    gone(descendant).await;
}

#[tokio::test]
async fn an_opening_deadline_cleans_up_the_owned_process_group() {
    let fixture = Fixture::new("opening-wait", "3.8", Duration::from_millis(500));
    let error = fixture.attach().await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Timeout);
    gone(fixture.child().await).await;
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_blocked_partial_write_closes_and_reaps_the_connection() {
    let fixture = Fixture::new("never-read", "3.8", TEST_TIMEOUT);
    let (sender, events) = fixture
        .attach()
        .await
        .unwrap()
        .reply_timeout(Duration::from_millis(100))
        .split();
    let child = fixture.child().await;
    let error = sender
        .send(
            Command::new("display-message")
                .arg("-p")
                .sensitive_arg("x".repeat(1024 * 1024)),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(!error.is_transient());
    assert!(!format!("{error:?} {error}").contains(&"x".repeat(32)));
    assert!(
        sender.is_closed(),
        "a partial input cannot enter a reusable drain"
    );
    let _ = events.shutdown().await;
    fixture.server.shutdown().await.unwrap();
    gone(child).await;
}

#[tokio::test]
async fn a_cancelled_reply_drains_its_nonce_before_the_next_input() {
    let fixture = Fixture::new("hold", "3.8", TEST_TIMEOUT);
    let (sender, events) = fixture.attach().await.unwrap().split();
    let first = sender.clone();
    let pending = tokio::spawn(async move {
        first
            .send(Command::new("display-message").arg("-p").arg("first"))
            .await
    });
    tokio::time::timeout(TEST_TIMEOUT, async {
        while !fixture.path("active").exists() {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .unwrap();
    pending.abort();
    let _ = pending.await;
    fs::write(fixture.path("release"), b"release").unwrap();
    let reply = sender
        .send(Command::new("display-message").arg("-p").arg("second"))
        .await
        .unwrap();
    assert_eq!(reply.blocks().len(), 1);
    assert_eq!(reply.blocks()[0].output(), b"second\n");
    events.shutdown().await.unwrap();
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_idle_eof_yields_one_terminal_error() {
    let fixture = Fixture::new("quiet", "3.8", TEST_TIMEOUT);
    let (_sender, mut events) = fixture.attach().await.unwrap().split();
    let descendant = wait_pid(&fixture.path("descendant")).await;
    kill_process(pid(descendant), Signal::KILL).unwrap();
    kill_process(pid(fixture.child().await), Signal::KILL).unwrap();
    let error = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            match events.next_notice().await {
                Some(Ok(_)) => {}
                Some(Err(error)) => break error,
                None => panic!("unexpected EOF needs a terminal error"),
            }
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        error,
        Error::ControlMode {
            kind: ControlModeErrorKind::Closed,
            ..
        }
    ));
    assert!(events.next_notice().await.is_none());
    events.shutdown().await.unwrap();
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn idle_event_overflow_never_stops_command_progress() {
    let fixture = Fixture::new("idle-flood", "3.8", TEST_TIMEOUT);
    let (sender, mut events) = ControlMode::attach_with_limits(
        &fixture.server,
        &session(),
        crate::ControlLimits::default()
            .max_events(2)
            .max_event_bytes(128),
    )
    .await
    .unwrap()
    .split();
    let reply = sender
        .send(Command::new("display-message").arg("-p").arg("second"))
        .await
        .unwrap();
    assert!(reply.succeeded());
    let dropped = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            match events.next_notice().await {
                Some(Ok(ControlNotice::Gap { dropped, .. })) => break dropped,
                Some(Ok(_)) => {}
                other => panic!("connection closed before its gap: {other:?}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(dropped > 0);
    events.shutdown().await.unwrap();
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn nested_and_hook_blocks_are_drained_by_nonce() {
    let fixture = Fixture::new("nested", "3.8", TEST_TIMEOUT);
    let control = fixture.attach().await.unwrap();
    let reply = control
        .send(Command::new("display-message").arg("-p").arg("requested"))
        .await
        .unwrap();
    assert_eq!(reply.blocks().len(), 2);
    assert_eq!(reply.blocks()[0].output(), b"first\n");
    assert_eq!(reply.blocks()[1].output(), b"nested\n");
    assert!(!format!("{reply:?}").contains("private-hook"));
    control.shutdown().await.unwrap();
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn mismatched_frame_tuples_fail_the_reply_and_stream_once() {
    let fixture = Fixture::new("bad-tuple", "3.8", TEST_TIMEOUT);
    let (sender, mut events) = fixture.attach().await.unwrap().split();
    let error = sender
        .send(Command::new("display-message").arg("-p").arg("second"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::ControlProtocol {
            fault: ControlFault::WrongTerminator
        }
    ));
    let terminal = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            match events.next_event().await {
                Some(Err(error)) => break error,
                Some(Ok(_)) => {}
                None => panic!("stream omitted its terminal failure"),
            }
        }
    })
    .await
    .expect("stream yields the original terminal failure");
    assert!(matches!(
        terminal,
        Error::ControlProtocol {
            fault: ControlFault::WrongTerminator
        }
    ));
    assert!(events.next_event().await.is_none());
    events.shutdown().await.unwrap();
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn server_shutdown_owns_live_control_clients() {
    let fixture = Fixture::new("quiet", "3.8", TEST_TIMEOUT);
    let (sender, events) = fixture.attach().await.unwrap().split();
    let child = fixture.child().await;
    fixture.server.shutdown().await.unwrap();
    assert!(
        sender
            .send(Command::new("list-panes").arg("-F").arg("#{pane_id}"))
            .await
            .is_err()
    );
    assert!(matches!(
        events.shutdown().await,
        Err(Error::ExecutorShutdown { .. })
    ));
    gone(child).await;
}

#[cfg(feature = "plan")]
#[tokio::test]
async fn control_creation_retains_its_first_receipt_across_later_failures() {
    for mode in [
        "creation-input-failure",
        "creation-hook-failure",
        "creation-bad-tuple",
        "creation-timeout",
    ] {
        let fixture = Fixture::new(mode, "3.8", TEST_TIMEOUT);
        let (sender, events) = fixture
            .attach()
            .await
            .unwrap()
            .reply_timeout(Duration::from_millis(100))
            .split();
        let mut plan = crate::plan::Plan::new();
        plan.add(crate::plan::NewSession::new("created"));
        let error = sender.run_plan(&plan).await.unwrap_err();
        assert_eq!(
            error.operation_effect(),
            Some(libtmux_core::Effect::Created),
            "{mode}: {error:?}"
        );
        assert_eq!(
            error.creation_receipt().unwrap().session().to_string(),
            "$2"
        );
        assert_eq!(error.kind(), ErrorKind::PartialEffect);
        assert!(!error.is_transient());
        assert!(!format!("{error:?} {error}").contains("private-later-failure"));
        let _ = events.shutdown().await;
        fixture.server.shutdown().await.unwrap();
    }
}

#[cfg(feature = "plan")]
#[tokio::test]
async fn an_incomplete_control_creation_row_never_becomes_a_receipt() {
    let fixture = Fixture::new("creation-before-receipt", "3.8", TEST_TIMEOUT);
    let (sender, events) = fixture
        .attach()
        .await
        .unwrap()
        .reply_timeout(Duration::from_millis(100))
        .split();
    let mut plan = crate::plan::Plan::new();
    plan.add(crate::plan::NewSession::new("uncertain"));
    let error = sender.run_plan(&plan).await.unwrap_err();
    assert_eq!(
        error.operation_effect(),
        Some(libtmux_core::Effect::Unknown)
    );
    assert!(error.creation_receipt().is_none());
    assert!(!error.is_transient());
    let _ = events.shutdown().await;
    fixture.server.shutdown().await.unwrap();
}

#[cfg(feature = "plan")]
#[tokio::test]
async fn aborting_the_actor_retains_only_a_completed_verified_creation() {
    for (mode, complete) in [
        ("creation-timeout", true),
        ("creation-before-receipt", false),
        ("creation-wrong-generation", false),
    ] {
        let fixture = Fixture::new(mode, "3.8", TEST_TIMEOUT);
        let (sender, events) = fixture.attach().await.unwrap().split();
        let child = fixture.child().await;
        let creating = tokio::spawn(async move {
            sender
                .create_session(libtmux_core::NewSession::new("actor-abort"))
                .await
        });
        tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                if if complete {
                    events.events.creation_receipt_ready()
                } else {
                    events.events.creation_input_seen()
                } {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the actor observes this request's bytes");
        events.connection.as_ref().unwrap().abort();
        let error = creating.await.unwrap().unwrap_err();
        assert_eq!(
            error.operation_effect(),
            Some(if complete {
                libtmux_core::Effect::Created
            } else {
                libtmux_core::Effect::Unknown
            })
        );
        assert_eq!(error.creation_receipt().is_some(), complete);
        assert!(!error.is_transient());
        let _ = events.shutdown().await;
        fixture.server.shutdown().await.unwrap();
        gone(child).await;
    }
}

#[cfg(feature = "plan")]
#[tokio::test]
async fn a_verified_control_creation_receipt_binds_the_shared_session_value() {
    let fixture = Fixture::new("creation-success", "3.8", TEST_TIMEOUT);
    let (sender, events) = fixture.attach().await.unwrap().split();
    let mut plan = crate::plan::Plan::new();
    plan.add(crate::plan::NewSession::new("created"));
    let result = sender.run_plan(&plan).await.unwrap();
    assert!(result.is_complete());
    assert_eq!(result.created(0).unwrap(), "$2");
    events.shutdown().await.unwrap();
    fixture.server.shutdown().await.unwrap();
}

#[cfg(feature = "plan")]
#[tokio::test]
async fn unrepresentable_control_creation_uses_one_native_operation() {
    let fixture = Fixture::new("quiet", "3.8", TEST_TIMEOUT);
    let (sender, events) = ControlMode::attach_with_limits(
        &fixture.server,
        &session(),
        crate::ControlLimits::default().max_line_bytes(512),
    )
    .await
    .unwrap()
    .split();
    for (index, (request, argument)) in [
        (
            libtmux_core::NewSession::new("raw-environment")
                .environment("LIBTMUX_RAW", OsString::from_vec(b"native-\xff".to_vec())),
            b"LIBTMUX_RAW=native-\xff".to_vec(),
        ),
        (
            libtmux_core::NewSession::new("x".repeat(512)),
            vec![b'x'; 512],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let receipt = sender.create_session(request).await.unwrap();
        assert_eq!(receipt.session().to_string(), "$2");
        assert_eq!(receipt.generation(), sender.generation());
        assert_eq!(
            fs::read_to_string(fixture.path("native-creations"))
                .unwrap()
                .lines()
                .count(),
            index + 1
        );
        let argv = fs::read(fixture.path("native-argv")).unwrap();
        assert!(argv.split(|byte| *byte == 0).any(|word| word == argument));
    }
    assert!(
        sender
            .send(Command::new("display-message").arg("-p").arg("second"))
            .await
            .unwrap()
            .succeeded()
    );
    events.shutdown().await.unwrap();
    fixture.server.shutdown().await.unwrap();
}

#[cfg(feature = "test-support")]
async fn control_qualified(server: &Server) -> bool {
    server
        .daemon_version()
        .await
        .unwrap()
        .has_behavior(&crate::since::CONTROL_ACQUISITION)
}
#[cfg(feature = "test-support")]
#[tokio::test]
async fn real_tmux_bootstrap_cleanup_preserves_frame_failure() {
    let guard = crate::test::TestServer::builder()
        .start()
        .await
        .expect("tmux starts");
    let server = guard.server();
    if !control_qualified(server).await {
        guard.shutdown().await.expect("ineligible fixture closes");
        return;
    }
    let receipt = server
        .new_session("frame-closed")
        .await
        .expect("session starts");
    let session = server
        .inspect_session(&receipt)
        .await
        .expect("inspect created session");
    let pane = session.panes().await.expect("panes list").remove(0);
    let (sender, events) = ControlMode::attach_with_limits(
        server,
        session.id(),
        crate::ControlLimits::default().max_line_bytes(512),
    )
    .await
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
        .await
        .expect("producer starts");
    assert!(produced.success(), "producer is accepted: {produced:?}");
    tokio::time::timeout(Duration::from_secs(1), sender.commands.closed())
        .await
        .expect("oversized output closes admission");
    let send_error = sender
        .watch_only(&[])
        .await
        .expect_err("narrowing is refused");
    drop(sender);
    assert!(matches!(
        send_error,
        Error::ControlMode {
            kind: ControlModeErrorKind::Closed,
            ..
        }
    ));
    let error = events.shutdown_after_error(send_error).await;
    guard.shutdown().await.expect("fixture shuts down");
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

/// Acquiring control reads the daemon's identity first. That read must not
/// have effects: `display-message` fires `after-display-message` on every
/// release, before this client has decided to attach.
#[cfg(feature = "test-support")]
#[tokio::test]
async fn real_tmux_control_acquisition_fires_no_user_hook() {
    let guard = crate::test::TestServer::builder()
        .start()
        .await
        .expect("tmux starts");
    let server = guard.server();
    if !control_qualified(server).await {
        guard.shutdown().await.expect("ineligible fixture closes");
        return;
    }
    let receipt = server.new_session("hooked").await.expect("session starts");
    let session = server
        .inspect_session(&receipt)
        .await
        .expect("inspect created session");
    let hook = server
        .cmd(
            Command::new("set-hook")
                .arg("-g")
                .arg("after-display-message")
                .arg("set-option -g @after_display_message fired"),
        )
        .await
        .expect("hook is set");
    assert!(hook.success(), "{hook:?}");
    let read = || async {
        let shown = server
            .cmd(
                Command::new("show-options")
                    .arg("-gqv")
                    .arg("@after_display_message"),
            )
            .await
            .expect("option reads");
        shown.stdout_lossy().trim().to_owned()
    };

    // Control: the hook is live, so an empty value below means acquisition
    // did not fire it rather than that the hook never worked.
    server
        .cmd(Command::new("display-message").arg("-p").arg("x"))
        .await
        .expect("display-message runs");
    assert_eq!(read().await, "fired");
    server
        .cmd(
            Command::new("set-option")
                .arg("-g")
                .arg("@after_display_message")
                .arg(""),
        )
        .await
        .expect("marker resets");

    let control = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches");
    drop(control);

    let after = read().await;
    guard.shutdown().await.expect("fixture shuts down");
    assert_eq!(after, "", "control acquisition must not fire the hook");
}
