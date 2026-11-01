//! `scripts/sandbox-runner.sh`, the Cargo runner `.cargo/config.toml` names,
//! run directly around small commands. It ships nowhere: a packaged crate has
//! no runner to run.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
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
        // A command that starts tmux must not do it without the sandbox.
        let guarded = format!("[ -d \"$TMUX_TMPDIR\" ] || exit 99; {script}");
        let mut command = Command::new(runner());
        command
            .args(["sh", "-c", &guarded])
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

    /// A directory of links to the tools the runner and a shell command use,
    /// and to no tmux, for a `PATH` on which no tmux is found.
    fn path_without_tmux(&self) -> String {
        let bin = self.file("bin");
        std::fs::create_dir(&bin).expect("a tool directory");
        let path = std::env::var_os("PATH").unwrap_or_default();
        for tool in [
            "id", "mktemp", "chmod", "wc", "sed", "tr", "find", "head", "rm", "ln", "sh",
            "dirname", "touch", "sleep",
        ] {
            let found = std::env::split_paths(&path)
                .map(|directory| directory.join(tool))
                .find(|candidate| candidate.is_file())
                .unwrap_or_else(|| panic!("{tool} is not on PATH"));
            std::os::unix::fs::symlink(found, bin.join(tool)).expect("a tool link");
        }
        bin.to_str().expect("UTF-8 path").to_owned()
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

const TMUX_VISIBLE: &str =
    "if [ -z \"${TMUX+x}${TMUX_PANE+x}\" ]; then echo unset; else echo set; fi > \"$OUT\"";

#[test]
fn it_unsets_the_ambient_tmux_and_pane() {
    let fixture = Fixture::new();
    let run = fixture.run(
        &[("TMUX", "SENTINEL-ambient,1,0"), ("TMUX_PANE", "%9")],
        TMUX_VISIBLE,
    );

    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(fixture.answer().trim(), "unset");
    assert_eq!(run.stderr, "", "a clean run says nothing");
}

/// The binary's view of the named server: whether `TMUX` is what the caller
/// named, whether the ambient one survived, and whether the pane or the
/// variable that carried the name did.
const NAMED_VISIBLE: &str = "{ [ \"${TMUX-}\" = \"$WANT\" ] && echo named || echo not-named; \
     [ -z \"${TMUX_PANE+x}\" ] && echo no-pane || echo pane; \
     [ -z \"${LIBTMUX_SANDBOX_TMUX+x}\" ] && echo no-carrier || echo carrier; } > \"$OUT\"";

#[test]
fn it_hands_the_binary_the_tmux_it_was_named_and_says_so() {
    let named = "SENTINEL-named,1,0";
    let fixture = Fixture::new();
    let run = fixture.run(
        &[
            ("LIBTMUX_SANDBOX_TMUX", named),
            ("WANT", named),
            ("TMUX", "SENTINEL-ambient,1,0"),
            ("TMUX_PANE", "%9"),
        ],
        NAMED_VISIBLE,
    );

    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        fixture.answer().lines().collect::<Vec<_>>(),
        ["named", "no-pane", "no-carrier"],
    );
    assert!(
        run.stderr.contains("LIBTMUX_SANDBOX_TMUX is set: true"),
        "the opt-out was not logged: {}",
        run.stderr,
    );
    assert!(!run.stderr.contains("SENTINEL"), "a value was printed");
}

