//! Daemon floor enforcement across native versions and dispatch lifetimes.

#![cfg(feature = "test-support")]
#![allow(clippy::expect_used)]

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};
use std::time::Duration;

use libtmux::test::TestServer;
use libtmux::{Command, CommandChain, Error, Server};

fn optional_binary(variable: &str) -> Option<PathBuf> {
    let Some(binary) = std::env::var_os(variable) else {
        eprintln!("{variable} unset; native boundary case skipped");
        return None;
    };
    Some(PathBuf::from(binary))
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
        .expect_err("empty layout preflight still admits the product endpoint");
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
        .expect_err("chains do not exempt client flags from admission");
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
    let path = directory.join("tmux-wrapper");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("wrapper written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
        .expect("wrapper executable");
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

#[tokio::test]
async fn admission_timeout_is_charged_to_the_logical_request() {
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
async fn admission_and_dispatch_share_one_deadline() {
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
async fn cancellation_during_admission_reaps_the_probe() {
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
        .expect("Duration::MAX does not overflow admission");
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
        &repeated.expect_err("a fence refusal invalidates the old admitted version"),
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
