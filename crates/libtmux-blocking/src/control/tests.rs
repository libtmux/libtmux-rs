use super::*;
use crate::native_contracts::Fixture;
use libtmux_core::Effect;

fn managed() -> Option<(Fixture, ControlMode)> {
    let fixture = Fixture::new();
    let output = fixture.checked(["display-message", "-p", "#{version}"]);
    let version = libtmux_core::TmuxVersion::parse_output(
        format!("tmux {}", String::from_utf8(output.stdout).unwrap()).as_bytes(),
    )
    .unwrap();
    if !version.managed_control_eligible(true) {
        return None;
    }
    let mode = ControlMode::attach(&fixture.server, "$0".parse().unwrap()).unwrap();
    Some((fixture, mode))
}

#[test]
fn native_attachment_challenges_identity_and_preserves_reply_blocks() {
    let Some((_fixture, mode)) = managed() else {
        return;
    };
    assert!(mode.sender.shared.lock().tracker.is_qualified());
    let reply = mode
        .send(
            Command::new("if-shell")
                .arg("-F")
                .arg("1")
                .arg("display-message -p outer ; display-message -p inner"),
        )
        .unwrap();
    assert!(reply.succeeded());
    let bytes: Vec<_> = reply
        .blocks()
        .iter()
        .flat_map(|block| block.output().iter().copied())
        .collect();
    assert_eq!(bytes, b"outer\ninner\n");
    let failed = mode.send(Command::new("libtmux-no-such-command")).unwrap();
    assert!(!failed.succeeded());
    let next = mode
        .send(Command::new("display-message").arg("-p").arg("next"))
        .unwrap();
    assert_eq!(next.blocks()[0].output(), b"next\n");
    mode.shutdown().unwrap();
}

#[test]
fn notification_shaped_payload_is_not_a_flow_notification() {
    let Some((_fixture, mode)) = managed() else {
        return;
    };
    mode.sender.shared.lock().tracker.discard_notices();
    let reply = mode
        .send(
            Command::new("display-message")
                .arg("-p")
                .arg("-l")
                .arg("%pause %0"),
        )
        .unwrap();
    assert_eq!(reply.blocks()[0].output(), b"%pause %0\n");
    let mut state = mode.sender.shared.lock();
    while let Some(notice) = state.tracker.next_notice() {
        assert!(!matches!(
            notice,
            ControlNotice::Event(Event::Paused { .. })
        ));
    }
    drop(state);
    mode.shutdown().unwrap();
}

