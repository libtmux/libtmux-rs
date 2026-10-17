//! Daemon floor enforcement across native versions and dispatch lifetimes.

#![cfg(feature = "test-support")]
#![allow(clippy::expect_used)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};
use std::time::Duration;

use libtmux::test::TestServer;
use libtmux::{Command, CommandChain, Error, Server};

/// Read the variable that gates a real-tmux case.
///
/// A lane lists the variables it provides in `LIBTMUX_REQUIRE_NATIVE`,
/// comma-separated. A case gated on a listed variable that is unset or empty
/// fails, so a lane whose setup did not export it cannot pass by skipping.
fn gated(variable: &str) -> Option<std::ffi::OsString> {
    if let Some(value) = std::env::var_os(variable).filter(|value| !value.is_empty()) {
        return Some(value);
    }
    let required = std::env::var("LIBTMUX_REQUIRE_NATIVE").unwrap_or_default();
    assert!(
        !required.split(',').any(|name| name.trim() == variable),
        "{variable} is listed in LIBTMUX_REQUIRE_NATIVE but is unset"
    );
    eprintln!("{variable} unset; case skipped");
    None
}

fn optional_binary(variable: &str) -> Option<PathBuf> {
    gated(variable).map(PathBuf::from)
}

fn current_binary() -> PathBuf {
    std::env::var_os("LIBTMUX_TEST_TMUX").map_or_else(|| PathBuf::from("tmux"), PathBuf::from)
}

fn native(binary: &Path, socket: &Path, args: &[&OsStr]) -> Output {
    ProcessCommand::new(binary)
        .arg("-S")
        .arg(socket)
        .arg("-f")
        .arg("/dev/null")
        .args(args)
        .env_remove("TMUX")
        .output()
        .expect("native tmux client starts")
}

