//! The tmux environment Cargo gives every test, checked from inside one.
//!
//! `.cargo/config.toml` empties `TMUX`, `TMUX_PANE`, `LIBTMUX_SOCKET` and
//! `LIBTMUX_SOCKET_PATH` and points `TMUX_TMPDIR` at `.cargo/tmux-sandbox`; the
//! runner then unsets the four and gives the process a directory of its own.
//! This file asserts the result, starts a bare tmux client to see where its
//! default socket lands, and runs itself again in the environment of a tmux
//! pane, where it must fail. A failure names a variable,
//! never its value. It ships nowhere: a packaged crate has no
//! `.cargo/config.toml` to assert.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};

use libtmux::Server;

/// A case: the finding it must produce, and the variables it changes (`None` removes one).
type Case<'a> = (&'a str, &'a [(&'a str, Option<&'a str>)]);

const INNER: &str = "the_environment_cargo_gave_this_process_is_the_sandbox";

/// `sun_path` holds 104 bytes on macOS and 108 on Linux.
const SOCKET_PATH_LIMIT: usize = 100;

/// A child forked while a listener is open holds the socket until it execs, and
/// a server one test starts is another test's leftover, so every test that
/// spawns, binds or looks at the sandbox's sockets takes a turn.
static SERIAL: Mutex<()> = Mutex::new(());

fn take_turn() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A view of the environment, so a case can differ from the real one in a
/// single variable without `set_var`, which the workspace's `unsafe_code`
/// lint forbids.
type Lookup<'a> = &'a dyn Fn(&str) -> Option<OsString>;

fn process_environment(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Whether anything accepts connections on `socket`. tmux never unlinks its
/// socket, so a file that is still there says nothing about a server.
fn listening(socket: &Path) -> bool {
    UnixStream::connect(socket).is_ok()
}

/// Everything in `lookup` that would let a bare tmux command leave the sandbox.
fn findings(lookup: Lookup<'_>) -> Vec<String> {
    let mut found = Vec::new();

    // The runner unsets them. `[env]` can only empty them, so empty means the
    // runner did not wrap this process.
    for name in ["TMUX", "TMUX_PANE", "LIBTMUX_SOCKET", "LIBTMUX_SOCKET_PATH"] {
        if lookup(name).is_some() {
            found.push(format!("{name} is set; the sandbox runner unsets it"));
        }
    }

    let tmpdir = lookup("TMUX_TMPDIR").filter(|value| !value.is_empty());
    let marker = lookup("LIBTMUX_TEST_SANDBOX").filter(|value| !value.is_empty());
    let (Some(tmpdir), Some(marker)) = (tmpdir, marker) else {
        found.push(
            "TMUX_TMPDIR or LIBTMUX_TEST_SANDBOX is unset; \
             `.cargo/config.toml` did not apply to this process"
                .to_owned(),
        );
        return found;
    };
    if tmpdir != marker {
        found.push("TMUX_TMPDIR is not LIBTMUX_TEST_SANDBOX".to_owned());
    }

    let root = Path::new(&marker);
    if !root.is_absolute() {
        found.push("LIBTMUX_TEST_SANDBOX is not an absolute path".to_owned());
        return found;
    }
    let resolved = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if resolved.ends_with(".cargo/tmux-sandbox") {
        found.push(
            "LIBTMUX_TEST_SANDBOX is the committed fallback directory; \
             the sandbox runner did not apply"
                .to_owned(),
        );
    }
    let per_user = resolved.join(format!("tmux-{}", uid()));
    if per_user.join("default").as_os_str().len() > SOCKET_PATH_LIMIT {
        found.push(format!(
            "the default socket path under LIBTMUX_TEST_SANDBOX is over {SOCKET_PATH_LIMIT} bytes"
        ));
    }

    // tmux ignores a TMUX_TMPDIR it cannot use and falls back to /tmp, so a
    // missing directory is the one failure that reaches the default socket.
    let metadata = match std::fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => metadata,
        _ => {
            found.push(
                "LIBTMUX_TEST_SANDBOX is not a directory; tmux would fall back to /tmp".to_owned(),
            );
            return found;
        }
    };
    if metadata.uid() != uid() {
        found.push("LIBTMUX_TEST_SANDBOX belongs to another user".to_owned());
    }
    if metadata.mode() & 0o002 != 0 {
        found.push("LIBTMUX_TEST_SANDBOX is world-writable".to_owned());
    }
    // The sandbox itself takes the checkout's umask, since git records no
    // directory modes. What tmux keeps private is this one, which it creates
    // 0700 and refuses unless it is ours and closed to others.
    if let Ok(metadata) = std::fs::symlink_metadata(&per_user) {
        if !metadata.is_dir() || metadata.uid() != uid() || metadata.mode() & 0o077 != 0 {
            found.push(
                "the per-user directory in the sandbox is not an owner-only directory of this user"
                    .to_owned(),
            );
        }
    }
    found
}