#[test]
fn it_ignores_an_empty_name_and_never_falls_back_to_the_ambient_tmux() {
    let fixture = Fixture::new();
    let run = fixture.run(
        &[
            ("LIBTMUX_SANDBOX_TMUX", ""),
            ("TMUX", "SENTINEL-ambient,1,0"),
        ],
        TMUX_VISIBLE,
    );

    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(fixture.answer().trim(), "unset");
    assert_eq!(run.stderr, "", "an empty name was logged as an opt-out");
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
        assert!(lines[0] == lines[1], "TMUX_TMPDIR and the marker differ");
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
fn a_refused_run_does_not_say_the_hatch_applied() {
    let fixture = Fixture::new();

    let run = fixture.run(
        &[
            ("TMPDIR", "relative/dir"),
            ("LIBTMUX_SANDBOX_TMUX", "SENTINEL-named,1,0"),
        ],
        "touch \"$MARKER\"",
    );

    assert_eq!(run.code, Some(REFUSED), "{}", run.stderr);
    assert!(!fixture.file("marker").exists(), "the binary ran");
    assert!(
        !run.stderr.contains("LIBTMUX_SANDBOX_TMUX"),
        "the opt-out was logged for a run that never started: {}",
        run.stderr,
    );
}

#[test]
fn it_refuses_a_relative_tmpdir_that_exists() {
    let fixture = Fixture::new();

    // `.` is a directory, so only its being relative can refuse it.
    let output = fixture
        .command(&[("TMPDIR", ".")], "touch \"$MARKER\"")
        .current_dir(fixture.parent.path())
        .output()
        .expect("the runner starts");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(REFUSED), "{stderr}");
    assert!(!fixture.file("marker").exists(), "the binary ran");
    assert!(stderr.contains("TMPDIR"), "TMPDIR is not named");
}

#[test]
fn it_measures_the_directory_a_tmpdir_link_resolves_to() {
    let fixture = Fixture::new();
    let long = fixture.file(&"a".repeat(80));
    std::fs::create_dir(&long).expect("a long directory");
    let link = fixture.file("short");
    std::os::unix::fs::symlink(&long, &link).expect("a link to it");
    let link = link.to_str().expect("UTF-8 path");

    let run = fixture.run(&[("TMPDIR", link)], "touch \"$MARKER\"");

    assert_eq!(run.code, Some(REFUSED), "{}", run.stderr);
    assert!(!fixture.file("marker").exists(), "the binary ran");
    assert!(run.stderr.contains("TMPDIR"), "TMPDIR is not named");
    assert!(!run.stderr.contains(link), "a value was printed");
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
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    fixture.script(
        "find",
        "case \" $* \" in *\" -type s \"*) echo \"find: $TMUX_TMPDIR: Permission denied\" >&2; exit 1 ;; esac\n\
         exec /usr/bin/find \"$@\"",
    );
    let path = format!(
        "{}:{}",
        fixture.parent.path().display(),
        std::env::var("PATH").unwrap_or_default(),
    );
    let run = fixture.run(&[("PATH", &path)], "exit 0");

    assert_eq!(run.code, Some(LEFT_A_SERVER), "{}", run.stderr);
    assert!(run.stderr.contains("could not search"), "{}", run.stderr);
    assert!(!run.stderr.contains("ltrs."), "a path was printed");
    assert_eq!(
        fixture.private_directories(),
        1,
        "the directory was deleted"
    );
}

#[test]
fn it_keeps_the_directory_it_cannot_remove_and_prints_no_path() {
    // Root removes what it likes.
    if rustix::process::getuid().is_root() {
        return;
    }
    let fixture = Fixture::new();

    let run = fixture.run(
        &[],
        "mkdir \"$TMUX_TMPDIR/d\" && touch \"$TMUX_TMPDIR/d/f\" && chmod 500 \"$TMUX_TMPDIR/d\"",
    );
    // Let the fixture's own cleanup remove what the runner could not.
    for entry in std::fs::read_dir(fixture.parent.path()).expect("the parent lists") {
        let entry = entry.expect("an entry");
        if entry.file_name().to_string_lossy().starts_with("ltrs.") {
            std::fs::set_permissions(
                entry.path().join("d"),
                std::fs::Permissions::from_mode(0o700),
            )
            .expect("the directory opens up");
        }
    }

    assert_eq!(run.code, Some(LEFT_A_SERVER), "{}", run.stderr);
    assert!(run.stderr.contains("could not remove"), "{}", run.stderr);
    assert!(!run.stderr.contains("ltrs."), "a path was printed");
    assert_eq!(fixture.private_directories(), 1, "the directory is gone");
}

