use std::ffi::{OsStr, OsString};
use std::fs::DirBuilder;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use libtmux_core::{CommandKind, Error, NewSession, PaneKey};

use crate::{RequestOptions, Server};

pub(super) struct Fixture {
    root: tempfile::TempDir,
    executable: OsString,
    pub(super) server: Server,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let fixture = Self::empty();
        fixture.start();
        fixture.checked(["set-option", "-g", "status", "off"]);
        fixture
    }

    fn empty() -> Self {
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
        Self {
            root,
            executable,
            server,
        }
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

    pub(super) fn checked(&self, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Output {
        let output = self.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "fixture command failed: {output:?}"
        );
        output
    }

    #[track_caller]
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

    fn isolated_client(&self, reported_version: Option<&str>) -> OsString {
        use std::os::unix::ffi::OsStrExt as _;

        let executable = self.root.path().join("ordinary-client");
        std::fs::write(
            self.root.path().join("tmux-path"),
            self.executable.as_bytes(),
        )
        .unwrap();
        if let Some(version) = reported_version {
            std::fs::write(self.root.path().join("version"), version).unwrap();
        }
        std::fs::write(&executable, b"#!/usr/bin/env python3\nimport os,sys\nfrom pathlib import Path\nroot=Path(__file__).parent\nif '-V' in sys.argv[1:] and (root/'version').exists():\n print('tmux '+(root/'version').read_text()); sys.exit(0)\nprogram=os.fsdecode((root/'tmux-path').read_bytes())\nos.execvp(program,[program,'-f','/dev/null',*sys.argv[1:]])\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        executable.into_os_string()
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
fn observing_a_generation_leaves_the_display_hook_untouched() {
    let fixture = Fixture::new();
    let expected = fixture.screen().generation();
    fixture.checked([
        "set-hook",
        "-g",
        "after-display-message",
        "set-environment -g HOOK_FIRED yes",
    ]);
    assert_eq!(fixture.server.generation().unwrap(), expected);
    let variable = || {
        fixture
            .command()
            .args(["show-environment", "-g", "HOOK_FIRED"])
            .output()
            .unwrap()
    };
    assert!(!variable().status.success());
    fixture.checked(["display-message", "-p", "#{pid}"]);
    assert_eq!(variable().stdout, b"HOOK_FIRED=yes\n");
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
    let error = observed
        .decode_capture(output.status.code(), output.stdout, &output.stderr, late)
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Command {
            operation: CommandKind::CapturePane,
            status: Some(1),
            ..
        }
    ));
    assert_eq!(error.effect(), libtmux_core::Effect::Observation);
    assert_eq!(fixture.server.observe_pane(key.id()).unwrap(), key);
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

#[test]
fn a_cold_endpoint_uses_only_a_supported_executable_and_a_version_fence() {
    let fixture = Fixture::empty();
    let server = fixture
        .server
        .clone()
        .with_executable(fixture.isolated_client(None));
    let created = server
        .new_session(NewSession::new("cold").command("exec sleep 30"))
        .unwrap();
    assert_eq!(created.session().to_string(), "$0");
    assert_eq!(
        server.observe_pane(created.pane().id()).unwrap(),
        *created.pane()
    );
}

#[test]
fn a_pre_floor_executable_report_is_refused_before_creation() {
    let fixture = Fixture::empty();
    let server = fixture
        .server
        .clone()
        .with_executable(fixture.isolated_client(Some("3.3")));
    let error = server
        .new_session(NewSession::new("must-not-exist"))
        .unwrap_err();
    assert_eq!(error.effect(), libtmux_core::Effect::NotSubmitted);
    assert!(matches!(error, Error::Version(_)));
    assert!(!fixture.root.path().join("s").exists());
}

#[test]
fn a_probe_error_is_not_permission_to_start_a_new_daemon() {
    let fixture = Fixture::empty();
    let component = fixture.root.path().join("ordinary-file");
    std::fs::write(&component, b"not a directory").unwrap();
    let server = Server::at(component.join("socket"))
        .unwrap()
        .with_executable(fixture.executable.clone());
    let error = server
        .new_session(NewSession::new("must-not-exist"))
        .unwrap_err();
    assert_eq!(error.effect(), libtmux_core::Effect::NotSubmitted);
    assert!(!matches!(error, Error::UnconfirmedCreation { .. }));
}

#[test]
fn a_stale_version_witness_cannot_create_on_the_replacement_version() {
    let fixture = Fixture::new();
    let actual = fixture.checked(["-V"]);
    let actual =
        libtmux_core::TmuxVersion::parse_executable_output(&fixture.executable, &actual.stdout)
            .unwrap();
    let floor = libtmux_core::TmuxVersion::parse_output(b"tmux 3.3a\n").unwrap();
    let expected = if actual == floor {
        libtmux_core::TmuxVersion::parse_output(b"tmux 3.8\n").unwrap()
    } else {
        floor
    };
    let error = fixture
        .server
        .create_qualified(
            NewSession::new("must-not-exist").command("exec sleep 30"),
            RequestOptions::default(),
            &expected,
            false,
        )
        .unwrap_err();
    assert_eq!(error.effect(), libtmux_core::Effect::Unknown);
    assert!(matches!(error, Error::UnconfirmedCreation { .. }));
    assert_eq!(
        fixture
            .checked(["list-sessions", "-F", "#{session_name}"])
            .stdout,
        b"fixture\n"
    );
}

#[test]
fn a_parked_creation_hook_preserves_the_real_ordinary_receipt() {
    let fixture = Fixture::new();
    fixture.checked([
        "set-hook",
        "-g",
        "after-new-session",
        "wait-for ordinary-receipt-gate",
    ]);
    let options = RequestOptions {
        budget: libtmux_core::supervision::Budget {
            deadline: Instant::now() + Duration::from_millis(500),
            bytes_per_stream: 65_536,
        },
        cancellation: crate::CancellationToken::default(),
    };
    let result = fixture.server.new_session_with(
        NewSession::new("created-before-timeout").command("exec sleep 30"),
        options,
    );
    fixture.checked(["wait-for", "-S", "ordinary-receipt-gate"]);
    let error = result.unwrap_err();
    assert_eq!(error.effect(), libtmux_core::Effect::Created);
    let Error::CreationInterrupted { receipt, source } = error else {
        panic!("{error}")
    };
    assert!(matches!(*source, Error::Deadline));
    assert_eq!(
        fixture.server.observe_pane(receipt.pane().id()).unwrap(),
        *receipt.pane()
    );
}

#[test]
fn cleanup_cannot_kill_a_replacement_session_with_the_same_id() {
    let fixture = Fixture::new();
    let original = fixture
        .server
        .new_session(NewSession::new("original"))
        .unwrap();
    fixture.checked(["kill-server"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let probe = fixture.command().arg("list-sessions").output().unwrap();
        let diagnostic = String::from_utf8_lossy(&probe.stderr);
        if diagnostic.contains("no server running") || diagnostic.contains("error connecting") {
            break;
        }
        assert!(Instant::now() < deadline, "original server still reachable");
        std::thread::sleep(Duration::from_millis(5));
    }
    fixture.start();
    let replacement = fixture
        .server
        .new_session(NewSession::new("replacement"))
        .unwrap();
    assert_eq!(original.session(), replacement.session());
    assert_ne!(original.generation(), replacement.generation());
    assert!(fixture.server.kill_created_session(&original).is_err());
    assert_eq!(
        fixture
            .checked([
                "display-message",
                "-p",
                "-t",
                replacement.session().as_ref(),
                "#{session_name}"
            ])
            .stdout,
        b"replacement\n"
    );
    fixture.server.kill_created_session(&replacement).unwrap();
}

#[test]
fn snapshots_preserve_non_ascii_session_names_in_the_c_locale() {
    use libtmux_core::SessionSnapshot;

    if std::env::var_os("LC_ALL").as_deref() != Some(OsStr::new("C")) {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_contracts::snapshots_preserve_non_ascii_session_names_in_the_c_locale",
            ])
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert!(output.status.success(), "C-locale snapshot: {output:?}");
        return;
    }

    let fixture = Fixture::new();
    fixture.checked(["rename-session", "-t", "fixture", "café"]);
    let snapshots = fixture.server.snapshots::<SessionSnapshot>().unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].session_name().as_bytes(), "café".as_bytes());
}