#[test]
fn cancellation_drains_its_place_before_later_submission() {
    let Some((fixture, mode)) = managed() else {
        return;
    };
    fixture.checked(["wait-for", "-L", "private-drain"]);
    let (sender, _events) = mode.split();
    let clone = sender.clone();
    let options = RequestOptions::default();
    let cancellation = options.cancellation.clone();
    let task = std::thread::spawn(move || {
        clone.send_with(
            Command::new("wait-for").arg("-L").arg("private-drain"),
            options,
        )
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if sender.shared.available.load(Ordering::Acquire)
            < sender.shared.limits.max_pending_requests
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(INTERVAL);
    }
    std::thread::sleep(Duration::from_millis(30));
    cancellation.cancel();
    let error = task.join().unwrap().unwrap_err();
    assert_eq!(error.effect(), Effect::Unknown);
    fixture.checked(["wait-for", "-U", "private-drain"]);
    let next = sender
        .send(Command::new("display-message").arg("-p").arg("later"))
        .unwrap();
    assert_eq!(next.blocks()[0].output(), b"later\n");
    sender.shutdown().unwrap();
}

#[test]
fn cancelled_unsubmitted_request_cannot_reach_the_connection() {
    let Some((_fixture, mode)) = managed() else {
        return;
    };
    let options = RequestOptions::default();
    options.cancellation.cancel();
    let error = mode
        .sender
        .send_with(
            Command::new("display-message").arg("-p").arg("refused"),
            options,
        )
        .unwrap_err();
    assert_eq!(error.effect(), Effect::NotSubmitted);
    mode.shutdown().unwrap();
}

#[test]
fn managed_control_accepts_a_compatible_client_older_than_the_daemon_floor() {
    let Some(client) = std::env::var_os("LIBTMUX_TEST_CONTROL_CLIENT") else {
        assert!(
            !std::env::var("LIBTMUX_REQUIRE_NATIVE")
                .unwrap_or_default()
                .split(',')
                .any(|name| name == "LIBTMUX_TEST_CONTROL_CLIENT")
        );
        return;
    };
    let fixture = Fixture::new();
    let binary = std::process::Command::new(&client)
        .arg("-V")
        .output()
        .unwrap();
    let version = libtmux_core::TmuxVersion::parse_output(&binary.stdout).unwrap();
    assert!(
        !version.managed_control_eligible(true),
        "the test needs an older client"
    );
    let transport = fixture.server.clone().with_executable(client);
    let mode = ControlMode::attach(&transport, "$0".parse().unwrap()).unwrap();
    let reply = mode
        .send(
            Command::new("display-message")
                .arg("-p")
                .arg("#{session_id}"),
        )
        .unwrap();
    assert_eq!(reply.blocks()[0].output(), b"$0\n");
    mode.shutdown().unwrap();
}

#[test]
fn managed_control_refuses_the_ordinary_supported_floor() {
    let fixture = Fixture::new();
    let output = fixture.checked(["display-message", "-p", "#{version}"]);
    let version = libtmux_core::TmuxVersion::parse_output(
        format!("tmux {}", String::from_utf8(output.stdout).unwrap()).as_bytes(),
    )
    .unwrap();
    if version.managed_control_eligible(true) {
        return;
    }
    let error = ControlMode::attach(&fixture.server, "$0".parse().unwrap()).unwrap_err();
    assert_eq!(error.effect(), Effect::NotSubmitted);
}

#[test]
fn control_input_rejects_record_boundaries_nul_and_non_utf8_operands() {
    use std::os::unix::ffi::OsStringExt as _;
    for bytes in [
        b"before\nafter".as_slice(),
        b"before\rafter",
        b"before\0after",
        b"before\xffafter",
    ] {
        let command = Command::new("set-environment")
            .arg("-g")
            .arg("VALUE")
            .sensitive_arg(std::ffi::OsString::from_vec(bytes.to_vec()));
        let error = wire(&command, CreationNonce::from_bytes([0x33; 16]), 1024).unwrap_err();
        assert_eq!(error.fault(), Some(ControlFault::InvalidCommand));
        assert_eq!(error.effect(), Effect::NotSubmitted);
    }
}

#[test]
fn bounded_notices_preserve_private_qualification_and_pause_gaps() {
    let fixture = Fixture::new();
    let identity = probe(
        &fixture.server,
        "$0".parse().unwrap(),
        &RequestOptions::default(),
    );
    if identity.is_err() {
        return;
    }
    let mode = ControlMode::attach_with_limits(
        &fixture.server,
        "$0".parse().unwrap(),
        ControlLimits {
            max_events: 0,
            max_event_bytes: 0,
            ..ControlLimits::default()
        },
    )
    .unwrap();
    mode.sender.pause_pane("%0".parse().unwrap()).unwrap();
    let reply = mode
        .send(Command::new("display-message").arg("-p").arg("unblocked"))
        .unwrap();
    assert_eq!(reply.blocks()[0].output(), b"unblocked\n");
    assert!(matches!(
        mode.sender.shared.lock().tracker.next_notice(),
        Some(ControlNotice::Gap {
            included_pause: true,
            ..
        })
    ));
    mode.sender.continue_pane("%0".parse().unwrap()).unwrap();
    mode.shutdown().unwrap();
}

struct ProtocolFixture {
    root: tempfile::TempDir,
    _socket: std::os::unix::net::UnixListener,
    server: Server,
}

impl ProtocolFixture {
    fn new(scenario: &str) -> Self {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create("/tmp/libtmux-rs-test")
            .unwrap();
        let root = tempfile::Builder::new()
            .prefix("blocking-control-")
            .tempdir_in("/tmp/libtmux-rs-test")
            .unwrap();
        let socket = std::os::unix::net::UnixListener::bind(root.path().join("s")).unwrap();
        let executable = root.path().join("protocol-client");
        let source = PROTOCOL_FIXTURE
            .replace("@ROOT@", root.path().to_str().unwrap())
            .replace("@SCENARIO@", scenario);
        std::fs::write(&executable, source).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let server = Server::at(root.path().join("s"))
            .unwrap()
            .with_executable(executable);
        Self {
            root,
            _socket: socket,
            server,
        }
    }

    fn attach(&self, limits: ControlLimits) -> ControlMode {
        ControlMode::attach_with_limits(&self.server, "$0".parse().unwrap(), limits).unwrap()
    }

    fn wait_for(&self, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.root.path().join(name).exists() {
            assert!(Instant::now() < deadline, "missing fixture marker {name}");
            std::thread::sleep(INTERVAL);
        }
    }

    #[cfg(target_os = "linux")]
    fn reaped(&self) {
        let pid = std::fs::read_to_string(self.root.path().join("pid")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while std::path::Path::new("/proc").join(pid.trim()).exists() {
            assert!(Instant::now() < deadline, "control child was not reaped");
            std::thread::sleep(INTERVAL);
        }
    }
}

#[test]
fn cancelled_reply_flood_still_closes_at_the_original_byte_limit() {
    let fixture = ProtocolFixture::new("flood");
    let mode = fixture.attach(ControlLimits::default());
    let mut options = RequestOptions::default();
    options.budget.bytes_per_stream = 512;
    let cancellation = options.cancellation.clone();
    let sender = mode.sender.clone();
    let task =
        std::thread::spawn(move || sender.send_with(Command::new("display-message"), options));
    fixture.wait_for("started");
    cancellation.cancel();
    assert_eq!(task.join().unwrap().unwrap_err().effect(), Effect::Unknown);
    std::fs::write(fixture.root.path().join("release"), b"go").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !mode.sender.shared.stop.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(INTERVAL);
    }
    let error = mode.shutdown().unwrap_err();
    assert_eq!(error.fault(), Some(ControlFault::ReplyLimit { limit: 512 }));
    #[cfg(target_os = "linux")]
    fixture.reaped();
}

#[test]
fn partial_writes_cancel_then_close_and_reap_without_replay() {
    let fixture = ProtocolFixture::new("stall-writes");
    let mode = fixture.attach(ControlLimits {
        drain_timeout: Duration::from_millis(60),
        ..ControlLimits::default()
    });
    let options = RequestOptions::default();
    let cancellation = options.cancellation.clone();
    let sender = mode.sender.clone();
    let task = std::thread::spawn(move || {
        sender.send_with(
            Command::new("display-message").arg("x".repeat(512 * 1024)),
            options,
        )
    });
    std::thread::sleep(Duration::from_millis(30));
    cancellation.cancel();
    assert_eq!(task.join().unwrap().unwrap_err().effect(), Effect::Unknown);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !mode.sender.shared.stop.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(INTERVAL);
    }
    assert_eq!(
        mode.shutdown().unwrap_err().fault(),
        Some(ControlFault::Deadline)
    );
    #[cfg(target_os = "linux")]
    fixture.reaped();
}