/// Whether a shell here can trap `INT`. A process started in the background of
/// a non-interactive shell ignores it, and a shell cannot trap what it inherits
/// as ignored.
fn a_shell_can_trap_interrupts() -> bool {
    Command::new("sh")
        .args(["-c", "trap 'exit 7' INT; kill -INT $$; sleep 1; exit 0"])
        .status()
        .is_ok_and(|status| status.code() == Some(7))
}

#[test]
fn an_interrupt_the_binary_survives_exits_130() {
    if !a_shell_can_trap_interrupts() {
        eprintln!("INT is ignored in this process; no shell can trap it");
        return;
    }
    let fixture = Fixture::new();

    let run = fixture.run(&[], "kill -INT $PPID; exit 0");

    assert_eq!(run.code, Some(130), "{}", run.stderr);
    assert_eq!(fixture.private_directories(), 0, "the directory was left");
}

#[test]
fn without_tmux_a_socket_in_the_sandbox_keeps_the_directory() {
    let fixture = Fixture::new();
    let outer = fixture.file("outer.sock");
    let _listener = UnixListener::bind(&outer).expect("a socket binds");
    let path = fixture.path_without_tmux();
    let run = fixture.run(
        &[
            ("PATH", &path),
            ("LIBTMUX_TEST_TMUX", "/nonexistent/tmux"),
            ("OUTER", outer.to_str().expect("UTF-8 path")),
        ],
        "ln \"$OUTER\" \"$TMUX_TMPDIR/linked.sock\"",
    );

    assert_eq!(run.code, Some(LEFT_A_SERVER), "{}", run.stderr);
    assert!(run.stderr.contains("no tmux"), "{}", run.stderr);
    assert!(run.stderr.contains("directory is kept"), "{}", run.stderr);
    assert_eq!(
        fixture.private_directories(),
        1,
        "the directory was deleted"
    );
}

#[test]
fn without_tmux_a_directory_with_no_socket_is_removed() {
    let fixture = Fixture::new();
    let path = fixture.path_without_tmux();

    let run = fixture.run(
        &[("PATH", &path), ("LIBTMUX_TEST_TMUX", "/nonexistent/tmux")],
        "exit 0",
    );

    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stderr, "", "a clean run says nothing");
    assert_eq!(fixture.private_directories(), 0);
}

/// A directory standing in for one tmux would have used, with the per-user
/// directory inside it, and the socket path a bare client would bind there.
fn stand_in_root(fixture: &Fixture, name: &str) -> (PathBuf, PathBuf) {
    let root = fixture.file(name);
    let per_user = root.join(format!("tmux-{}", rustix::process::getuid().as_raw()));
    std::fs::create_dir_all(&per_user).expect("a stand-in root");
    // tmux refuses a per-user directory that others can reach.
    std::fs::set_permissions(&per_user, std::fs::Permissions::from_mode(0o700))
        .expect("an owner-only directory");
    (root, per_user.join("default"))
}

/// Start a server on `socket` with a client that follows `root`, before the
/// runner does, and stop it when dropped.
struct EarlierServer {
    socket: PathBuf,
}

impl EarlierServer {
    fn start(root: &Path, socket: PathBuf) -> Self {
        let tmux = std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into());
        let started = Command::new(tmux)
            .args(["-f", "/dev/null", "new-session", "-d"])
            .env("TMUX_TMPDIR", root)
            .env("SHELL", "/bin/sh")
            .env_remove("TMUX")
            .stdin(Stdio::null())
            .status()
            .expect("tmux runs");
        assert!(started.success(), "tmux did not start a server");
        Self { socket }
    }
}

impl Drop for EarlierServer {
    fn drop(&mut self) {
        let tmux = std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into());
        let _ = Command::new(tmux)
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .stderr(Stdio::null())
            .status();
    }
}