#[test]
fn snapshots_keep_the_observed_session_name_and_count_a_linked_window_once() {
    use libtmux_core::{ClientSnapshot, PaneSnapshot, SessionSnapshot, WindowSnapshot};

    let fixture = Fixture::new();
    let original = fixture
        .server
        .snapshots::<SessionSnapshot>()
        .unwrap()
        .remove(0);
    let peer = fixture.server.new_session(NewSession::new("peer")).unwrap();
    fixture.checked(["link-window", "-s", "fixture:0", "-t", "peer:7"]);
    fixture.checked(["rename-session", "-t", "fixture", "renamed"]);
    assert_eq!(original.session_name().as_bytes(), b"fixture");
    let current = fixture.server.snapshots::<SessionSnapshot>().unwrap();
    let renamed = current
        .iter()
        .find(|s| s.session_id() == original.session_id())
        .unwrap();
    assert_eq!(renamed.session_name().as_bytes(), b"renamed");
    let windows = fixture.server.snapshots::<WindowSnapshot>().unwrap();
    assert_eq!(windows.len(), 2);
    assert!(windows.iter().any(|w| w.window_id() == &peer.window()));
    assert_eq!(fixture.server.snapshots::<PaneSnapshot>().unwrap().len(), 2);
    assert!(
        fixture
            .server
            .snapshots::<ClientSnapshot>()
            .unwrap()
            .is_empty()
    );
}
