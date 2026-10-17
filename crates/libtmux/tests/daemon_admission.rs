//! Daemon floor enforcement across native versions and dispatch lifetimes.

#![cfg(feature = "test-support")]
#![allow(clippy::expect_used)]

use std::ffi::OsStr;
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

/// Shell that answers the hook-free version probe the way tmux does: it
/// refuses the marker string and reports the format-expanded text on stderr.
fn probe_report(version: &str) -> String {
    format!(
        "for a; do case $a in libtmux-version-*) m=${{a%%#*}};; esac; done; printf 'unknown command: %s{version}\\n' \"$m\" >&2; exit 1"
    )
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
        probe_report("unknown"),
        probe_report("3.3rc1"),
        probe_report("3.3a-rc1"),
        probe_report("next-3.3a"),
        probe_report("3.3a-extra"),
        probe_report("3.3a-master"),
        "exit 23".to_owned(),
    ] {
        std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
        let directory =
            tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned fixture directory");
        let marker = directory.path().join("effect");
        let executable = wrapper(
            directory.path(),
            &format!(
                "case \"$*\" in *libtmux-version-*) {output};; *) touch {}; exit 0;; esac",
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
        &format!(
            "case \"$*\" in -V) printf 'tmux 3.7d\\n';; *libtmux-version-*) sleep 0.2; {};; *) exec sleep 0.2;; esac",
            probe_report("3.7d")
        ),
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

async fn assert_undated_master_refused(cold: bool) {
    std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned directory");
    let marker = directory.path().join("effect");
    let endpoint = if cold {
        "printf 'no server running on fixture\\n' >&2; exit 1".to_owned()
    } else {
        probe_report("master")
    };
    let executable = wrapper(
        directory.path(),
        &format!(
            "case \"$*\" in *-V) printf 'tmux master\\n';; *libtmux-version-*) {endpoint};; *) touch {};; esac",
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
    assert!(!effect, "undated master admitted a requested effect");
    assert_unsupported(
        &result.expect_err("undated master refuses admission"),
        "master",
    );
}

#[tokio::test]
async fn strict_admission_refuses_undated_daemon() {
    assert_undated_master_refused(false).await;
}

#[tokio::test]
async fn strict_admission_refuses_undated_creating_executable() {
    assert_undated_master_refused(true).await;
}

#[tokio::test]
async fn strict_admission_does_not_reuse_creating_executable_permission() {
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
        .expect("supported client warms admission");
    assert!(
        native(&current, guard.socket_path(), &[OsStr::new("kill-server")])
            .status
            .success()
    );
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
async fn strict_admission_supports_explicit_restarts_on_a_warmed_handle() {
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
        .expect("admission warms");
    server.kill().await.expect("owned kill succeeds");
    let owned_restart = server.new_session("after-owned-kill").await;
    let _ = native(&current, guard.socket_path(), &[OsStr::new("kill-server")]);
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
    let result = server.new_session("freshly-admitted").await;
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
async fn strict_admission_cold_metadata_cannot_qualify_a_changed_executable() {
    assert_fresh_start_after_cold_metadata(true).await;
}

#[tokio::test]
async fn strict_admission_cold_metadata_preserves_the_first_explicit_start() {
    assert_fresh_start_after_cold_metadata(false).await;
}

#[tokio::test]
async fn strict_probe_program_prefixes_refuse_unrelated_reports_before_effects() {
    for cold in [false, true] {
        std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
        let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned fixture");
        let marker = directory.path().join("effect");
        let endpoint = if cold {
            "printf 'no server running on missing\\n' >&2; exit 1".to_owned()
        } else {
            "printf 'unrelated 3.7d\\n'".to_owned()
        };
        let executable = wrapper(
            directory.path(),
            &format!(
                "case \"$*\" in *-V) printf 'unrelated 3.7d\\n';; *libtmux-version-*) {endpoint};; *) touch {};; esac",
                shell_quote(marker.as_os_str())
            ),
        );
        let server = Server::builder()
            .socket_path(directory.path().join("s"))
            .tmux_executable(executable)
            .build()
            .expect("client");
        assert_eq!(
            server
                .capabilities()
                .await
                .expect("diagnostic framing remains available")
                .tmux_version()
                .raw(),
            "3.7d"
        );
        let result = server.cmd(Command::new("new-session").arg("-d")).await;
        assert!(
            !marker.exists(),
            "unrelated program report admitted effects; cold={cold}"
        );
        assert!(
            matches!(result, Err(Error::InvalidVersionOutput { .. })),
            "unrelated prefix refuses admission; cold={cold}"
        );
        server.shutdown().await.expect("client closes");
    }
}

#[tokio::test]
async fn strict_probe_accepts_the_actual_3_3a_creating_executable_basename() {
    let Some(minimum) = optional_binary("LIBTMUX_TEST_MINIMUM_TMUX") else {
        return;
    };
    std::fs::create_dir_all("/tmp/libtmux-rs-test").expect("fixture root");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned fixture");
    let renamed = directory.path().join("renamed-tmux");
    std::os::unix::fs::symlink(minimum, &renamed).expect("native executable symlink");
    let report = ProcessCommand::new(&renamed)
        .arg("-V")
        .output()
        .expect("native diagnostic");
    assert_eq!(
        report.stdout, b"renamed-tmux 3.3a\n",
        "native3.3a prints argv0 basename"
    );
    let server = Server::builder()
        .socket_path(directory.path().join("s"))
        .config_file("/dev/null")
        .tmux_executable(&renamed)
        .build()
        .expect("client");
    let result = async {
        server.new_session("qualified").await?;
        assert_eq!(server.sessions().await?.len(), 1);
        Ok::<_, Error>(())
    }
    .await;
    let _ = server.kill().await;
    server.shutdown().await.expect("client closes");
    result.expect("renamed supported executable can explicitly start its daemon");
}

fn hook_marker(binary: &Path, socket: &Path, name: &str) -> String {
    let output = native(
        binary,
        socket,
        &[
            OsStr::new("show-options"),
            OsStr::new("-gqv"),
            OsStr::new(name),
        ],
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn reset_hook_marker(binary: &Path, socket: &Path, name: &str) {
    native(
        binary,
        socket,
        &[
            OsStr::new("set-option"),
            OsStr::new("-g"),
            OsStr::new(name),
            OsStr::new(""),
        ],
    );
}

/// The probe that admits a daemon must not have effects of its own.
/// `display-message` fires `after-display-message` on every release, so a
/// hook configured by the daemon's owner ran before admission had decided
/// anything.
#[tokio::test]
async fn admission_probe_fires_no_user_hook() {
    let binary = current_binary();
    let guard = TestServer::builder()
        .tmux_executable(&binary)
        .start()
        .await
        .expect("daemon starts");
    let socket = guard.socket_path();
    // Hooks resolve against a session; a bare server runs none of them.
    assert!(
        native(
            &binary,
            socket,
            &[
                OsStr::new("new-session"),
                OsStr::new("-d"),
                OsStr::new("-s"),
                OsStr::new("hooked"),
                OsStr::new("sh"),
            ],
        )
        .status
        .success()
    );
    let set = |event: &str, option: &str| {
        native(
            &binary,
            socket,
            &[
                OsStr::new("set-hook"),
                OsStr::new("-g"),
                OsStr::new(event),
                OsStr::new(&format!("set-option -g {option} fired")),
            ],
        )
        .status
        .success()
    };
    assert!(
        set("after-display-message", "@after_display_message"),
        "after-display-message exists on every supported release"
    );
    // `command-error` arrived in tmux 3.5; older releases refuse the hook.
    let has_command_error = set("command-error", "@command_error");

    // Controls: each hook is live, so an empty marker below means the probe
    // did not fire it rather than that the hook never worked.
    native(
        &binary,
        socket,
        &[
            OsStr::new("display-message"),
            OsStr::new("-p"),
            OsStr::new("x"),
        ],
    );
    assert_eq!(
        hook_marker(&binary, socket, "@after_display_message"),
        "fired",
        "control: display-message fires its hook"
    );
    reset_hook_marker(&binary, socket, "@after_display_message");
    if has_command_error {
        native(
            &binary,
            socket,
            &[
                OsStr::new("kill-session"),
                OsStr::new("-t"),
                OsStr::new("=absent"),
            ],
        );
        assert_eq!(
            hook_marker(&binary, socket, "@command_error"),
            "fired",
            "control: a failing command fires command-error"
        );
        reset_hook_marker(&binary, socket, "@command_error");
    }

    // A fresh client handle has no admitted version, so this dispatch runs the
    // endpoint probe.
    let server = Server::builder()
        .socket_path(socket)
        .tmux_executable(&binary)
        .build()
        .expect("client configured");
    let result = server
        .cmd(Command::new("list-sessions"))
        .await
        .expect("admitted dispatch runs");
    assert!(result.success(), "{result:?}");

    assert_eq!(
        hook_marker(&binary, socket, "@after_display_message"),
        "",
        "admission must not fire after-display-message"
    );
    if has_command_error {
        assert_eq!(
            hook_marker(&binary, socket, "@command_error"),
            "",
            "admission must not fire command-error"
        );
    }
    server.shutdown().await.expect("client shuts down");
    guard.shutdown().await.expect("fixture shuts down");
}