fn assert_sandboxed() {
    let found = findings(&process_environment);
    assert!(
        found.is_empty(),
        "this process is not in the tmux sandbox:\n  {}\n\
         Run it through Cargo, which applies .cargo/config.toml.",
        found.join("\n  "),
    );
}

#[test]
fn the_environment_cargo_gave_this_process_is_the_sandbox() {
    assert_sandboxed();
}

#[test]
fn the_default_socket_resolves_under_the_sandbox() {
    assert_sandboxed();
    let _turn = take_turn();
    let root = PathBuf::from(std::env::var_os("LIBTMUX_TEST_SANDBOX").unwrap_or_default())
        .canonicalize()
        .expect("the sandbox exists");
    let expected = root.join(format!("tmux-{}", uid())).join("default");

    let server = Server::new().expect("a default server configuration");

    assert!(
        server.socket_path() == expected,
        "Server::new() does not resolve its default socket under the sandbox",
    );
    assert!(
        !listening(&expected),
        "something listens on the sandbox's default socket; a test or an earlier \
         run started a server there",
    );
}

fn tmux() -> OsString {
    std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into())
}

fn tmux_is_available() -> bool {
    Command::new(tmux())
        .arg("-V")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn is_socket(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

/// Where a bare tmux client's server appeared.
struct Landing {
    started: bool,
    at_expected: bool,
}

/// Start a server with a bare client (no `-L`, no `-S`) whose `TMUX_TMPDIR` is
/// `tmux_tmpdir`, see whether a server listens at `expected`, and stop the one it started.
fn bare_client_lands(tmux_tmpdir: &Path, expected: &Path) -> Landing {
    let actual = tmux_tmpdir.join(format!("tmux-{}", uid())).join("default");
    let client = |arguments: &[&OsStr]| {
        Command::new(tmux())
            .args(arguments)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env("TMUX_TMPDIR", tmux_tmpdir)
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    let started = client(&[
        "-f".as_ref(),
        "/dev/null".as_ref(),
        "new-session".as_ref(),
        "-d".as_ref(),
    ]);
    let at_expected = listening(expected);
    if is_socket(&actual) {
        client(&["-S".as_ref(), actual.as_os_str(), "kill-server".as_ref()]);
        for _ in 0..50 {
            if !listening(&actual) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    Landing {
        started,
        at_expected,
    }
}

fn assert_lands_in_the_sandbox(landing: &Landing) {
    assert!(landing.started, "tmux did not start a server");
    assert!(
        landing.at_expected,
        "a bare tmux client did not put its server on the sandbox's default socket",
    );
}

#[test]
fn a_bare_tmux_client_starts_its_server_in_the_sandbox() {
    assert_sandboxed();
    let _turn = take_turn();
    if !tmux_is_available() {
        eprintln!("tmux is not on PATH; no bare client to run");
        return;
    }
    let sandbox = PathBuf::from(std::env::var_os("LIBTMUX_TEST_SANDBOX").unwrap_or_default())
        .canonicalize()
        .expect("the sandbox exists");
    let expected = sandbox.join(format!("tmux-{}", uid())).join("default");
    assert!(
        !listening(&expected),
        "something listens on the sandbox's default socket before the client starts",
    );

    assert_lands_in_the_sandbox(&bare_client_lands(&sandbox, &expected));
}

#[test]
fn the_bare_client_assertion_fails_when_the_server_lands_elsewhere() {
    let _turn = take_turn();
    if !tmux_is_available() {
        eprintln!("tmux is not on PATH; no bare client to run");
        return;
    }
    let sandbox = PathBuf::from(std::env::var_os("LIBTMUX_TEST_SANDBOX").unwrap_or_default());
    let expected = sandbox.join(format!("tmux-{}", uid())).join("default");
    let elsewhere = tempfile::tempdir().expect("scratch directory");

    let landing = bare_client_lands(elsewhere.path(), &expected);

    assert!(landing.started, "the control: the client started a server");
    assert!(
        std::panic::catch_unwind(|| assert_lands_in_the_sandbox(&landing)).is_err(),
        "the assertion held for a server that landed elsewhere",
    );
}

#[test]
fn a_leftover_socket_file_is_not_a_server_but_a_listener_is() {
    let _turn = take_turn();
    let directory = tempfile::tempdir().expect("scratch directory");
    let socket = directory.path().join("s");

    drop(UnixListener::bind(&socket).expect("a socket binds"));
    assert!(socket.exists(), "the control: the file outlives its server");
    assert!(!listening(&socket), "a file with no server behind it");

    std::fs::remove_file(&socket).expect("the leftover is removed");
    let _server = UnixListener::bind(&socket).expect("a socket binds");
    assert!(listening(&socket), "a socket that accepts connections");
}

#[test]
fn each_finding_names_what_is_wrong() {
    assert_eq!(
        findings(&process_environment),
        Vec::<String>::new(),
        "the control: the real environment has nothing to report",
    );

    let sandbox = std::env::var_os("LIBTMUX_TEST_SANDBOX")
        .and_then(|value| value.into_string().ok())
        .expect("a UTF-8 sandbox path");
    let absent = format!("{sandbox}/does-not-exist");
    let executable = std::env::current_exe().expect("test executable");
    let executable = executable.to_str().expect("UTF-8 executable path");
    let other = std::env::temp_dir();
    let other = other.to_str().expect("UTF-8 temporary directory");
    let long = format!("/{}", "a".repeat(SOCKET_PATH_LIMIT));
    let fallback = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cargo/tmux-sandbox");
    let fallback = fallback.to_str().expect("UTF-8 fallback path");

    let cases: [Case<'_>; 16] = [
        ("TMUX is set", &[("TMUX", Some("/tmp/decoy.sock,1,0"))]),
        ("TMUX is set", &[("TMUX", Some(""))]),
        ("TMUX_PANE is set", &[("TMUX_PANE", Some("%9"))]),
        ("LIBTMUX_SOCKET is set", &[("LIBTMUX_SOCKET", Some("name"))]),
        ("LIBTMUX_SOCKET is set", &[("LIBTMUX_SOCKET", Some(""))]),
        (
            "LIBTMUX_SOCKET_PATH is set",
            &[("LIBTMUX_SOCKET_PATH", Some(""))],
        ),
        (
            "LIBTMUX_SOCKET_PATH is set",
            &[("LIBTMUX_SOCKET_PATH", Some("/tmp/decoy.sock"))],
        ),
        (
            "did not apply",
            &[("LIBTMUX_TEST_SANDBOX", None), ("TMUX_TMPDIR", None)],
        ),
        ("did not apply", &[("LIBTMUX_TEST_SANDBOX", None)]),
        (
            "is not LIBTMUX_TEST_SANDBOX",
            &[("TMUX_TMPDIR", Some(other))],
        ),
        (
            "not an absolute path",
            &[
                ("TMUX_TMPDIR", Some("relative/sandbox")),
                ("LIBTMUX_TEST_SANDBOX", Some("relative/sandbox")),
            ],
        ),
        (
            "fall back to /tmp",
            &[
                ("TMUX_TMPDIR", Some(&absent)),
                ("LIBTMUX_TEST_SANDBOX", Some(&absent)),
            ],
        ),
        (
            "not a directory",
            &[
                ("TMUX_TMPDIR", Some(executable)),
                ("LIBTMUX_TEST_SANDBOX", Some(executable)),
            ],
        ),
        (
            "committed fallback directory",
            &[
                ("TMUX_TMPDIR", Some(fallback)),
                ("LIBTMUX_TEST_SANDBOX", Some(fallback)),
            ],
        ),
        (
            "world-writable",
            &[
                ("TMUX_TMPDIR", Some("/tmp")),
                ("LIBTMUX_TEST_SANDBOX", Some("/tmp")),
            ],
        ),
        (
            "bytes",
            &[
                ("TMUX_TMPDIR", Some(&long)),
                ("LIBTMUX_TEST_SANDBOX", Some(&long)),
            ],
        ),
    ];

    for (expected, changes) in cases {
        let overrides: HashMap<&str, Option<OsString>> = changes
            .iter()
            .map(|(name, value)| (*name, value.map(OsString::from)))
            .collect();
        let found = findings(&|name| {
            overrides
                .get(name)
                .cloned()
                .unwrap_or_else(|| process_environment(name))
        });
        assert!(
            found.iter().any(|finding| finding.contains(expected)),
            "changing {:?} should report {expected:?}, got {found:?}",
            changes.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        );
    }
}

#[test]
fn a_directory_of_another_user_is_reported() {
    // Root owns every directory it could be pointed at.
    if uid() == 0 {
        return;
    }
    let found = findings(&|name| match name {
        "TMUX_TMPDIR" | "LIBTMUX_TEST_SANDBOX" => Some(OsString::from("/usr")),
        _ => process_environment(name),
    });

    assert!(
        found
            .iter()
            .any(|finding| finding.contains("belongs to another user")),
        "a directory of another user went unreported: {found:?}",
    );
}

#[test]
fn an_open_per_user_directory_is_reported() {
    let open = tempfile::tempdir().expect("scratch directory");
    let path = open.path().to_str().expect("UTF-8 scratch path");
    let per_user = open.path().join(format!("tmux-{}", uid()));
    std::fs::create_dir(&per_user).expect("a per-user directory");

    for (mode, reported) in [(0o700, false), (0o750, true), (0o755, true)] {
        std::fs::set_permissions(&per_user, std::fs::Permissions::from_mode(mode))
            .expect("a per-user directory mode");
        let found = findings(&|name| match name {
            "TMUX_TMPDIR" | "LIBTMUX_TEST_SANDBOX" => Some(OsString::from(path)),
            _ => process_environment(name),
        });
        let open_finding = found
            .iter()
            .any(|finding| finding.contains("not an owner-only directory"));
        assert_eq!(open_finding, reported, "mode {mode:o}: {found:?}");
    }
}

/// Run this binary again on `INNER` alone and return its status and output.
fn rerun(environment: Option<&[(&str, &str)]>) -> (bool, String) {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command.args(["--exact", INNER, "--nocapture", "--test-threads=1"]);
    if let Some(environment) = environment {
        command.env_clear().envs(environment.iter().copied());
    }
    let _turn = take_turn();
    let output = command.output().expect("the test executable runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    (output.status.success(), text)
}

#[test]
fn the_check_fails_in_the_environment_of_a_tmux_pane() {
    let (passes, text) = rerun(None);
    assert!(
        passes && text.contains("1 passed"),
        "the control, run again as it is, must pass and run once:\n{text}",
    );

    // What a pane that was not started through Cargo has: a TMUX naming a
    // server, its pane, and a TMUX_TMPDIR that is not the sandbox. The paths
    // exist nowhere, and this binary only reads them.
    let Ok(directory) = tempfile::tempdir() else {
        panic!("no scratch directory");
    };
    let tmpdir = directory.path().to_str().expect("UTF-8 scratch path");
    let socket = directory.path().join("decoy.sock");
    let tmux = format!("{},4242,0", socket.display());
    let (passes, text) = rerun(Some(&[
        ("TMUX", &tmux),
        ("TMUX_PANE", "%9"),
        ("TMUX_TMPDIR", tmpdir),
    ]));
    assert!(
        !passes && text.contains("1 failed"),
        "the check let a tmux pane's environment through:\n{text}",
    );
    assert!(text.contains("TMUX is set"), "{text}");
    assert!(text.contains("did not apply"), "{text}");
    assert!(
        !text.contains(tmpdir) && !text.contains(&tmux),
        "a failure printed a value",
    );
}
