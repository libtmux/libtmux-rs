//! `scripts/sandbox-runner.sh`, the Cargo runner `.cargo/config.toml` names,
//! run directly around small commands. Each test is a guard of the runner: a
//! mutation that removes it fails the test named for it. It ships nowhere: a
//! packaged crate has no runner to run.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process, kill_process_group, test_kill_process};

/// Exit status when the runner refuses to start the binary.
const REFUSED: i32 = 78;
/// Exit status when the binary passed but left a server running.
const LEFT_A_SERVER: i32 = 70;
/// A value no message may repeat.
const SENTINEL: &str = "SENTINEL-must-not-be-printed";

fn runner() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/sandbox-runner.sh")
}

fn tmux_is_available() -> bool {
    let tmux = std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into());
    Command::new(tmux)
        .arg("-V")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// What one run of the runner did.
struct Run {
    code: Option<i32>,
    stderr: String,
}

/// A parent directory the runner makes its private directories in, and files
/// the wrapped command writes its answers to.
struct Fixture {
    parent: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            parent: tempfile::tempdir().expect("scratch directory"),
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.parent.path().join(name)
    }

    fn command(&self, environment: &[(&str, &str)], script: &str) -> Command {
        let mut command = Command::new(runner());
        command
            .args(["sh", "-c", script])
            .env("TMPDIR", self.parent.path())
            .env("OUT", self.file("out"))
            .env("MARKER", self.file("marker"))
            .env("SHELL", "/bin/sh")
            .envs(environment.iter().copied())
            .stdin(Stdio::null());
        command
    }

    fn run(&self, environment: &[(&str, &str)], script: &str) -> Run {
        let output = self
            .command(environment, script)
            .output()
            .expect("the runner starts");
        Run {
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// An executable script in the parent, named `name`.
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.file(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("a script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("an executable script");
        path
    }

    fn answer(&self) -> String {
        std::fs::read_to_string(self.file("out")).unwrap_or_default()
    }

    /// The runner's private directories still in the parent.
    fn private_directories(&self) -> usize {
        std::fs::read_dir(self.parent.path())
            .expect("the parent lists")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("ltrs."))
            .count()
    }
}

/// A server a test recorded the pid of. If the runner under test did not stop
/// it, dropping this does, so a failing test leaves nothing running.
struct Recorded(i32);

impl Recorded {
    fn is_alive(&self) -> bool {
        Pid::from_raw(self.0).is_some_and(|pid| test_kill_process(pid).is_ok())
    }

    fn is_gone(&self) -> bool {
        let Some(pid) = Pid::from_raw(self.0) else {
            return true;
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if test_kill_process(pid) == Err(Errno::SRCH) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }
}

impl Drop for Recorded {
    fn drop(&mut self) {
        if let Some(pid) = Pid::from_raw(self.0) {
            if test_kill_process(pid).is_ok() {
                let _ = kill_process(pid, Signal::TERM);
            }
        }
    }
}

/// The server pid the wrapped command wrote, once it has.
fn recorded_server(fixture: &Fixture) -> Option<Recorded> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(pid) = fixture.answer().trim().parse() {
            return Some(Recorded(pid));
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

const SOCKET_VISIBLE: &str = "if [ -z \"${LIBTMUX_SOCKET+x}${LIBTMUX_SOCKET_PATH+x}\" ]; \
     then echo unset; else echo set; fi > \"$OUT\"";

#[test]
fn it_removes_the_socket_variables_before_the_binary_runs() {
    for (what, value) in [("set", SENTINEL), ("empty", "")] {
        for names in [
            &["LIBTMUX_SOCKET"][..],
            &["LIBTMUX_SOCKET_PATH"],
            &["LIBTMUX_SOCKET", "LIBTMUX_SOCKET_PATH"],
        ] {
            let fixture = Fixture::new();
            let environment: Vec<(&str, &str)> = names.iter().map(|name| (*name, value)).collect();
            let run = fixture.run(&environment, SOCKET_VISIBLE);

            assert_eq!(run.code, Some(0), "{what} {names:?}: {}", run.stderr);
            assert_eq!(fixture.answer().trim(), "unset", "{what} {names:?}");
            assert_eq!(run.stderr, "", "{what} {names:?}: a clean run says nothing");
        }
    }
}

#[test]
fn it_unsets_tmux_and_the_pane_unless_asked_to_keep_them() {
    let script =
        "if [ -z \"${TMUX+x}${TMUX_PANE+x}\" ]; then echo unset; else echo set; fi > \"$OUT\"";
    let hostile = [("TMUX", "/decoy.sock,1,0"), ("TMUX_PANE", "%9")];

    let fixture = Fixture::new();
    assert_eq!(fixture.run(&hostile, script).code, Some(0));
    assert_eq!(fixture.answer().trim(), "unset");

    let fixture = Fixture::new();
    let mut kept = hostile.to_vec();
    kept.push(("LIBTMUX_SANDBOX_KEEP_TMUX", "1"));
    assert_eq!(fixture.run(&kept, script).code, Some(0));
    assert_eq!(fixture.answer().trim(), "set", "the opt-out did nothing");
}

#[test]
fn it_gives_each_run_a_fresh_owner_only_directory_and_removes_it() {
    let script = "printf '%s\\n%s\\n' \"$TMUX_TMPDIR\" \"$LIBTMUX_TEST_SANDBOX\" > \"$OUT\"; \
                  ls -ld \"$TMUX_TMPDIR\" | cut -c1-10 >> \"$OUT\"";
    let outer = std::env::var("TMUX_TMPDIR").unwrap_or_default();
    let fixture = Fixture::new();
    let mut seen = Vec::new();
    for _ in 0..2 {
        let run = fixture.run(&[], script);
        assert_eq!(run.code, Some(0), "{}", run.stderr);

        let answer = fixture.answer();
        let lines: Vec<&str> = answer.lines().collect();
        assert_eq!(lines.len(), 3, "the command answered");
        assert_eq!(lines[0], lines[1], "TMUX_TMPDIR and the marker differ");
        assert_eq!(lines[2], "drwx------", "the directory is not owner-only");
        assert!(
            lines[0] != outer,
            "the runner reused the caller's directory"
        );
        assert!(
            Path::new(lines[0]).starts_with(fixture.parent.path()),
            "not under TMPDIR",
        );
        assert!(
            !Path::new(lines[0]).exists(),
            "the directory outlived the run"
        );
        assert_eq!(fixture.private_directories(), 0);
        seen.push(lines[0].to_owned());
    }
    assert!(
        seen[0] != seen[1],
        "two runs under one TMPDIR shared a directory"
    );
}

#[test]
fn it_refuses_a_tmpdir_too_long_for_a_socket_path() {
    let fixture = Fixture::new();
    let long = fixture.file(&"a".repeat(100));
    std::fs::create_dir(&long).expect("a long directory");
    let long = long.to_str().expect("UTF-8 path");

    let run = fixture.run(&[("TMPDIR", long)], "touch \"$MARKER\"");

    assert_eq!(run.code, Some(REFUSED), "{}", run.stderr);
    assert!(!fixture.file("marker").exists(), "the binary ran");
    assert!(run.stderr.contains("TMPDIR"), "TMPDIR is not named");
    assert!(!run.stderr.contains(long), "a value was printed");
}

#[test]
fn it_passes_the_status_of_the_binary_through() {
    let fixture = Fixture::new();
    assert_eq!(fixture.run(&[], "exit 7").code, Some(7));
    assert_eq!(fixture.run(&[], "kill -TERM $$").code, Some(143));
    let quiet = fixture.run(&[], "exit 0");
    assert_eq!(quiet.code, Some(0));
    assert_eq!(quiet.stderr, "", "a clean run says nothing");
}

#[test]
fn with_tmux_a_server_left_behind_is_stopped_and_fails_the_run() {
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    let run = fixture.run(
        &[],
        "tmux -f /dev/null new-session -d -P -F '#{pid}' > \"$OUT\"",
    );

    let server = recorded_server(&fixture);
    assert_eq!(run.code, Some(LEFT_A_SERVER), "{}", run.stderr);
    assert!(run.stderr.contains("default"), "the socket is not named");
    assert!(!run.stderr.contains(fixture.parent.path().to_str().unwrap()));
    assert!(
        server.expect("the server pid").is_gone(),
        "the server is still running"
    );
    assert_eq!(fixture.private_directories(), 0, "the directory was left");
}

#[test]
fn with_tmux_a_failed_binary_keeps_its_status_over_a_left_server() {
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    let run = fixture.run(
        &[],
        "tmux -f /dev/null new-session -d -P -F '#{pid}' > \"$OUT\"; exit 3",
    );

    let server = recorded_server(&fixture);
    assert_eq!(run.code, Some(3), "{}", run.stderr);
    assert!(run.stderr.contains("left a tmux server"), "{}", run.stderr);
    assert!(
        server.expect("the server pid").is_gone(),
        "the server is still running"
    );
}

#[test]
fn with_tmux_a_leftover_socket_file_is_not_a_left_server() {
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    let run = fixture.run(
        &[],
        "for socket in \"\" \"-S $TMUX_TMPDIR/stale.sock\"; do \
             tmux -f /dev/null $socket new-session -d && tmux $socket kill-server; \
             while tmux -N $socket list-sessions >/dev/null 2>&1; do sleep 0.1; done; \
         done",
    );

    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stderr, "", "a stopped server was reported");
    assert_eq!(fixture.private_directories(), 0);
}

#[test]
fn with_tmux_an_interrupt_still_stops_the_server() {
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    let mut command = fixture.command(
        &[],
        "tmux -f /dev/null new-session -d -P -F '#{pid}' > \"$OUT\"; sleep 60",
    );
    let child = command
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("the runner starts");
    let server = recorded_server(&fixture).expect("the server pid");

    let group = Pid::from_child(&child);
    kill_process_group(group, Signal::INT).expect("the group is interrupted");
    let output = child.wait_with_output().expect("the runner exits");

    assert!(server.is_gone(), "the server is still running");
    assert_ne!(output.status.code(), Some(0), "an interrupted run passed");
    assert!(
        output.status.signal().is_none(),
        "the runner died of the signal"
    );
    assert_eq!(fixture.private_directories(), 0, "the directory was left");
}

const START_ON_CUSTOM: &str =
    "tmux -f /dev/null -S \"$TMUX_TMPDIR/custom.sock\" new-session -d -P -F '#{pid}' > \"$OUT\"";

#[test]
fn with_tmux_a_server_on_a_socket_of_its_own_is_stopped_and_fails_the_run() {
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    let run = fixture.run(&[], START_ON_CUSTOM);

    let server = recorded_server(&fixture);
    assert_eq!(run.code, Some(LEFT_A_SERVER), "{}", run.stderr);
    assert!(run.stderr.contains("custom.sock"), "{}", run.stderr);
    assert!(
        !run.stderr.contains(fixture.parent.path().to_str().unwrap()),
        "a path outside the sandbox was printed",
    );
    assert!(
        server.expect("the server pid").is_gone(),
        "the server is still running"
    );
    assert_eq!(fixture.private_directories(), 0, "the directory was left");
}

#[test]
fn with_tmux_a_server_in_a_nested_directory_is_found() {
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    let run = fixture.run(
        &[],
        "mkdir -p \"$TMUX_TMPDIR/a/b\" && tmux -f /dev/null -S \"$TMUX_TMPDIR/a/b/deep.sock\" \
         new-session -d -P -F '#{pid}' > \"$OUT\"",
    );

    let server = recorded_server(&fixture);
    assert_eq!(run.code, Some(LEFT_A_SERVER), "{}", run.stderr);
    assert!(run.stderr.contains("a/b/deep.sock"), "{}", run.stderr);
    assert!(
        server.expect("the server pid").is_gone(),
        "the server is still running"
    );
}

#[test]
fn with_tmux_a_link_out_of_the_sandbox_is_not_followed() {
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    let elsewhere = fixture.file("elsewhere");
    std::fs::create_dir(&elsewhere).expect("a directory outside the sandbox");
    let run = fixture.run(
        &[("ELSEWHERE", elsewhere.to_str().expect("UTF-8 path"))],
        "tmux -f /dev/null -S \"$ELSEWHERE/out.sock\" new-session -d -P -F '#{pid}' > \"$OUT\"; \
         ln -s \"$ELSEWHERE\" \"$TMUX_TMPDIR/link\"",
    );

    let server = recorded_server(&fixture).expect("the server pid");
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stderr, "", "something outside the sandbox was reported");
    assert!(
        server.is_alive(),
        "a server outside the sandbox was stopped"
    );
    assert!(elsewhere.join("out.sock").exists(), "the link was followed");
    assert_eq!(fixture.private_directories(), 0);
}

#[test]
fn with_tmux_a_server_that_survives_kill_server_keeps_the_directory() {
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    let wrapper = fixture.script(
        "tmux-that-cannot-kill",
        "for argument in \"$@\"; do [ \"$argument\" = kill-server ] && exit 0; done\nexec tmux \"$@\"",
    );
    let run = fixture.run(
        &[("LIBTMUX_TEST_TMUX", wrapper.to_str().expect("UTF-8 path"))],
        "tmux -f /dev/null new-session -d -P -F '#{pid}' > \"$OUT\"",
    );

    let server = recorded_server(&fixture).expect("the server pid");
    assert_eq!(run.code, Some(LEFT_A_SERVER), "{}", run.stderr);
    assert!(run.stderr.contains("could not stop"), "{}", run.stderr);
    assert!(run.stderr.contains("default"), "{}", run.stderr);
    assert!(run.stderr.contains("directory is kept"), "{}", run.stderr);
    assert!(
        server.is_alive(),
        "the control: the wrapper did not stop it either"
    );
    assert_eq!(
        fixture.private_directories(),
        1,
        "the directory was deleted"
    );
}

#[test]
fn with_tmux_a_slow_kill_server_is_waited_for() {
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    let wrapper = fixture.script(
        "tmux-that-kills-late",
        "for argument in \"$@\"; do\n\
         if [ \"$argument\" = kill-server ]; then ( sleep 1; tmux \"$@\" ) >/dev/null 2>&1 & exit 0; fi\n\
         done\nexec tmux \"$@\"",
    );
    let run = fixture.run(
        &[("LIBTMUX_TEST_TMUX", wrapper.to_str().expect("UTF-8 path"))],
        "tmux -f /dev/null new-session -d -P -F '#{pid}' > \"$OUT\"",
    );

    let server = recorded_server(&fixture).expect("the server pid");
    assert_eq!(run.code, Some(LEFT_A_SERVER), "{}", run.stderr);
    assert!(run.stderr.contains("stopped"), "{}", run.stderr);
    assert!(!run.stderr.contains("could not stop"), "{}", run.stderr);
    assert!(server.is_gone(), "the server is still running");
    assert_eq!(fixture.private_directories(), 0, "the directory was left");
}

#[test]
fn with_tmux_a_server_of_another_release_keeps_the_directory() {
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    let wrapper = fixture.script(
        "tmux-of-another-release",
        "for argument in \"$@\"; do\n\
         if [ \"$argument\" = list-sessions ]; then\n\
         echo 'protocol version mismatch (client 9, server 8)' >&2; exit 1\nfi\n\
         done\nexec tmux \"$@\"",
    );
    let run = fixture.run(
        &[("LIBTMUX_TEST_TMUX", wrapper.to_str().expect("UTF-8 path"))],
        "tmux -f /dev/null -S \"$TMUX_TMPDIR/old.sock\" new-session -d && \
         tmux -S \"$TMUX_TMPDIR/old.sock\" kill-server; \
         while tmux -N -S \"$TMUX_TMPDIR/old.sock\" list-sessions >/dev/null 2>&1; do sleep 0.1; done",
    );

    assert_eq!(run.code, Some(LEFT_A_SERVER), "{}", run.stderr);
    assert!(run.stderr.contains("could not stop"), "{}", run.stderr);
    assert!(run.stderr.contains("old.sock"), "{}", run.stderr);
    assert_eq!(
        fixture.private_directories(),
        1,
        "the directory was deleted"
    );
}

#[test]
fn it_keeps_the_directory_when_it_cannot_search_it() {
    let fixture = Fixture::new();
    fixture.script("find", "exit 1");
    let path = format!(
        "{}:{}",
        fixture.parent.path().display(),
        std::env::var("PATH").unwrap_or_default(),
    );
    let run = fixture.run(&[("PATH", &path)], "exit 0");

    assert_eq!(run.code, Some(LEFT_A_SERVER), "{}", run.stderr);
    assert!(run.stderr.contains("could not search"), "{}", run.stderr);
    assert_eq!(
        fixture.private_directories(),
        1,
        "the directory was deleted"
    );
}