#[test]
fn a_server_started_on_the_callers_default_socket_fails_the_run_and_is_left_alone() {
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    let (root, _) = stand_in_root(&fixture, "caller");
    let root_text = root.to_str().expect("UTF-8 path");

    let run = fixture.run(
        &[("TMUX_TMPDIR", root_text), ("CALLER", root_text)],
        "[ -d \"$CALLER\" ] || exit 99; TMUX_TMPDIR=\"$CALLER\" tmux -f /dev/null new-session -d -P -F '#{pid}' > \"$OUT\"",
    );

    let server = recorded_server(&fixture).expect("the server pid");
    assert_eq!(run.code, Some(LEFT_A_SERVER), "{}", run.stderr);
    assert!(run.stderr.contains("outside the sandbox"), "{}", run.stderr);
    assert!(run.stderr.contains("TMUX_TMPDIR"), "{}", run.stderr);
    assert!(!run.stderr.contains(root_text), "a path was printed");
    assert!(
        server.is_alive(),
        "a server that was not the run's to stop was stopped"
    );
}

#[test]
fn a_server_started_in_the_fallback_directory_fails_the_run() {
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    let (root, _) = stand_in_root(&fixture, "fallback");
    let root_text = root.to_str().expect("UTF-8 path");

    // tmux uses its fallback when TMUX_TMPDIR names no directory, so a
    // command that overrides it stands in for that.
    let run = fixture.run(
        &[
            ("LIBTMUX_SANDBOX_FALLBACK_ROOT", root_text),
            ("FALLBACK", root_text),
        ],
        "[ -d \"$FALLBACK\" ] || exit 99; TMUX_TMPDIR=\"$FALLBACK\" tmux -f /dev/null new-session -d -P -F '#{pid}' > \"$OUT\"",
    );

    let server = recorded_server(&fixture).expect("the server pid");
    assert_eq!(run.code, Some(LEFT_A_SERVER), "{}", run.stderr);
    assert!(
        run.stderr.contains("tmux fallback directory"),
        "{}",
        run.stderr
    );
    assert!(!run.stderr.contains(root_text), "a path was printed");
    assert!(
        server.is_alive(),
        "a server that was not the run's to stop was stopped"
    );
}

#[test]
fn a_server_there_before_the_run_is_not_reported() {
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    let (root, socket) = stand_in_root(&fixture, "caller");
    let _earlier = EarlierServer::start(&root, socket);
    let root_text = root.to_str().expect("UTF-8 path");

    let run = fixture.run(
        &[
            ("TMUX_TMPDIR", root_text),
            ("LIBTMUX_SANDBOX_FALLBACK_ROOT", root_text),
        ],
        "exit 0",
    );

    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(
        !run.stderr.contains("outside the sandbox"),
        "{}",
        run.stderr
    );
}

#[test]
fn a_stale_socket_file_there_is_not_reported() {
    let fixture = Fixture::new();
    let (root, socket) = stand_in_root(&fixture, "caller");
    drop(UnixListener::bind(&socket).expect("a socket binds"));
    let root_text = root.to_str().expect("UTF-8 path");

    let run = fixture.run(
        &[
            ("TMUX_TMPDIR", root_text),
            ("LIBTMUX_SANDBOX_FALLBACK_ROOT", root_text),
        ],
        "exit 0",
    );

    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(
        !run.stderr.contains("outside the sandbox"),
        "{}",
        run.stderr
    );
}

#[test]
fn the_fallback_root_is_named_in_the_log_and_not_handed_on() {
    let fixture = Fixture::new();
    let root = fixture.file("elsewhere");
    std::fs::create_dir(&root).expect("a directory");
    let root_text = root.to_str().expect("UTF-8 path");

    let run = fixture.run(
        &[("LIBTMUX_SANDBOX_FALLBACK_ROOT", root_text)],
        "if [ -z \"${LIBTMUX_SANDBOX_FALLBACK_ROOT+x}\" ]; then echo gone; else echo kept; fi > \"$OUT\"",
    );

    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        fixture.answer().trim(),
        "gone",
        "the binary saw the variable"
    );
    assert!(
        run.stderr
            .contains("LIBTMUX_SANDBOX_FALLBACK_ROOT is set: true"),
        "{}",
        run.stderr,
    );
    assert!(!run.stderr.contains(root_text), "a path was printed");
}