#[test]
fn last_handle_drop_stops_the_owned_client() {
    let fixture = ProtocolFixture::new("ordinary");
    let mode = fixture.attach(ControlLimits::default());
    let shared = Arc::clone(&mode.sender.shared);
    let (sender, events) = mode.split();
    drop(events);
    assert_eq!(
        sender
            .send(Command::new("display-message"))
            .unwrap()
            .blocks()[0]
            .output(),
        b"OK\n"
    );
    drop(sender);
    shared.shutdown().unwrap();
    #[cfg(target_os = "linux")]
    fixture.reaped();
}

#[test]
fn failed_attached_identity_never_exposes_a_sender() {
    let fixture = ProtocolFixture::new("wrong-generation");
    let error = ControlMode::attach(&fixture.server, "$0".parse().unwrap()).unwrap_err();
    assert_eq!(error.fault(), Some(ControlFault::InvalidChallenge));
    assert_eq!(error.effect(), Effect::NotSubmitted);
    #[cfg(target_os = "linux")]
    fixture.reaped();
}

#[test]
fn queued_cancellation_retains_admission_until_it_is_proved_unsubmitted() {
    let fixture = ProtocolFixture::new("park");
    let mode = fixture.attach(ControlLimits {
        max_pending_requests: 2,
        ..ControlLimits::default()
    });
    let first = mode.sender.clone();
    let active = std::thread::spawn(move || first.send(Command::new("park")));
    fixture.wait_for("started");
    let options = RequestOptions::default();
    let cancellation = options.cancellation.clone();
    let second = mode.sender.clone();
    let queued =
        std::thread::spawn(move || second.send_with(Command::new("must-not-run"), options));
    let deadline = Instant::now() + Duration::from_secs(2);
    while mode.sender.shared.available.load(Ordering::Acquire) != 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(INTERVAL);
    }
    let full = mode.send(Command::new("over-capacity")).unwrap_err();
    assert_eq!(full.effect(), Effect::NotSubmitted);
    cancellation.cancel();
    assert_eq!(
        queued.join().unwrap().unwrap_err().effect(),
        Effect::NotSubmitted
    );
    std::fs::write(fixture.root.path().join("release"), b"go").unwrap();
    assert!(active.join().unwrap().unwrap().succeeded());
    assert_eq!(
        mode.send(Command::new("later")).unwrap().blocks()[0].output(),
        b"OK\n"
    );
    assert!(!fixture.root.path().join("unexpected").exists());
    mode.shutdown().unwrap();
}

