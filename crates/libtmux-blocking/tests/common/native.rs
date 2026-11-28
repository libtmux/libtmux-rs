use std::ffi::{OsStr, OsString};
use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use libtmux_core::{CommandKind, Error, NewSession, PaneKey};

use crate::{RequestOptions, Server};

struct Fixture {
    root: tempfile::TempDir,
    executable: OsString,
    server: Server,
}

impl Fixture {
    fn new() -> Self {
        let fixture_root = Path::new("/tmp/libtmux-rs-test");
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(fixture_root)
            .unwrap();
        let root = tempfile::Builder::new()
            .prefix("blocking-native-")
            .tempdir_in(fixture_root)
            .unwrap();
        let executable = std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into());
        let server = Server::at(root.path().join("s"))
            .unwrap()
            .with_executable(executable.clone());
        let fixture = Self {
            root,
            executable,
            server,
        };
        fixture.start();
        fixture.checked(["set-option", "-g", "status", "off"]);
        fixture
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .args(["-N", "-S"])
            .arg(self.root.path().join("s"))
            .args(["-f", "/dev/null"])
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::null());
        command
    }

    fn checked(&self, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Output {
        let output = self.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "fixture command failed: {output:?}"
        );
        output
    }

    fn start(&self) {
        let output = Command::new(&self.executable)
            .arg("-S")
            .arg(self.root.path().join("s"))
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "fixture",
                "sleep 30",
            ])
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "fixture starts: {output:?}");
    }

    fn screen(&self) -> PaneKey {
        let created = self
            .server
            .new_session(
                NewSession::new("screen")
                    .size(12, 4)
                    .command("printf 'abcdefghijklmno\\n'; exec sleep 30"),
            )
            .unwrap();
        let key = created.pane().clone();
        let end = Instant::now() + Duration::from_secs(3);
        loop {
            let bytes = self.server.capture(&key).unwrap();
            if bytes == b"abcdefghijkl\nmno\n\n\n" {
                break;
            }
            assert!(
                Instant::now() < end,
                "screen did not become ready: {bytes:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        key
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.command().arg("kill-server").output();
    }
}

#[test]
fn real_tmux_creation_observation_and_capture_share_object_identity() {
    let fixture = Fixture::new();
    let key = fixture.screen();
    let observed = fixture.server.observe_pane(key.id()).unwrap();
    assert_eq!(observed, key);
    assert_eq!(
        fixture.server.capture(&observed).unwrap(),
        b"abcdefghijkl\nmno\n\n\n"
    );
    fixture.checked([
        OsString::from("copy-mode"),
        OsString::from("-t"),
        key.id().to_string().into(),
    ]);
    assert_eq!(
        fixture.server.capture(&observed).unwrap(),
        b"abcdefghijkl\nmno\n\n\n"
    );
}

#[test]
fn successful_capture_hook_output_cannot_become_screen_bytes() {
    let fixture = Fixture::new();
    let key = fixture.screen();
    fixture.checked([
        "set-hook",
        "-g",
        "after-capture-pane",
        "display-message -p 'private-hook-output'",
    ]);
    assert_eq!(
        fixture.server.capture(&key).unwrap(),
        b"abcdefghijkl\nmno\n\n\n"
    );
}

#[test]
fn failed_capture_hook_returns_an_observation_error_without_screen_bytes() {
    let fixture = Fixture::new();
    let key = fixture.screen();
    fixture.checked([
        "set-hook",
        "-g",
        "after-capture-pane",
        "display-message -p 'private-hook-output' ; if-shell -F 1 '}'",
    ]);
    let error = fixture.server.capture(&key).unwrap_err();
    assert_eq!(error.effect(), libtmux_core::Effect::Observation);
    assert!(matches!(
        error,
        Error::Command {
            operation: CommandKind::CapturePane,
            ..
        }
    ));
    assert!(!format!("{error:?} {error}").contains("private-hook-output"));
}

#[test]
fn a_resized_viewport_fails_the_same_queue_extent_guard() {
    let fixture = Fixture::new();
    let key = fixture.screen();
    let observed = fixture
        .server
        .probe_pane(key.id(), RequestOptions::default())
        .unwrap();
    fixture.checked(["resize-window", "-t", "screen:", "-y", "7"]);
    let (output, late) = fixture
        .server
        .output(
            observed
                .capture_arguments(&fixture.server.socket, &key)
                .unwrap(),
            RequestOptions::default(),
            None,
        )
        .unwrap();
    assert!(matches!(
        observed.decode_capture(output.status.code(), output.stdout, &output.stderr, late),
        Err(Error::StalePane)
    ));
}

#[test]
#[cfg(target_os = "linux")]
fn a_replacement_daemon_cannot_reuse_an_observed_pane_key() {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::process::{Pid, PidfdFlags, pidfd_open};

    let fixture = Fixture::new();
    let key = fixture.server.observe_pane("%0".parse().unwrap()).unwrap();
    let pid = Pid::from_raw(i32::try_from(key.generation().pid()).unwrap()).unwrap();
    let daemon = pidfd_open(pid, PidfdFlags::empty()).unwrap();
    fixture.checked(["kill-server"]);
    let mut fds = [PollFd::new(&daemon, PollFlags::IN)];
    let timeout = Timespec::try_from(Duration::from_secs(3)).unwrap();
    assert!(
        poll(&mut fds, Some(&timeout)).unwrap() > 0,
        "old daemon has exited"
    );
    fixture.start();
    let replacement = fixture.server.observe_pane("%0".parse().unwrap()).unwrap();
    assert_ne!(key.generation(), replacement.generation());
    assert!(matches!(
        fixture.server.capture(&key),
        Err(Error::StalePane)
    ));
}