#[test]
fn the_checkouts_own_fallback_directory_is_not_watched() {
    if !tmux_is_available() {
        return;
    }
    let fixture = Fixture::new();
    // What Cargo's `[env]` gives the runner in place of the caller's value.
    let (root, _) = stand_in_root(&fixture, ".cargo/tmux-sandbox");
    let root_text = root.to_str().expect("UTF-8 path");
    let elsewhere = fixture.file("elsewhere");
    std::fs::create_dir(&elsewhere).expect("a directory");

    let run = fixture.run(
        &[
            ("TMUX_TMPDIR", root_text),
            (
                "LIBTMUX_SANDBOX_FALLBACK_ROOT",
                elsewhere.to_str().expect("UTF-8"),
            ),
            ("CALLER", root_text),
        ],
        "[ -d \"$CALLER\" ] || exit 99; TMUX_TMPDIR=\"$CALLER\" tmux -f /dev/null new-session -d -P -F '#{pid}' > \"$OUT\"",
    );

    let _server = recorded_server(&fixture).expect("the server pid");
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(
        !run.stderr.contains("outside the sandbox"),
        "{}",
        run.stderr
    );
}

/// The helper `scripts/sandbox-outside.sh`, called the way the runner does.
fn outside(script: &str, arguments: &[&Path]) -> (Option<i32>, String) {
    let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/sandbox-outside.sh");
    let output = Command::new("sh")
        .args(["-c", &format!(". \"$1\"; shift; {script}"), "sh"])
        .arg(helper)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .expect("sh runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn the_outside_check_fails_closed_without_a_reference() {
    let fixture = Fixture::new();
    let (root, _) = stand_in_root(&fixture, "caller");
    let missing = fixture.file("no-reference");

    let (code, printed) = outside("outside_started \"$@\"", &[&missing, &root]);

    assert_eq!(code, Some(2));
    assert_eq!(printed, "", "something was named without a reference");
}

#[test]
fn the_outside_check_names_only_a_socket_made_after_the_reference() {
    let fixture = Fixture::new();
    let (root, socket) = stand_in_root(&fixture, "caller");
    let (other, other_socket) = stand_in_root(&fixture, "other");
    let reference = fixture.file("reference");
    let before = UnixListener::bind(&other_socket).expect("a socket binds");
    assert_eq!(
        outside("outside_reference \"$1\"", &[&reference]).0,
        Some(0)
    );
    let _after = UnixListener::bind(&socket).expect("a socket binds");

    let (code, printed) = outside("outside_started \"$@\"", &[&reference, &root, &other]);

    assert_eq!(code, Some(0));
    assert_eq!(printed.trim(), root.to_str().expect("UTF-8 path"));
    drop(before);
}

#[test]
fn the_reference_is_older_than_whatever_is_made_next() {
    let fixture = Fixture::new();
    let reference = fixture.file("reference");
    let next = fixture.file("next");
    for round in 0..50 {
        let (code, printed) = outside(
            "outside_reference \"$1\" && touch \"$2\" && find \"$2\" -newer \"$1\"",
            &[&reference, &next],
        );
        assert_eq!(code, Some(0), "round {round}");
        assert!(!printed.is_empty(), "round {round}: made in the same tick");
    }
}

#[test]
fn a_run_whose_outside_check_cannot_be_made_fails() {
    let fixture = Fixture::new();

    // The reference is how the runner tells what the run made.
    let run = fixture.run(&[], "rm -f \"$TMUX_TMPDIR/.reference\"");

    assert_eq!(run.code, Some(LEFT_A_SERVER), "{}", run.stderr);
    assert!(
        run.stderr.contains("could not check for servers outside"),
        "{}",
        run.stderr,
    );
    assert!(!run.stderr.contains("ltrs."), "a path was printed");
}