const PROTOCOL_FIXTURE: &str = r#"#!/usr/bin/python3
import os
import re
import sys
import time
from pathlib import Path

root = Path('@ROOT@')
scenario = '@SCENARIO@'
if '-V' in sys.argv:
    print('tmux 3.8')
    sys.exit(0)
if 'attach-session' not in sys.argv:
    nonce = re.search(r'challenge:([0-9a-f]{32}):', sys.argv[-1]).group(1)
    os.write(2, f'unknown command: libtmux-control-challenge:{nonce}:3.8,123,456,$0\n'.encode())
    sys.exit(1)

(root / 'pid').write_text(str(os.getpid()))
os.write(1, b'%begin 0 1 0\n%end 0 1 0\n%session-changed $0 fixture\n')
number = 2
challenged = False

def block(payload=b'', success=True):
    global number
    os.write(1, f'%begin 0 {number} 1\n'.encode() + payload + f'%{"end" if success else "error"} 0 {number} 1\n'.encode())
    number += 1

for line in sys.stdin:
    challenge = re.search(r'libtmux-control-challenge:([0-9a-f]{32}):', line)
    fence = re.search(r'libtmux-control-fence:([0-9a-f]{32})', line)
    if challenge:
        block()
        started = 457 if scenario == 'wrong-generation' else 456
        os.write(1, f'unknown command: libtmux-control-challenge:{challenge.group(1)}:3.8,123,{started},$0\n'.encode())
        challenged = True
    elif fence:
        block()
        os.write(1, f'unknown command: libtmux-control-fence:{fence.group(1)}\n'.encode())
        if challenged and scenario == 'stall-writes':
            time.sleep(30)
    elif scenario == 'flood':
        os.write(1, f'%begin 0 {number} 1\n'.encode())
        (root / 'started').touch()
        while not (root / 'release').exists():
            time.sleep(0.002)
        os.write(1, b'x' * 2048)
        time.sleep(30)
    elif scenario == 'park' and 'park' in line:
        (root / 'started').touch()
        while not (root / 'release').exists():
            time.sleep(0.002)
        block(b'OK\n')
    else:
        if 'must-not-run' in line:
            (root / 'unexpected').touch()
        block(b'OK\n')
"#;