/// A native `kill-server` returns while the daemon is still exiting. A client
/// that connects in that window sees `server exited unexpectedly` or an empty
/// success, neither of which the version check may read as a version, so wait until
/// the socket reports no server before the next library call.
fn wait_until_gone(binary: &Path, socket: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let output = native(
            binary,
            socket,
            &[OsStr::new("-N"), OsStr::new("list-sessions")],
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no server running") || stderr.contains("error connecting") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "daemon still answering after kill-server: {stderr}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn assert_unsupported(error: &Error, version: &str) {
    assert!(
        matches!(error, Error::UnsupportedTmuxVersion { .. }),
        "{error:?}"
    );
    assert_eq!(error.found_version().expect("found version").raw(), version);
    assert_eq!(
        error.minimum_version().expect("minimum").to_string(),
        "3.3a"
    );
}

#[tokio::test]
async fn real_tmux_compat_old_daemon_refuses_all_dispatch_paths() {
    let Some(old) = optional_binary("LIBTMUX_TEST_UNSUPPORTED_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(&old)
        .start()
        .await
        .expect("old daemon starts");
    let server = Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(current_binary())
        .build()
        .expect("client configured");
    let raw = server
        .cmd(
            Command::new("set-buffer")
                .arg("-b")
                .arg("refused")
                .arg("payload"),
        )
        .await
        .expect_err("public raw effects refuse");
    assert_unsupported(&raw, "3.2a");
    let typed = server
        .new_session("refused")
        .await
        .expect_err("typed creation refuses");
    assert_unsupported(&typed, "3.2a");
    let chain = server
        .chain(
            CommandChain::new(
                Command::new("new-session")
                    .arg("-d")
                    .arg("-s")
                    .arg("refused-chain"),
            )
            .then(
                Command::new("set-buffer")
                    .arg("-b")
                    .arg("refused-chain")
                    .arg("payload"),
            ),
        )
        .await
        .expect_err("whole chain refuses");
    assert_unsupported(&chain, "3.2a");
    let lock = server
        .lock_channel("refused-lock")
        .await
        .expect_err("unbounded dispatch also refuses");
    assert_unsupported(&lock, "3.2a");
    let listing = server
        .sessions()
        .await
        .expect_err("public reads enforce the floor");
    assert_unsupported(&listing, "3.2a");
    let layouts = server
        .validate_layouts(std::iter::empty())
        .await
        .expect_err("empty layout preflight still checks the product endpoint");
    assert_unsupported(&layouts, "3.2a");
    assert!(
        native(&old, guard.socket_path(), &[OsStr::new("list-sessions")])
            .stdout
            .is_empty()
    );
    assert!(
        native(&old, guard.socket_path(), &[OsStr::new("list-buffers")])
            .stdout
            .is_empty()
    );
    server.shutdown().await.expect("client shuts down");
    guard.shutdown().await.expect("fixture shuts down");
}

#[tokio::test]
async fn real_tmux_compat_cold_old_executable_refuses_before_starting() {
    let Some(old) = optional_binary("LIBTMUX_TEST_UNSUPPORTED_TMUX") else {
        return;
    };
    std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned fixture directory");
    let socket = directory.path().join("s");
    let server = Server::builder()
        .socket_path(&socket)
        .tmux_executable(old)
        .build()
        .expect("client configured");
    assert_eq!(
        server
            .capabilities()
            .await
            .expect("old executable diagnostics remain available")
            .tmux_version()
            .raw(),
        "3.2a"
    );
    let diagnostic = server
        .cmd(Command::new("-V"))
        .await
        .expect("native version flag remains available");
    assert_eq!(diagnostic.stdout(), b"tmux 3.2a\n");
    let help = server
        .cmd(Command::new("-h"))
        .await
        .expect("native help flag remains available");
    assert!(String::from_utf8_lossy(help.stderr()).contains("usage: tmux"));
    let error = server
        .chain(CommandChain::new(Command::new("-V")))
        .await
        .expect_err("chains do not exempt client flags from the version check");
    assert_unsupported(&error, "3.2a");
    let error = server
        .new_session("must-not-start")
        .await
        .expect_err("cold old executable refuses");
    assert_unsupported(&error, "3.2a");
    assert!(!socket.exists(), "no daemon was started");
    server.kill().await.expect("absent cleanup has no effect");
    assert!(!socket.exists());
    server.shutdown().await.expect("client shuts down");
}

#[tokio::test]
async fn real_tmux_compat_old_client_never_restarts_a_missing_daemon() {
    let Some(old) = optional_binary("LIBTMUX_TEST_UNSUPPORTED_TMUX") else {
        return;
    };
    let Some(minimum) = optional_binary("LIBTMUX_TEST_MINIMUM_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(&minimum)
        .start()
        .await
        .expect("minimum daemon starts");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned directory");
    let marker = directory.path().join("started");
    let config = directory.path().join("tmux.conf");
    std::fs::write(
        &config,
        format!("run-shell \"touch {}\"\n", shell_quote(marker.as_os_str())),
    )
    .expect("startup witness configured");
    let server = Server::builder()
        .socket_path(guard.socket_path())
        .config_file(&config)
        .tmux_executable(&old)
        .build()
        .expect("older client configured");
    let result = server
        .cmd(
            Command::new("display-message")
                .arg("-p")
                .arg("--")
                .arg("live"),
        )
        .await
        .expect("older client can operate on a supported live daemon");
    assert_eq!(result.stdout(), b"live\n");
    assert!(
        native(&minimum, guard.socket_path(), &[OsStr::new("kill-server")])
            .status
            .success()
    );
    let result = server.new_session("must-not-restart").await;
    let started = marker.exists();
    let cleanup = native(&old, guard.socket_path(), &[OsStr::new("kill-server")]);
    let _ = cleanup;
    assert!(
        result.is_err(),
        "the missing live endpoint refuses: {result:?}"
    );
    assert!(
        !started,
        "the configured old executable was never started as a daemon"
    );
    server.shutdown().await.expect("client shuts down");
    guard.shutdown().await.expect("fixture shuts down");
}

#[tokio::test]
async fn real_tmux_compat_unpatched_3_3_is_below_the_floor() {
    let Some(binary) = optional_binary("LIBTMUX_TEST_BOUNDARY_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(&binary)
        .start()
        .await
        .expect("3.3 daemon starts");
    let server = Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(current_binary())
        .build()
        .expect("client configured");
    let error = server
        .new_session("refused")
        .await
        .expect_err("3.3 is below 3.3a");
    assert_unsupported(&error, "3.3");
    assert!(
        native(&binary, guard.socket_path(), &[OsStr::new("list-sessions")])
            .stdout
            .is_empty()
    );
    server.shutdown().await.expect("client shuts down");
    guard.shutdown().await.expect("fixture shuts down");
}

#[tokio::test]
async fn real_tmux_compat_minimum_daemon_accepts_newer_clients() {
    let Some(binary) = optional_binary("LIBTMUX_TEST_MINIMUM_TMUX") else {
        return;
    };
    let guard = TestServer::builder()
        .tmux_executable(&binary)
        .start()
        .await
        .expect("3.3a daemon starts");
    let server = Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(current_binary())
        .build()
        .expect("newer client configured");
    let mut session = server
        .new_session("supported")
        .await
        .expect("minimum daemon accepts typed creation");
    let panes = session
        .panes()
        .await
        .expect("minimum daemon hydrates panes through a newer client");
    let pane = panes.first().expect("the session has a pane");
    assert_eq!(
        server.panes().await.expect("pane listing decodes")[0].id(),
        pane.id()
    );
    let result = session
        .cmd(
            Command::new("display-message")
                .arg("-p")
                .arg("--")
                .arg("scoped"),
        )
        .await
        .expect("scoped raw dispatch works");
    assert_eq!(result.stdout(), b"scoped\n");
    session
        .rename("renamed")
        .await
        .expect("typed mutation works");
    let result = server
        .chain(
            CommandChain::new(
                Command::new("set-buffer")
                    .arg("-b")
                    .arg("a")
                    .sensitive_arg("first"),
            )
            .then(
                Command::new("set-buffer")
                    .arg("-b")
                    .arg("b")
                    .sensitive_arg("second"),
            ),
        )
        .await
        .expect("chain dispatch works");
    assert!(result.success());
    assert!(result.command().to_string().starts_with("\"set-buffer\" "));
    assert!(!result.command().to_string().contains("if-shell"));
    assert_eq!(result.command().sensitive_argument_count(), 2);
    server
        .lock_channel("supported-lock")
        .await
        .expect("unbounded lock dispatch works");
    server
        .unlock_channel("supported-lock")
        .await
        .expect("unlock works");
    server.shutdown().await.expect("client shuts down");
    guard.shutdown().await.expect("fixture shuts down");
}

fn wrapper(directory: &Path, body: &str) -> PathBuf {
    // Installing through the fixture helper waits out the `ETXTBSY` a process
    // forked by a parallel test can cause while the script is open for write.
    let path = directory.join("tmux-wrapper");
    libtmux::test::install_executable(&path, &format!("#!/bin/sh\n{body}\n"))
        .expect("wrapper installed");
    path
}

fn shell_quote(value: &OsStr) -> String {
    format!(
        "'{}'",
        value
            .to_str()
            .expect("test path is UTF-8")
            .replace('\'', "'\\''")
    )
}

#[tokio::test]
async fn malformed_and_failed_probes_refuse_without_dispatching() {
    for output in [
        "printf 'tmux unknown\\n'",
        "printf 'tmux 3.3rc1\\n'",
        "printf 'tmux 3.3a-rc1\\n'",
        "printf 'tmux next-3.3a\\n'",
        "printf 'tmux 3.3a-extra\\n'",
        "printf 'tmux 3.3a-master\\n'",
        "exit 23",
    ] {
        std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
        let directory =
            tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned fixture directory");
        let marker = directory.path().join("effect");
        let executable = wrapper(
            directory.path(),
            &format!(
                "case \"$*\" in *'tmux '#*) {output};; *) touch {}; exit 0;; esac",
                shell_quote(marker.as_os_str())
            ),
        );
        let server = Server::builder()
            .socket_path(directory.path().join("s"))
            .tmux_executable(executable)
            .build()
            .expect("client configured");
        let error = server
            .cmd(Command::new("new-session").arg("-d"))
            .await
            .expect_err("unproved endpoint refuses");
        assert!(
            matches!(
                error,
                Error::InvalidVersionOutput { .. } | Error::VersionProbeFailed { .. }
            ),
            "{error:?}"
        );
        assert!(!marker.exists(), "the effect was never dispatched");
        server.shutdown().await.expect("client shuts down");
    }
}

/// `run-shell -C` arrived in tmux 3.2: a 3.1 daemon answers the probe with a
/// usage line. That is an unsupported daemon, not a transport or parse error.
#[tokio::test]
async fn a_daemon_older_than_the_probe_is_refused_as_unsupported() {
    std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned fixture directory");
    let marker = directory.path().join("effect");
    let executable = wrapper(
        directory.path(),
        &format!(
            "case \"$*\" in *libtmux-version-*) echo 'usage: run-shell [-b] [-t target-pane] shell-command' >&2; exit 1;; *) touch {}; exit 0;; esac",
            shell_quote(marker.as_os_str())
        ),
    );
    let server = Server::builder()
        .socket_path(directory.path().join("s"))
        .tmux_executable(executable)
        .build()
        .expect("client configured");
    let error = server
        .cmd(Command::new("new-session").arg("-d"))
        .await
        .expect_err("a pre-3.2 daemon is refused");
    assert!(
        matches!(error, Error::UnsupportedTmuxVersion { .. }),
        "{error:?}"
    );
    assert_eq!(error.kind(), libtmux::ErrorKind::UnsupportedVersion);
    let message = error.to_string();
    assert!(
        message.starts_with(
            "tmux older than 3.2 is not supported; libtmux requires tmux 3.3a or newer"
        ),
        "{message}"
    );
    assert!(
        message.contains("Upgrade tmux and restart the server"),
        "{message}"
    );
    assert!(!marker.exists(), "nothing was sent to the daemon");
    server.shutdown().await.expect("client shuts down");
}

/// A real tmux older than the probe: the configured executable is a 3.0a or
/// 3.1c build, so the client itself rejects the probe's flags.
#[tokio::test]
async fn real_tmux_compat_pre_probe_daemon_is_refused_as_unsupported() {
    let Some(old) = optional_binary("LIBTMUX_TEST_PRE_PROBE_TMUX") else {
        return;
    };
    std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned fixture directory");
    let socket = directory.path().join("s");
    let started = native(
        &old,
        &socket,
        &[OsStr::new("new-session"), OsStr::new("-d")],
    );
    assert!(started.status.success(), "{started:?}");
    let server = Server::builder()
        .socket_path(&socket)
        .tmux_executable(&old)
        .build()
        .expect("client configured");
    let error = server
        .new_session("refused")
        .await
        .expect_err("a pre-3.2 daemon is refused");
    assert!(
        matches!(error, Error::UnsupportedTmuxVersion { .. }),
        "{error:?}"
    );
    assert!(
        error
            .to_string()
            .starts_with("tmux older than 3.2 is not supported"),
        "{error}"
    );
    let listing = native(&old, &socket, &[OsStr::new("list-sessions")]);
    assert_eq!(
        String::from_utf8_lossy(&listing.stdout).lines().count(),
        1,
        "only the native session exists: {listing:?}"
    );
    native(&old, &socket, &[OsStr::new("kill-server")]);
    server.shutdown().await.expect("client shuts down");
}

/// A report that names no release cannot be compared with the floor, so it is
/// refused as unrecognized rather than described as too old.
#[tokio::test]
async fn a_version_that_names_no_release_is_refused_as_unrecognized() {
    for reported in ["master", "openbsd-7.5", "3.3a-dev"] {
        let Ok(version) =
            libtmux::TmuxVersion::parse_output(format!("tmux {reported}\n").as_bytes())
        else {
            continue;
        };
        let error = version
            .ensure_supported()
            .expect_err("no release to compare");
        assert!(
            matches!(error, Error::UnrecognizedTmuxVersion { .. }),
            "{reported}: {error:?}"
        );
        assert_eq!(error.kind(), libtmux::ErrorKind::UnsupportedVersion);
        let message = error.to_string();
        assert!(
            !message.contains("below") && !message.contains("needs"),
            "{message}"
        );
        assert!(message.contains("released tmux build"), "{message}");
    }
    // The control: a plain old release still reads as too old.
    let old = libtmux::TmuxVersion::parse_output(b"tmux 3.2a\n").expect("parses");
    assert!(matches!(
        old.ensure_supported(),
        Err(Error::UnsupportedTmuxVersion { .. })
    ));
}

#[tokio::test]
async fn version_check_timeout_is_charged_to_the_logical_request() {
    std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned fixture directory");
    let executable = wrapper(directory.path(), "exec sleep 30");
    let server = Server::builder()
        .socket_path(directory.path().join("s"))
        .tmux_executable(executable)
        .default_timeout(Duration::from_millis(100))
        .build()
        .expect("client configured");
    let error = server
        .cmd(Command::new("set-buffer").sensitive_arg("secret"))
        .await
        .expect_err("probe deadline expires");
    assert!(
        matches!(error, Error::Timeout { request_id: 1, ref command, .. } if command.to_string().starts_with("\"set-buffer\" ")),
        "{error:?}"
    );
    server
        .shutdown()
        .await
        .expect("shutdown reaps the cancelled probe");
}

#[tokio::test]
async fn version_check_and_dispatch_share_one_deadline() {
    std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned fixture directory");
    let executable = wrapper(
        directory.path(),
        "case \"$*\" in -V) printf 'tmux 3.7d\\n';; *'tmux '#*) sleep 0.2; printf 'tmux 3.7d\\n';; *) exec sleep 0.2;; esac",
    );
    let server = Server::builder()
        .socket_path(directory.path().join("s"))
        .tmux_executable(executable)
        .default_timeout(Duration::from_millis(300))
        .build()
        .expect("client configured");
    let error = server
        .cmd(Command::new("set-buffer").sensitive_arg("secret"))
        .await
        .expect_err("two individually bounded phases exceed the shared deadline");
    assert!(
        matches!(error, Error::Timeout { request_id: 1, .. }),
        "{error:?}"
    );
    server
        .shutdown()
        .await
        .expect("cancelled dispatch is reaped");
}

#[tokio::test]
async fn cancellation_during_version_check_reaps_the_probe() {
    std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned fixture directory");
    let ready = directory.path().join("ready");
    let executable = wrapper(
        directory.path(),
        &format!(
            "printf '%s' \"$$\" > {}; exec sleep 30",
            shell_quote(ready.as_os_str())
        ),
    );
    let server = Server::builder()
        .socket_path(directory.path().join("s"))
        .tmux_executable(executable)
        .build()
        .expect("client configured");
    let caller = server.clone();
    let task = tokio::spawn(async move { caller.cmd(Command::new("new-session").arg("-d")).await });
    let pid = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(&ready)
                .unwrap_or_default()
                .parse::<i32>()
            {
                break pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("probe starts");
    task.abort();
    assert!(task.await.expect_err("caller cancelled").is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), server.shutdown())
        .await
        .expect("probe cleanup completes")
        .expect("executor shuts down");
    let pid = rustix::process::Pid::from_raw(pid).expect("positive PID");
    assert_eq!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH)
    );
}

#[tokio::test]
async fn an_unrepresentable_deadline_retains_the_unbounded_budget() {
    let guard = TestServer::new().await.expect("daemon starts");
    let server = Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(current_binary())
        .default_timeout(Duration::MAX)
        .build()
        .expect("client configured");
    let result = server
        .cmd(
            Command::new("display-message")
                .arg("-p")
                .arg("--")
                .arg("bounded-by-caller"),
        )
        .await
        .expect("Duration::MAX does not overflow the version check");
    assert_eq!(result.stdout(), b"bounded-by-caller\n");
    server.shutdown().await.expect("client shuts down");
    guard.shutdown().await.expect("fixture shuts down");
}

#[tokio::test]
async fn real_tmux_compat_replacement_between_probe_and_dispatch_refuses_effects() {
    let Some(old) = optional_binary("LIBTMUX_TEST_UNSUPPORTED_TMUX") else {
        return;
    };
    let current = current_binary();
    let guard = TestServer::builder()
        .tmux_executable(&current)
        .start()
        .await
        .expect("supported daemon starts");
    std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("wrapper directory");
    let socket = shell_quote(guard.socket_path().as_os_str());
    let executable = wrapper(
        directory.path(),
        &format!(
            "case \"$*\" in *set-buffer*) {} -S {socket} kill-server; {} -S {socket} -f /dev/null new-session -d -s replacement 'sleep 30';; esac\nexec {} \"$@\"",
            shell_quote(current.as_os_str()),
            shell_quote(old.as_os_str()),
            shell_quote(current.as_os_str())
        ),
    );
    let server = Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(executable)
        .build()
        .expect("client configured");
    let result = server
        .cmd(
            Command::new("set-buffer")
                .arg("-b")
                .arg("must-not-write")
                .arg("payload"),
        )
        .await;
    let buffers = native(&old, guard.socket_path(), &[OsStr::new("list-buffers")]);
    let repeated = server
        .cmd(Command::new("set-buffer").arg("must-not-write-either"))
        .await;
    let cleanup = native(&old, guard.socket_path(), &[OsStr::new("kill-server")]);
    assert!(cleanup.status.success(), "replacement is stopped");
    assert_unsupported(
        &repeated.expect_err("a fence refusal invalidates the old cached version"),
        "3.2a",
    );
    assert!(
        matches!(result, Err(Error::VersionProbeFailed { .. })),
        "{result:?}"
    );
    assert!(
        buffers.stdout.is_empty(),
        "replacement received no buffer effect"
    );
    server.shutdown().await.expect("client shuts down");
    guard.shutdown().await.expect("fixture shuts down");
}

async fn assert_undated_master_refused(cold: bool) {
    std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned directory");
    let marker = directory.path().join("effect");
    let endpoint = if cold {
        "printf 'no server running on fixture\\n' >&2; exit 1"
    } else {
        "printf 'tmux master\\n'"
    };
    let executable = wrapper(
        directory.path(),
        &format!(
            "case \"$*\" in *-V) printf 'tmux master\\n';; *'tmux '#*) {endpoint};; *) touch {};; esac",
            shell_quote(marker.as_os_str())
        ),
    );
    let server = Server::builder()
        .socket_path(directory.path().join("s"))
        .tmux_executable(executable)
        .build()
        .expect("client configured");
    assert_eq!(
        server
            .capabilities()
            .await
            .expect("diagnostic parses master")
            .tmux_version()
            .raw(),
        "master"
    );
    assert_eq!(
        server
            .cmd(Command::new("-V"))
            .await
            .expect("diagnostic remains available")
            .stdout(),
        b"tmux master\n"
    );
    let result = server.cmd(Command::new("new-session").arg("-d")).await;
    let effect = marker.exists();
    server.shutdown().await.expect("client shuts down");
    assert!(!effect, "undated master ran a requested effect");
    let error = result.expect_err("undated master fails the version check");
    assert!(
        matches!(error, Error::UnrecognizedTmuxVersion { .. }),
        "{error:?}"
    );
    assert_eq!(
        error.found_version().expect("found version").raw(),
        "master"
    );
}

#[tokio::test]
async fn strict_version_check_refuses_undated_daemon() {
    assert_undated_master_refused(false).await;
}

#[tokio::test]
async fn strict_version_check_refuses_undated_creating_executable() {
    assert_undated_master_refused(true).await;
}

#[tokio::test]
async fn strict_version_check_does_not_reuse_creating_executable_permission() {
    let Some(old) = optional_binary("LIBTMUX_TEST_UNSUPPORTED_TMUX") else {
        return;
    };
    let current = current_binary();
    let guard = TestServer::builder()
        .tmux_executable(&current)
        .start()
        .await
        .expect("daemon starts");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned directory");
    let old_mode = directory.path().join("old-mode");
    let startup = directory.path().join("started");
    let config = directory.path().join("tmux.conf");
    std::fs::write(
        &config,
        format!("run-shell \"touch {}\"\n", shell_quote(startup.as_os_str())),
    )
    .expect("startup witness configured");
    let executable = wrapper(
        directory.path(),
        &format!(
            "if [ -e {} ]; then exec {} \"$@\"; else exec {} \"$@\"; fi",
            shell_quote(old_mode.as_os_str()),
            shell_quote(old.as_os_str()),
            shell_quote(current.as_os_str())
        ),
    );
    let server = Server::builder()
        .socket_path(guard.socket_path())
        .config_file(&config)
        .tmux_executable(executable)
        .build()
        .expect("client configured");
    server
        .cmd(
            Command::new("display-message")
                .arg("-p")
                .arg("--")
                .arg("warm"),
        )
        .await
        .expect("supported client warms the version cache");
    assert!(
        native(&current, guard.socket_path(), &[OsStr::new("kill-server")])
            .status
            .success()
    );
    wait_until_gone(&current, guard.socket_path());
    std::fs::write(&old_mode, "old").expect("configured executable changes");
    let first = server.new_session("must-not-start").await;
    let second = server.new_session("still-must-not-start").await;
    let started = startup.exists();
    let _ = native(&old, guard.socket_path(), &[OsStr::new("kill-server")]);
    server.shutdown().await.expect("client shuts down");
    guard.shutdown().await.expect("fixture shuts down");
    assert!(
        !started,
        "cached permission started the swapped old executable"
    );
    assert!(matches!(first, Err(Error::ServerGone { .. })), "{first:?}");
    assert_unsupported(
        &second.expect_err("next explicit call freshly checks old executable"),
        "3.2a",
    );
}

#[tokio::test]
async fn strict_version_check_supports_explicit_restarts_on_a_warmed_handle() {
    let current = current_binary();
    let guard = TestServer::builder()
        .tmux_executable(&current)
        .start()
        .await
        .expect("daemon starts");
    let server = Server::builder()
        .socket_path(guard.socket_path())
        .config_file("/dev/null")
        .tmux_executable(&current)
        .build()
        .expect("client configured");
    server
        .new_session("original")
        .await
        .expect("the version cache warms");
    server.kill().await.expect("owned kill succeeds");
    wait_until_gone(&current, guard.socket_path());
    let owned_restart = server.new_session("after-owned-kill").await;
    let _ = native(&current, guard.socket_path(), &[OsStr::new("kill-server")]);
    wait_until_gone(&current, guard.socket_path());
    let disappeared = server.new_session("disappeared").await;
    let explicit_restart = server.new_session("after-disappearance").await;
    let _ = native(&current, guard.socket_path(), &[OsStr::new("kill-server")]);
    server.shutdown().await.expect("client shuts down");
    guard.shutdown().await.expect("fixture shuts down");
    assert!(owned_restart.is_ok(), "{owned_restart:?}");
    assert!(
        matches!(disappeared, Err(Error::ServerGone { .. })),
        "{disappeared:?}"
    );
    assert!(explicit_restart.is_ok(), "{explicit_restart:?}");
}

async fn assert_fresh_start_after_cold_metadata(swap: bool) {
    let Some(old) = optional_binary("LIBTMUX_TEST_UNSUPPORTED_TMUX") else {
        return;
    };
    std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned directory");
    let socket = directory.path().join("s");
    let old_mode = directory.path().join("old-mode");
    let startup = directory.path().join("started");
    let config = directory.path().join("tmux.conf");
    std::fs::write(
        &config,
        format!("run-shell \"touch {}\"\n", shell_quote(startup.as_os_str())),
    )
    .expect("startup witness configured");
    let executable = wrapper(
        directory.path(),
        &format!(
            "if [ -e {} ]; then exec {} \"$@\"; else exec {} \"$@\"; fi",
            shell_quote(old_mode.as_os_str()),
            shell_quote(old.as_os_str()),
            shell_quote(current_binary().as_os_str())
        ),
    );
    let server = Server::builder()
        .socket_path(&socket)
        .config_file(&config)
        .tmux_executable(executable)
        .build()
        .expect("client configured");
    server
        .validate_layouts(std::iter::empty())
        .await
        .expect("cold metadata preflight succeeds");
    if swap {
        std::fs::write(&old_mode, "old").expect("executable changes after preflight");
    }
    let result = server.new_session("freshly-checked").await;
    let started = startup.exists();
    let _ = native(&old, &socket, &[OsStr::new("kill-server")]);
    server.shutdown().await.expect("client shuts down");
    if swap {
        assert!(!started, "cold metadata granted stale creating permission");
        assert_unsupported(
            &result.expect_err("creating executable is freshly checked"),
            "3.2a",
        );
    } else {
        assert!(result.is_ok(), "{result:?}");
        assert!(started, "fresh supported startup ran its configuration");
    }
}

#[tokio::test]
async fn strict_version_check_cold_metadata_cannot_qualify_a_changed_executable() {
    assert_fresh_start_after_cold_metadata(true).await;
}

#[tokio::test]
async fn strict_version_check_cold_metadata_preserves_the_first_explicit_start() {
    assert_fresh_start_after_cold_metadata(false).await;
}
