//! Integration tests for the public isolated real-tmux guard.

#![cfg(feature = "test-support")]
// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::error::Error as StdError;
use std::ffi::OsStr;
use std::fmt::{Debug, Display};
use std::fs::{self, File};
use std::future::Future as _;
use std::io::Write as _;
#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{self, Child, Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

use libtmux::Command;
use libtmux::test::{
    TestServer, TestServerBuilder, TestServerError, TestServerErrorKind, retry_until,
};
use rustix::io::Errno;
use rustix::process::{
    Pid, Signal, WaitOptions, getpgid, getpgrp, kill_process, test_kill_process, waitpid,
};
#[cfg(target_os = "linux")]
use rustix::process::{PidfdFlags, pidfd_open, pidfd_send_signal};
use static_assertions::{assert_impl_all, assert_not_impl_any};

assert_impl_all!(TestServer: Debug, Send, Sync);
assert_not_impl_any!(TestServer: Clone);
assert_impl_all!(TestServerBuilder: Debug, Send, Sync);
assert_impl_all!(TestServerError: Debug, Display, StdError, Send, Sync);
assert_impl_all!(TestServerErrorKind: Clone, Copy, Debug, Eq, Send, Sync);

const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(5);
const UMASK_CHILD: &str = "LIBTMUX_TEST_SERVER_UMASK_CHILD";
const ENVIRONMENT_CHILD: &str = "LIBTMUX_TEST_SERVER_ENVIRONMENT_CHILD";
const CLIENT_ENVIRONMENT_CHILD: &str = "LIBTMUX_TEST_SERVER_CLIENT_ENVIRONMENT_CHILD";

#[test]
fn error_kinds_cover_each_observable_lifecycle_phase() {
    let _ = [
        TestServerErrorKind::FilesystemSetupFailed,
        TestServerErrorKind::SocketPathTooLong,
        TestServerErrorKind::ExecutableNotFound,
        TestServerErrorKind::DaemonSpawnFailed,
        TestServerErrorKind::DaemonExited,
        TestServerErrorKind::ReadinessProbeFailed,
        TestServerErrorKind::DaemonPidMismatch,
        TestServerErrorKind::StartupTimedOut,
        TestServerErrorKind::ShutdownFailed,
        TestServerErrorKind::CleanupFailed,
    ];
}

fn pid(value: u32) -> Pid {
    Pid::from_raw(i32::try_from(value).expect("test PID fits i32")).expect("test PID is nonzero")
}

async fn assert_process_gone(value: u32) {
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        loop {
            if matches!(test_kill_process(pid(value)), Err(Errno::SRCH)) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("process disappears before the observation deadline");
}

async fn wait_for_file(path: &Path) -> String {
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        loop {
            if let Ok(contents) = fs::read_to_string(path) {
                if !contents.is_empty() {
                    return contents;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("child publishes readiness before the observation deadline")
}

async fn wait_for_child_exit(child: &mut Child) -> process::ExitStatus {
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        loop {
            if let Some(status) = child.try_wait().expect("helper status is observable") {
                return status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("helper exits before the observation deadline")
}

#[cfg(not(any(
    target_os = "cygwin",
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
async fn wait_for_unreaped_child_exit(value: u32) {
    use rustix::process::{WaitId, WaitIdOptions, waitid};

    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        loop {
            match waitid(
                WaitId::Pid(pid(value)),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            ) {
                Ok(Some(_)) => return,
                Ok(None) | Err(Errno::INTR) => tokio::task::yield_now().await,
                outcome => panic!("unexpected daemon wait outcome: {outcome:?}"),
            }
        }
    })
    .await
    .expect("daemon exits unreaped before the observation deadline");
}

fn assert_child_already_reaped(value: u32) {
    let child = pid(value);
    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    loop {
        match waitpid(Some(child), WaitOptions::NOHANG) {
            Err(Errno::CHILD) => return,
            Ok(None) | Err(Errno::INTR) if Instant::now() < deadline => {
                std::thread::yield_now();
            }
            Ok(Some((_pid, status))) => {
                panic!("direct client child was not reaped before runtime teardown: {status:?}")
            }
            outcome => panic!("unexpected readiness-child wait outcome: {outcome:?}"),
        }
    }
}

fn assert_child_reaped_on_return(value: u32) {
    match waitpid(Some(pid(value)), WaitOptions::NOHANG) {
        Err(Errno::CHILD) => {}
        outcome => panic!("startup returned before reaping its client child: {outcome:?}"),
    }
}

fn write_executable(path: &Path, script: &str) {
    let staged = path.with_extension("staged");
    let script = script.replacen(
        "#!/bin/sh\n",
        "#!/bin/sh\nif [ \"${LIBTMUX_FAKE_READY:-}\" = 1 ]; then exit 0; fi\n",
        1,
    );
    let mut file = File::create(&staged).expect("fake tmux staging file is created");
    file.write_all(script.as_bytes())
        .expect("fake tmux contents are written");
    file.sync_all().expect("fake tmux contents are durable");
    drop(file);
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))
        .expect("fake tmux is executable");
    fs::rename(&staged, path).expect("fake tmux is installed atomically");

    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    loop {
        match ProcessCommand::new(path)
            .env("LIBTMUX_FAKE_READY", "1")
            .status()
        {
            Ok(status) if status.success() => return,
            Err(error)
                if error.raw_os_error() == Some(Errno::TXTBSY.raw_os_error())
                    && Instant::now() < deadline =>
            {
                std::thread::yield_now();
            }
            Ok(status) => panic!("fake tmux readiness exited with {status}"),
            Err(error) => panic!("fake tmux readiness failed: {error}"),
        }
    }
}

fn shell_literal(path: &Path) -> String {
    let value = path.to_string_lossy();
    shell_text_literal(&value)
}

fn shell_text_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "linux")]
struct PublishedProcess {
    pidfd: OwnedFd,
}

#[cfg(target_os = "linux")]
impl PublishedProcess {
    fn new(value: u32) -> Self {
        Self {
            pidfd: pidfd_open(pid(value), PidfdFlags::empty())
                .expect("published process pidfd opens"),
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for PublishedProcess {
    fn drop(&mut self) {
        let _ = pidfd_send_signal(&self.pidfd, Signal::KILL);
    }
}

#[cfg(target_os = "linux")]
struct UnrelatedMarkerProcess {
    child: Child,
}

#[cfg(target_os = "linux")]
impl UnrelatedMarkerProcess {
    fn spawn() -> Self {
        let mut command = ProcessCommand::new("sh");
        command
            .arg("-c")
            .arg("trap '' HUP INT TERM; while :; do sleep 1; done")
            .env(
                "LIBTMUX_TEST_SERVER_OWNER",
                "parallel-unrelated-marker-sentinel",
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        Self {
            child: command.spawn().expect("unrelated marker process starts"),
        }
    }

    fn assert_alive(&mut self) {
        match self.child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => {
                panic!("cleanup terminated a differently marked process: {status}")
            }
            Err(error) => panic!("differently marked process status is observable: {error}"),
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for UnrelatedMarkerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(target_os = "linux")]
async fn start_detached_real_pane(guard: &TestServer, pid_file: &Path) -> u32 {
    let body = format!(
        "trap '' HUP INT TERM; printf '%s' \"$$\" > {}; while :; do sleep 1; done",
        shell_literal(pid_file),
    );
    let shell_command = format!("exec setsid -f -w sh -c {}", shell_text_literal(&body));
    let result = guard
        .server()
        .cmd(
            Command::new("new-session")
                .arg("-d")
                .arg("-s")
                .arg("contained-pane")
                .arg(shell_command),
        )
        .await
        .expect("detached real pane creation executes");
    assert!(result.success(), "detached real pane starts");
    wait_for_file(pid_file)
        .await
        .parse::<u32>()
        .expect("detached pane PID is numeric")
}

fn fake_tmux_script(
    pid_file: &Path,
    probe_log: Option<&Path>,
    term_marker: Option<&Path>,
    daemon_setup: &str,
    daemon_body: &str,
) -> String {
    let ready_file = pid_file.with_extension("ready");
    let log_probe = probe_log.map_or_else(String::new, |path| {
        format!(
            "printf '%s\\n' \"$@\" > {path}.new\nmv {path}.new {path}\n",
            path = shell_literal(path),
        )
    });
    let term_trap = term_marker.map_or_else(
        || "trap '' TERM\n".to_owned(),
        |path| {
            format!(
                "trap 'printf term > {path}.new; mv {path}.new {path}' TERM\n",
                path = shell_literal(path),
            )
        },
    );
    format!(
        "#!/bin/sh\n\
         daemon=0\n\
         socket=''\n\
         previous=''\n\
         for argument in \"$@\"; do\n\
           if [ \"$argument\" = '-D' ]; then daemon=1; fi\n\
           if [ \"$previous\" = '-S' ]; then socket=$argument; fi\n\
           previous=$argument\n\
         done\n\
         if [ \"$daemon\" = 1 ]; then\n\
           printf '%s' \"$$\" > {pid_file}.new\n\
           mv {pid_file}.new {pid_file}\n\
           : > \"$socket\"\n\
           {term_trap}\
           {daemon_setup}\n\
           printf ready > {ready_file}.new\n\
           mv {ready_file}.new {ready_file}\n\
           {daemon_body}\n\
         fi\n\
         while [ ! -s {ready_file} ]; do :; done\n\
         {log_probe}\
         cat {pid_file}\n",
        pid_file = shell_literal(pid_file),
        ready_file = shell_literal(&ready_file),
    )
}

fn stalled_tmux_script(pid_file: &Path, daemon_body: &str) -> String {
    format!(
        "#!/bin/sh\n\
         daemon=0\n\
         socket=''\n\
         previous=''\n\
         for argument in \"$@\"; do\n\
           if [ \"$argument\" = '-D' ]; then daemon=1; fi\n\
           if [ \"$previous\" = '-S' ]; then socket=$argument; fi\n\
           previous=$argument\n\
         done\n\
         if [ \"$daemon\" = 1 ]; then\n\
           printf '%s' \"$$\" > {pid_file}.new\n\
           mv {pid_file}.new {pid_file}\n\
           : > \"$socket\"\n\
           trap '' TERM\n\
           {daemon_body}\n\
         fi\n\
         exit 1\n",
        pid_file = shell_literal(pid_file),
    )
}

fn blocking_readiness_tmux_script(pid_file: &Path, client_pid_file: &Path) -> String {
    format!(
        "#!/bin/sh\n\
         daemon=0\n\
         socket=''\n\
         previous=''\n\
         for argument in \"$@\"; do\n\
           if [ \"$argument\" = '-D' ]; then daemon=1; fi\n\
           if [ \"$previous\" = '-S' ]; then socket=$argument; fi\n\
           previous=$argument\n\
         done\n\
         if [ \"$daemon\" = 1 ]; then\n\
           printf '%s' \"$$\" > {pid_file}.new\n\
           mv {pid_file}.new {pid_file}\n\
           : > \"$socket\"\n\
           trap '' TERM\n\
           while :; do sleep 1; done\n\
         fi\n\
         printf '%s' \"$$\" > {client_pid_file}.new\n\
         mv {client_pid_file}.new {client_pid_file}\n\
         trap '' TERM\n\
         while :; do sleep 1; done\n",
        pid_file = shell_literal(pid_file),
        client_pid_file = shell_literal(client_pid_file),
    )
}

fn blocking_exposed_client_tmux_script(pid_file: &Path, client_pid_file: &Path) -> String {
    format!(
        "#!/bin/sh\n\
         daemon=0\n\
         block=0\n\
         socket=''\n\
         previous=''\n\
         for argument in \"$@\"; do\n\
           if [ \"$argument\" = '-D' ]; then daemon=1; fi\n\
           if [ \"$argument\" = 'libtmux-block-client' ]; then block=1; fi\n\
           if [ \"$previous\" = '-S' ]; then socket=$argument; fi\n\
           previous=$argument\n\
         done\n\
         if [ \"$daemon\" = 1 ]; then\n\
           printf '%s' \"$$\" > {pid_file}.new\n\
           mv {pid_file}.new {pid_file}\n\
           : > \"$socket\"\n\
           trap '' TERM\n\
           while :; do sleep 1; done\n\
         fi\n\
         while [ ! -s {pid_file} ]; do :; done\n\
         if [ \"$block\" = 1 ]; then\n\
           printf '%s' \"$$\" > {client_pid_file}.new\n\
           mv {client_pid_file}.new {client_pid_file}\n\
           trap '' TERM\n\
           while :; do sleep 1; done\n\
         fi\n\
         cat {pid_file}\n",
        pid_file = shell_literal(pid_file),
        client_pid_file = shell_literal(client_pid_file),
    )
}

#[test]
fn builder_debug_redacts_the_executable() {
    let builder = TestServer::builder()
        .tmux_executable("/tmp/libtmux-SENTINEL-executable")
        .lifecycle_timeout(Duration::from_secs(1));

    let diagnostic = format!("{builder:?}");
    assert!(!diagnostic.contains("SENTINEL"));
}

#[tokio::test]
async fn new_exposes_one_owned_real_server() {
    let guard = TestServer::new().await.expect("real tmux starts");
    let socket = guard.socket_path().to_path_buf();
    let config = guard
        .server()
        .config_file()
        .expect("test server owns its config")
        .to_path_buf();
    let directory = socket
        .parent()
        .expect("socket has an owned directory")
        .to_path_buf();
    let daemon_pid = guard.daemon_pid();

    assert_eq!(guard.server().socket_path(), socket);
    assert_eq!(guard.server().socket_name(), None);
    assert!(daemon_pid > 1);
    assert!(socket.is_absolute());
    assert!(socket.parent().is_some_and(Path::is_dir));
    assert_eq!(socket.parent(), config.parent());
    // The config carries the fixture's own settings and nothing else, so a
    // fixture server still starts from a known state rather than the
    // developer's tmux configuration.
    let configured = fs::read_to_string(&config).expect("config is readable");
    assert_eq!(
        configured.lines().collect::<Vec<_>>(),
        [
            "set -g default-shell /bin/sh",
            "set -g default-command /bin/sh"
        ],
    );
    assert_eq!(
        fs::metadata(&config)
            .expect("config exists")
            .permissions()
            .mode()
            & 0o777,
        0o600,
    );
    assert_eq!(
        fs::metadata(socket.parent().expect("socket has parent"))
            .expect("owned directory exists")
            .permissions()
            .mode()
            & 0o777,
        0o700,
    );
    let diagnostic = format!("{guard:?}");
    assert!(!diagnostic.contains(&socket.to_string_lossy().into_owned()));
    assert!(!diagnostic.contains(&config.to_string_lossy().into_owned()));
    assert_eq!(
        getpgid(Some(pid(daemon_pid))).expect("daemon has a group"),
        pid(daemon_pid)
    );
    assert_ne!(pid(daemon_pid), getpgrp());

    let reported = guard
        .server()
        .cmd(Command::new("display-message").arg("-p").arg("#{pid}"))
        .await
        .expect("PID query executes");
    assert!(reported.success());
    assert_eq!(
        reported.stdout_utf8().expect("tmux PID is UTF-8").trim(),
        daemon_pid.to_string()
    );

    let sessions = guard
        .server()
        .cmd(Command::new("list-sessions"))
        .await
        .expect("list-sessions executes");
    assert!(sessions.success());
    assert!(sessions.stdout().is_empty());

    guard.shutdown().await.expect("fixture shuts down");
    assert_process_gone(daemon_pid).await;
    assert!(!socket.exists());
    assert!(!directory.exists());
}

#[tokio::test]
async fn parallel_guards_have_independent_endpoints_and_daemons() {
    let (left, right) = tokio::join!(TestServer::new(), TestServer::new());
    let left = left.expect("left server starts");
    let right = right.expect("right server starts");

    assert_ne!(left.socket_path(), right.socket_path());
    assert_ne!(left.daemon_pid(), right.daemon_pid());
    assert_ne!(left.server(), right.server());

    let (left_result, right_result) = tokio::join!(left.shutdown(), right.shutdown());
    left_result.expect("left server shuts down");
    right_result.expect("right server shuts down");
}

#[tokio::test]
async fn drop_reaps_the_daemon_and_unlinks_the_socket() {
    let guard = TestServer::new().await.expect("real tmux starts");
    let daemon_pid = guard.daemon_pid();
    let socket = guard.socket_path().to_path_buf();
    let directory = socket
        .parent()
        .expect("socket has an owned directory")
        .to_path_buf();

    drop(guard);

    assert_process_gone(daemon_pid).await;
    assert!(!socket.exists());
    assert!(!directory.exists());
}

#[tokio::test]
async fn unwind_reaps_the_daemon_and_unlinks_the_socket() {
    let guard = TestServer::new().await.expect("real tmux starts");
    let daemon_pid = guard.daemon_pid();
    let socket = guard.socket_path().to_path_buf();
    let directory = socket
        .parent()
        .expect("socket has an owned directory")
        .to_path_buf();

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = guard;
        panic!("intentional fixture unwind");
    }));

    assert!(unwind.is_err());
    assert_process_gone(daemon_pid).await;
    assert!(!socket.exists());
    assert!(!directory.exists());
}

#[test]
fn drop_reaps_after_the_tokio_runtime_is_destroyed() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let guard = runtime
        .block_on(TestServer::new())
        .expect("real tmux starts");
    let daemon_pid = guard.daemon_pid();
    let socket = guard.socket_path().to_path_buf();
    let directory = socket
        .parent()
        .expect("socket has an owned directory")
        .to_path_buf();

    drop(runtime);
    drop(guard);

    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    while !matches!(test_kill_process(pid(daemon_pid)), Err(Errno::SRCH)) {
        assert!(
            Instant::now() < deadline,
            "daemon survives synchronous Drop"
        );
        std::thread::yield_now();
    }
    assert!(!socket.exists());
    assert!(!directory.exists());
}

#[tokio::test]
async fn missing_executable_is_bounded_and_path_free() {
    let error = TestServer::builder()
        .tmux_executable("/tmp/libtmux-SENTINEL-missing")
        .lifecycle_timeout(Duration::from_millis(100))
        .start()
        .await
        .expect_err("missing tmux is rejected");

    assert_eq!(error.kind(), TestServerErrorKind::ExecutableNotFound);
    assert!(StdError::source(&error).is_none());
    assert!(!error.to_string().contains("SENTINEL"));
    assert!(!format!("{error:?}").contains("SENTINEL"));
}

#[tokio::test]
async fn builder_starts_with_consuming_configuration() {
    let guard = TestServer::builder()
        .tmux_executable(OsStr::new("tmux"))
        .lifecycle_timeout(Duration::from_secs(5))
        .start()
        .await
        .expect("configured fixture starts");

    assert_eq!(guard.server().tmux_executable(), OsStr::new("tmux"));
    assert_eq!(guard.server().default_timeout(), Duration::from_secs(30));
    guard.shutdown().await.expect("fixture shuts down");
}

#[tokio::test]
async fn maximum_lifecycle_timeout_does_not_overflow() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let pid_file = fake_root.path().join("pid");
    write_executable(
        &executable,
        &fake_tmux_script(
            &pid_file,
            None,
            None,
            "trap 'exit 0' TERM",
            "while :; do sleep 1; done",
        ),
    );

    let guard = TestServer::builder()
        .tmux_executable(&executable)
        .lifecycle_timeout(Duration::MAX)
        .start()
        .await
        .expect("maximum timeout does not overflow startup");
    guard
        .shutdown()
        .await
        .expect("maximum timeout does not overflow shutdown");
}

#[tokio::test]
async fn readiness_probe_is_no_start_and_uses_the_retained_pid() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let pid_file = fake_root.path().join("pid");
    let probe_log = fake_root.path().join("probe-argv");
    write_executable(
        &executable,
        &fake_tmux_script(
            &pid_file,
            Some(&probe_log),
            None,
            "",
            "while :; do sleep 1; done",
        ),
    );

    let guard = TestServer::builder()
        .tmux_executable(&executable)
        .lifecycle_timeout(Duration::from_secs(2))
        .start()
        .await
        .expect("PID-emulating fixture starts");
    let arguments = fs::read_to_string(&probe_log).expect("readiness argv is recorded");
    let arguments = arguments.lines().collect::<Vec<_>>();
    let no_start = arguments
        .iter()
        .position(|argument| *argument == "-N")
        .expect("readiness uses no-start");
    let display = arguments
        .iter()
        .position(|argument| *argument == "display-message")
        .expect("readiness issues display-message");

    assert!(no_start < display, "-N must precede the readiness command");
    assert_eq!(&arguments[display..], ["display-message", "-p", "#{pid}"]);
    let published_pid = wait_for_file(&pid_file)
        .await
        .parse::<u32>()
        .expect("fake daemon publishes one PID");
    assert_eq!(guard.daemon_pid(), published_pid);

    guard.shutdown().await.expect("fake fixture shuts down");
    assert_process_gone(published_pid).await;
}

#[test]
fn exposed_no_start_clients_use_the_isolated_environment() {
    if std::env::var_os(CLIENT_ENVIRONMENT_CHILD).is_some() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("child runtime builds");
        runtime.block_on(exposed_no_start_clients_use_the_isolated_environment_inner());
        return;
    }

    let executable = std::env::current_exe().expect("test executable path");
    let status = ProcessCommand::new(executable)
        .arg("--exact")
        .arg("exposed_no_start_clients_use_the_isolated_environment")
        .arg("--nocapture")
        .env(CLIENT_ENVIRONMENT_CHILD, "1")
        .env("TERM", "libtmux-SENTINEL-term")
        .env("TMUX", "/tmp/libtmux-SENTINEL-outside,111,0")
        .env("TMUX_PANE", "%999")
        .status()
        .expect("isolated-environment child starts");
    assert!(status.success());
}

async fn exposed_no_start_clients_use_the_isolated_environment_inner() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let pid_file = fake_root.path().join("pid");
    write_executable(
        &executable,
        &format!(
            "#!/bin/sh\n\
             daemon=0\n\
             socket=''\n\
             previous=''\n\
             for argument in \"$@\"; do\n\
               if [ \"$argument\" = '-D' ]; then daemon=1; fi\n\
               if [ \"$previous\" = '-S' ]; then socket=$argument; fi\n\
               previous=$argument\n\
             done\n\
             if [ \"$daemon\" = 1 ]; then\n\
               printf '%s' \"$$\" > {pid}.new\n\
               mv {pid}.new {pid}\n\
               : > \"$socket\"\n\
               trap '' TERM\n\
               sleep 86400 & wait\n\
             fi\n\
             test \"${{TERM:-}}\" = xterm-256color || exit 41\n\
             test -z \"${{TMUX:-}}\" || exit 42\n\
             test -z \"${{TMUX_PANE:-}}\" || exit 43\n\
             cat {pid}\n",
            pid = shell_literal(&pid_file),
        ),
    );

    let guard = TestServer::builder()
        .tmux_executable(&executable)
        .start()
        .await
        .expect("readiness client uses the isolated environment");
    let command = guard
        .server()
        .cmd(Command::new("display-message").arg("-p").arg("#{pid}"))
        .await
        .expect("exposed client executes");
    assert!(command.success());

    guard.shutdown().await.expect("fixture shuts down");
}

#[tokio::test]
async fn construction_failure_after_spawn_reaps_the_child() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let pid_file = fake_root.path().join("pid");
    write_executable(
        &executable,
        &format!(
            "#!/bin/sh\n\
             for argument in \"$@\"; do\n\
               if [ \"$argument\" = '-D' ]; then\n\
                 printf '%s' \"$$\" > {pid}.new\n\
                 mv {pid}.new {pid}\n\
                 printf 'libtmux-SENTINEL-daemon-stdout\\n'\n\
                 printf 'libtmux-SENTINEL-daemon-stderr\\n' >&2\n\
                 exit 23\n\
               fi\n\
             done\n\
             exit 1\n",
            pid = shell_literal(&pid_file),
        ),
    );

    let error = TestServer::builder()
        .tmux_executable(&executable)
        .lifecycle_timeout(Duration::from_secs(1))
        .start()
        .await
        .expect_err("exited foreground daemon is rejected");
    let daemon_pid = wait_for_file(&pid_file)
        .await
        .parse::<u32>()
        .expect("fake daemon publishes one PID");

    assert_eq!(error.kind(), TestServerErrorKind::DaemonExited);
    assert!(StdError::source(&error).is_none());
    assert!(!error.to_string().contains("SENTINEL"));
    assert!(!format!("{error:?}").contains("SENTINEL"));
    assert_process_gone(daemon_pid).await;
}

#[tokio::test]
async fn startup_rollback_reports_descriptor_cleanup_failure() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let pid_file = fake_root.path().join("pid");
    let socket_file = fake_root.path().join("socket");
    write_executable(
        &executable,
        &format!(
            "#!/bin/sh\n\
             daemon=0\n\
             socket=''\n\
             previous=''\n\
             for argument in \"$@\"; do\n\
               if [ \"$argument\" = '-D' ]; then daemon=1; fi\n\
               if [ \"$previous\" = '-S' ]; then socket=$argument; fi\n\
               previous=$argument\n\
             done\n\
             if [ \"$daemon\" = 1 ]; then\n\
               printf '%s' \"$$\" > {pid}.new\n\
               mv {pid}.new {pid}\n\
               : > \"$socket\"\n\
               : > \"$socket.unknown\"\n\
               printf '%s' \"$socket\" > {socket}.new\n\
               mv {socket}.new {socket}\n\
               trap '' TERM\n\
               sleep 86400 & wait\n\
             fi\n\
             while [ ! -s {pid} ] || [ ! -s {socket} ]; do :; done\n\
             printf '4294967295\\n'\n",
            pid = shell_literal(&pid_file),
            socket = shell_literal(&socket_file),
        ),
    );

    let error = TestServer::builder()
        .tmux_executable(&executable)
        .lifecycle_timeout(Duration::from_secs(1))
        .start()
        .await
        .expect_err("mismatched daemon PID rolls startup back");
    let error_kind = error.kind();
    let daemon_pid = wait_for_file(&pid_file)
        .await
        .parse::<u32>()
        .expect("fake daemon publishes one PID");
    let socket = std::path::PathBuf::from(wait_for_file(&socket_file).await);
    let unknown = socket.with_extension("unknown");
    let directory = socket
        .parent()
        .expect("fake socket has an owned directory")
        .to_path_buf();

    assert_process_gone(daemon_pid).await;
    assert!(unknown.exists(), "unknown entry prevents directory removal");

    fs::remove_file(unknown).expect("test removes the unknown entry");
    fs::remove_dir(directory).expect("test removes the retained directory");
    assert_eq!(error_kind, TestServerErrorKind::CleanupFailed);
}

#[tokio::test]
async fn exited_construction_leader_still_cleans_its_same_group_descendant() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let pid_file = fake_root.path().join("pids");
    write_executable(
        &executable,
        &format!(
            "#!/bin/sh\n\
             for argument in \"$@\"; do\n\
               if [ \"$argument\" = '-D' ]; then\n\
                 (trap '' TERM; sleep 86400 & wait) &\n\
                 helper=$!\n\
                 printf '%s\\n%s\\n' \"$$\" \"$helper\" > {pids}.new\n\
                 mv {pids}.new {pids}\n\
                 exit 23\n\
               fi\n\
             done\n\
             exit 1\n",
            pids = shell_literal(&pid_file),
        ),
    );

    let error = TestServer::builder()
        .tmux_executable(&executable)
        .lifecycle_timeout(Duration::from_millis(200))
        .start()
        .await
        .expect_err("exited foreground daemon is rejected");
    let pids = wait_for_file(&pid_file)
        .await
        .lines()
        .map(|value| value.parse::<u32>().expect("published PID is numeric"))
        .collect::<Vec<_>>();
    assert_eq!(pids.len(), 2);

    assert_eq!(error.kind(), TestServerErrorKind::DaemonExited);
    assert_process_gone(pids[0]).await;
    assert_process_gone(pids[1]).await;
}

#[tokio::test]
async fn aborting_startup_reaps_the_owned_process_group() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let pid_file = fake_root.path().join("pid");
    write_executable(
        &executable,
        &stalled_tmux_script(&pid_file, "while :; do sleep 1; done"),
    );

    let mut startup = tokio::spawn(
        TestServer::builder()
            .tmux_executable(&executable)
            .lifecycle_timeout(Duration::from_secs(2))
            .start(),
    );
    let daemon_pid = tokio::select! {
        contents = wait_for_file(&pid_file) => contents.parse::<u32>().expect("fake daemon PID"),
        result = &mut startup => panic!("startup ended before abort: {result:?}"),
    };

    startup.abort();
    assert!(
        startup
            .await
            .expect_err("startup task is aborted")
            .is_cancelled()
    );
    assert_process_gone(daemon_pid).await;
}

#[tokio::test]
async fn unresponsive_startup_is_bounded_and_reaps_the_group() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let pid_file = fake_root.path().join("pid");
    write_executable(
        &executable,
        &stalled_tmux_script(&pid_file, "while :; do sleep 1; done"),
    );
    let started = Instant::now();

    let error = TestServer::builder()
        .tmux_executable(&executable)
        .lifecycle_timeout(Duration::from_millis(150))
        .start()
        .await
        .expect_err("unready daemon times out");
    let daemon_pid = wait_for_file(&pid_file)
        .await
        .parse::<u32>()
        .expect("fake daemon PID");

    assert_eq!(error.kind(), TestServerErrorKind::StartupTimedOut);
    assert!(started.elapsed() < OBSERVATION_TIMEOUT);
    assert_process_gone(daemon_pid).await;
}

#[test]
fn startup_timeout_reaps_readiness_child_before_runtime_teardown() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let daemon_pid_file = fake_root.path().join("daemon-pid");
    let client_pid_file = fake_root.path().join("client-pid");
    write_executable(
        &executable,
        &blocking_readiness_tmux_script(&daemon_pid_file, &client_pid_file),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime builds");

    let error = runtime
        .block_on(
            TestServer::builder()
                .tmux_executable(&executable)
                .lifecycle_timeout(Duration::from_millis(250))
                .start(),
        )
        .expect_err("blocked readiness times out");
    let readiness_pid = fs::read_to_string(&client_pid_file)
        .expect("readiness client publishes its PID")
        .parse::<u32>()
        .expect("readiness client PID is numeric");

    assert_eq!(error.kind(), TestServerErrorKind::StartupTimedOut);
    assert_child_reaped_on_return(readiness_pid);
    drop(runtime);
    assert_child_already_reaped(readiness_pid);
}

#[test]
fn aborting_startup_reaps_readiness_child_before_runtime_teardown() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let daemon_pid_file = fake_root.path().join("daemon-pid");
    let client_pid_file = fake_root.path().join("client-pid");
    write_executable(
        &executable,
        &blocking_readiness_tmux_script(&daemon_pid_file, &client_pid_file),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime builds");

    runtime.block_on(async {
        let mut startup = Box::pin(
            TestServer::builder()
                .tmux_executable(&executable)
                .lifecycle_timeout(Duration::MAX)
                .start(),
        );
        let deadline = Instant::now() + OBSERVATION_TIMEOUT;
        std::future::poll_fn(|context| {
            if client_pid_file.exists() {
                return std::task::Poll::Ready(());
            }
            if let std::task::Poll::Ready(result) = startup.as_mut().poll(context) {
                panic!("startup ended before readiness client published its PID: {result:?}");
            }
            assert!(
                Instant::now() < deadline,
                "readiness client publishes its PID before the observation deadline"
            );
            context.waker().wake_by_ref();
            std::task::Poll::Pending
        })
        .await;
        drop(startup);
    });
    drop(runtime);
    let readiness_pid = fs::read_to_string(&client_pid_file)
        .expect("readiness client publishes its PID")
        .parse::<u32>()
        .expect("readiness client PID is numeric");

    assert_child_already_reaped(readiness_pid);
}

#[cfg(not(any(
    target_os = "cygwin",
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
#[tokio::test]
async fn pending_readiness_timeout_reports_an_exited_leader() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let pid_file = fake_root.path().join("pid");
    let client_ready = fake_root.path().join("client-ready");
    write_executable(
        &executable,
        &format!(
            "#!/bin/sh\n\
             daemon=0\n\
             socket=''\n\
             previous=''\n\
             for argument in \"$@\"; do\n\
               if [ \"$argument\" = '-D' ]; then daemon=1; fi\n\
               if [ \"$previous\" = '-S' ]; then socket=$argument; fi\n\
               previous=$argument\n\
             done\n\
             if [ \"$daemon\" = 1 ]; then\n\
               printf '%s' \"$$\" > {pid}.new\n\
               mv {pid}.new {pid}\n\
               : > \"$socket\"\n\
               sleep 86400 & wait\n\
             fi\n\
             while [ ! -s {pid} ]; do :; done\n\
             kill -TERM \"$(cat {pid})\"\n\
             printf ready > {ready}.new\n\
             mv {ready}.new {ready}\n\
             sleep 86400 & wait\n",
            pid = shell_literal(&pid_file),
            ready = shell_literal(&client_ready),
        ),
    );

    let mut startup = tokio::spawn(
        TestServer::builder()
            .tmux_executable(&executable)
            .lifecycle_timeout(Duration::from_secs(2))
            .start(),
    );
    tokio::select! {
        _ = wait_for_file(&client_ready) => {}
        result = &mut startup => panic!("startup ended before readiness client signaled: {result:?}"),
    }
    let daemon_pid = wait_for_file(&pid_file)
        .await
        .parse::<u32>()
        .expect("fake daemon publishes one PID");
    tokio::select! {
        () = wait_for_unreaped_child_exit(daemon_pid) => {}
        result = &mut startup => panic!("startup ended before daemon exit was observed: {result:?}"),
    }
    let error = startup
        .await
        .expect("startup task completes")
        .expect_err("leader exits while the readiness client is pending");

    assert_eq!(error.kind(), TestServerErrorKind::DaemonExited);
    assert_process_gone(daemon_pid).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn externally_reaped_daemon_reports_shutdown_failed_and_retains_root() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let pid_file = fake_root.path().join("pid");
    let socket_file = fake_root.path().join("socket");
    let client_ready = fake_root.path().join("client-ready");
    write_executable(
        &executable,
        &format!(
            "#!/bin/sh\n\
             daemon=0\n\
             socket=''\n\
             previous=''\n\
             for argument in \"$@\"; do\n\
               if [ \"$argument\" = '-D' ]; then daemon=1; fi\n\
               if [ \"$previous\" = '-S' ]; then socket=$argument; fi\n\
               previous=$argument\n\
             done\n\
             if [ \"$daemon\" = 1 ]; then\n\
               printf '%s' \"$$\" > {pid}.new\n\
               mv {pid}.new {pid}\n\
               printf '%s' \"$socket\" > {socket}.new\n\
               mv {socket}.new {socket}\n\
               : > \"$socket\"\n\
               sleep 86400 & wait\n\
             fi\n\
             while [ ! -s {pid} ]; do :; done\n\
             printf ready > {ready}.new\n\
             mv {ready}.new {ready}\n\
             sleep 86400 & wait\n",
            pid = shell_literal(&pid_file),
            socket = shell_literal(&socket_file),
            ready = shell_literal(&client_ready),
        ),
    );

    let startup = tokio::spawn(
        TestServer::builder()
            .tmux_executable(&executable)
            .lifecycle_timeout(Duration::from_millis(500))
            .start(),
    );
    wait_for_file(&client_ready).await;
    let daemon_pid = wait_for_file(&pid_file)
        .await
        .parse::<u32>()
        .expect("fake daemon publishes one PID");
    let socket = std::path::PathBuf::from(wait_for_file(&socket_file).await);
    let directory = socket
        .parent()
        .expect("fake socket has an owned directory")
        .to_path_buf();
    let config = directory.join("c");
    let lock = directory.join("s.lock");
    let mut helper = ProcessCommand::new("sh");
    helper
        .arg("-c")
        .arg("exec sleep 86400")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(i32::try_from(daemon_pid).expect("daemon PID fits i32"));
    let mut helper = UnrelatedMarkerProcess {
        child: helper
            .spawn()
            .expect("unmarked helper joins the daemon group"),
    };
    assert_eq!(
        getpgid(Some(pid(helper.child.id()))).expect("helper has a process group"),
        pid(daemon_pid),
    );

    kill_process(pid(daemon_pid), Signal::KILL).expect("external owner kills the daemon");
    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    loop {
        match waitpid(Some(pid(daemon_pid)), WaitOptions::NOHANG) {
            Ok(Some((_pid, _status))) => break,
            Ok(None) | Err(Errno::INTR) if Instant::now() < deadline => {
                std::thread::yield_now();
            }
            outcome => panic!("external owner could not reap the daemon: {outcome:?}"),
        }
    }
    let result = startup.await.expect("startup task completes");
    let kind = result.as_ref().err().map(TestServerError::kind);
    drop(result);
    let helper_survived = matches!(helper.child.try_wait(), Ok(None));
    let root_retained = directory.exists();
    let config_retained = config.exists();

    drop(helper);
    for entry in [&socket, &config, &lock] {
        if entry.exists() {
            fs::remove_file(entry).expect("test removes a retained fixed entry");
        }
    }
    fs::remove_dir_all(&directory).expect("test removes the retained root");

    assert_eq!(kind, Some(TestServerErrorKind::ShutdownFailed));
    assert!(
        helper_survived,
        "lost child ownership cannot signal the retired process group"
    );
    assert!(root_retained, "lost child ownership retains the root");
    assert!(config_retained, "lost child ownership retains the config");
}

#[tokio::test]
async fn aborting_shutdown_leaves_an_unabortable_waiter_owning_cleanup() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let pid_file = fake_root.path().join("pid");
    let term_marker = fake_root.path().join("term");
    write_executable(
        &executable,
        &fake_tmux_script(
            &pid_file,
            None,
            Some(&term_marker),
            "",
            "sleep 86400 & wait",
        ),
    );
    let guard = TestServer::builder()
        .tmux_executable(&executable)
        .lifecycle_timeout(Duration::from_secs(2))
        .start()
        .await
        .expect("PID-emulating fixture starts");
    let daemon_pid = guard.daemon_pid();
    let socket = guard.socket_path().to_path_buf();

    let mut shutdown = tokio::spawn(guard.shutdown());
    tokio::select! {
        _ = wait_for_file(&term_marker) => {}
        result = &mut shutdown => panic!("shutdown ended before cancellation: {result:?}"),
    }
    shutdown.abort();
    assert!(
        shutdown
            .await
            .expect_err("shutdown task is aborted")
            .is_cancelled()
    );

    assert_process_gone(daemon_pid).await;
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        while socket.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached waiter removes the owned socket");
}

#[test]
fn aborting_shutdown_reaps_active_client_before_runtime_teardown() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let daemon_pid_file = fake_root.path().join("daemon-pid");
    let client_pid_file = fake_root.path().join("client-pid");
    write_executable(
        &executable,
        &blocking_exposed_client_tmux_script(&daemon_pid_file, &client_pid_file),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime builds");

    runtime.block_on(async {
        let guard = TestServer::builder()
            .tmux_executable(&executable)
            .lifecycle_timeout(Duration::from_secs(1))
            .start()
            .await
            .expect("fake fixture starts");
        let client = guard.server().clone();
        let mut command = Box::pin(client.cmd(Command::new("libtmux-block-client")));
        let deadline = Instant::now() + OBSERVATION_TIMEOUT;
        std::future::poll_fn(|context| {
            if client_pid_file.exists() {
                return std::task::Poll::Ready(());
            }
            if let std::task::Poll::Ready(result) = command.as_mut().poll(context) {
                panic!("client ended before publishing its PID: {result:?}");
            }
            assert!(
                Instant::now() < deadline,
                "active client publishes its PID before the observation deadline"
            );
            context.waker().wake_by_ref();
            std::task::Poll::Pending
        })
        .await;

        let mut shutdown = Box::pin(guard.shutdown());
        std::future::poll_fn(|context| match shutdown.as_mut().poll(context) {
            std::task::Poll::Pending => std::task::Poll::Ready(()),
            std::task::Poll::Ready(result) => {
                panic!("shutdown completed before cancellation: {result:?}")
            }
        })
        .await;
        drop(shutdown);
        drop(command);
    });
    drop(runtime);
    let client_pid = fs::read_to_string(&client_pid_file)
        .expect("active client publishes its PID")
        .parse::<u32>()
        .expect("active client PID is numeric");

    assert_child_already_reaped(client_pid);
}

#[tokio::test]
async fn forced_drop_kills_a_same_group_helper_descendant() {
    let guard = TestServer::new().await.expect("real tmux starts");
    let daemon_pid = guard.daemon_pid();
    let mut helper = ProcessCommand::new("sh");
    helper
        .arg("-c")
        .arg("trap '' TERM; while :; do sleep 1; done")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(i32::try_from(daemon_pid).expect("daemon PID fits i32"));
    let mut helper = helper.spawn().expect("same-group helper starts");
    let helper_pid = helper.id();
    assert_eq!(
        getpgid(Some(pid(helper_pid))).expect("helper has a process group"),
        pid(daemon_pid),
    );

    drop(guard);

    assert_process_gone(daemon_pid).await;
    let status = wait_for_child_exit(&mut helper).await;
    assert!(!status.success());
    assert_process_gone(helper_pid).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn forced_drop_contains_a_setsid_real_pane_before_removing_its_root() {
    let mut unrelated = UnrelatedMarkerProcess::spawn();
    let marker_root = tempfile::tempdir().expect("pane marker directory");
    let pane_pid_file = marker_root.path().join("pane-pid");
    let guard = TestServer::new().await.expect("real tmux starts");
    let directory = guard
        .socket_path()
        .parent()
        .expect("socket has an owned directory")
        .to_path_buf();
    let pane_pid = start_detached_real_pane(&guard, &pane_pid_file).await;
    let _pane_cleanup = PublishedProcess::new(pane_pid);
    assert_ne!(
        getpgid(Some(pid(pane_pid))).expect("pane has a process group"),
        pid(guard.daemon_pid()),
    );

    drop(guard);

    assert_process_gone(pane_pid).await;
    assert!(!directory.exists(), "root is removed after containment");
    unrelated.assert_alive();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn shutdown_contains_a_setsid_real_pane_before_removing_its_root() {
    let mut unrelated = UnrelatedMarkerProcess::spawn();
    let marker_root = tempfile::tempdir().expect("pane marker directory");
    let pane_pid_file = marker_root.path().join("pane-pid");
    let guard = TestServer::new().await.expect("real tmux starts");
    let directory = guard
        .socket_path()
        .parent()
        .expect("socket has an owned directory")
        .to_path_buf();
    let pane_pid = start_detached_real_pane(&guard, &pane_pid_file).await;
    let _pane_cleanup = PublishedProcess::new(pane_pid);

    guard.shutdown().await.expect("fixture shuts down");

    assert_process_gone(pane_pid).await;
    assert!(!directory.exists(), "root is removed after containment");
    unrelated.assert_alive();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn startup_failure_contains_a_reparented_double_fork_before_root_cleanup() {
    let mut unrelated = UnrelatedMarkerProcess::spawn();
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let descendant_pid_file = fake_root.path().join("descendant-pid");
    let release_file = fake_root.path().join("release-daemon");
    let socket_file = fake_root.path().join("socket");
    let descendant_body = format!(
        "trap '' HUP INT TERM; printf '%s' \"$$\" > {}; while :; do sleep 1; done",
        shell_literal(&descendant_pid_file),
    );
    let detached_command = format!(
        "setsid sh -c {} </dev/null >/dev/null 2>&1 &",
        shell_text_literal(&descendant_body),
    );
    write_executable(
        &executable,
        &format!(
            "#!/bin/sh\n\
             daemon=0\n\
             socket=''\n\
             previous=''\n\
             for argument in \"$@\"; do\n\
               if [ \"$argument\" = '-D' ]; then daemon=1; fi\n\
               if [ \"$previous\" = '-S' ]; then socket=$argument; fi\n\
               previous=$argument\n\
             done\n\
             if [ \"$daemon\" = 1 ]; then\n\
               sh -c {detached} &\n\
               while [ ! -s {pid} ]; do :; done\n\
               while [ ! -e {release} ]; do :; done\n\
               printf '%s' \"$socket\" > {socket}.new\n\
               mv {socket}.new {socket}\n\
               : > \"$socket\"\n\
               exit 23\n\
             fi\n\
             exit 1\n",
            detached = shell_text_literal(&detached_command),
            pid = shell_literal(&descendant_pid_file),
            release = shell_literal(&release_file),
            socket = shell_literal(&socket_file),
        ),
    );

    let startup = tokio::spawn(
        TestServer::builder()
            .tmux_executable(&executable)
            .lifecycle_timeout(Duration::from_secs(5))
            .start(),
    );
    let descendant_pid = wait_for_file(&descendant_pid_file)
        .await
        .parse::<u32>()
        .expect("detached descendant PID is numeric");
    let _descendant_cleanup = PublishedProcess::new(descendant_pid);
    fs::write(&release_file, b"release").expect("test releases the foreground daemon");
    let error = startup
        .await
        .expect("startup task completes")
        .expect_err("exited foreground daemon is rejected");
    let socket = std::path::PathBuf::from(wait_for_file(&socket_file).await);
    let directory = socket
        .parent()
        .expect("fake socket has an owned directory")
        .to_path_buf();

    assert_eq!(error.kind(), TestServerErrorKind::DaemonExited);
    assert_process_gone(descendant_pid).await;
    assert!(!directory.exists(), "root is removed after containment");
    unrelated.assert_alive();
}

#[tokio::test]
async fn graceful_shutdown_signals_only_the_retained_pid_before_forced_group_cleanup() {
    let fake_root = tempfile::tempdir().expect("fake executable directory");
    let executable = fake_root.path().join("tmux");
    let pid_file = fake_root.path().join("pid");
    let daemon_term = fake_root.path().join("daemon-term");
    write_executable(
        &executable,
        &fake_tmux_script(
            &pid_file,
            None,
            Some(&daemon_term),
            "",
            "sleep 86400 & wait",
        ),
    );
    let guard = TestServer::builder()
        .tmux_executable(&executable)
        .lifecycle_timeout(Duration::from_millis(500))
        .start()
        .await
        .expect("PID-emulating fixture starts");
    let daemon_pid = guard.daemon_pid();
    let markers = tempfile::tempdir().expect("marker directory");
    let ready = markers.path().join("ready");
    let term = markers.path().join("term");
    let helper_script = format!(
        "trap 'printf term > {term}.new; mv {term}.new {term}' TERM; \
         printf ready > {ready}.new; mv {ready}.new {ready}; \
         sleep 86400 & wait",
        ready = shell_literal(&ready),
        term = shell_literal(&term),
    );
    let mut helper = ProcessCommand::new("sh");
    helper
        .arg("-c")
        .arg(helper_script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(i32::try_from(daemon_pid).expect("daemon PID fits i32"));
    let mut helper = helper.spawn().expect("same-group helper starts");
    let helper_pid = helper.id();
    wait_for_file(&ready).await;

    guard.shutdown().await.expect("fixture shuts down");

    let status = wait_for_child_exit(&mut helper).await;
    assert!(!status.success());
    assert_eq!(
        fs::read(&daemon_term).expect("daemon receives TERM"),
        b"term"
    );
    assert!(
        !term.exists(),
        "graceful phase must not signal the process group"
    );
    assert_process_gone(daemon_pid).await;
    assert_process_gone(helper_pid).await;
}

#[tokio::test]
async fn graceful_shutdown_exits_an_ordinary_real_pane() {
    let guard = TestServer::new().await.expect("real tmux starts");
    let session = guard
        .server()
        .cmd(
            Command::new("new-session")
                .arg("-d")
                .arg("-s")
                .arg("ordinary-pane")
                .arg("sleep")
                .arg("120"),
        )
        .await
        .expect("session creation executes");
    assert!(session.success());
    let pane = guard
        .server()
        .cmd(
            Command::new("display-message")
                .arg("-p")
                .arg("-t")
                .arg("ordinary-pane:0.0")
                .arg("#{pane_pid}"),
        )
        .await
        .expect("pane PID query executes");
    assert!(pane.success());
    let pane_pid = pane
        .stdout_utf8()
        .expect("pane PID is UTF-8")
        .trim()
        .parse::<u32>()
        .expect("pane PID is numeric");

    guard.shutdown().await.expect("fixture shuts down");

    assert_process_gone(pane_pid).await;
}

#[tokio::test]
async fn renamed_parent_and_symlink_substitution_cannot_redirect_cleanup() {
    let guard = TestServer::new().await.expect("owned tmux starts");
    let outside = TestServer::new().await.expect("outside tmux starts");
    let owned_pid = guard.daemon_pid();
    let outside_pid = outside.daemon_pid();
    let original_directory = guard
        .socket_path()
        .parent()
        .expect("owned socket has a directory")
        .to_path_buf();
    let renamed_directory = original_directory.with_extension("renamed");
    let owned_socket_name = guard
        .socket_path()
        .file_name()
        .expect("owned socket has a basename")
        .to_owned();
    let owned_config_name = guard
        .server()
        .config_file()
        .expect("owned config is exposed through Server")
        .file_name()
        .expect("owned config has a basename")
        .to_owned();
    let outside_directory = outside
        .socket_path()
        .parent()
        .expect("outside socket has a directory");
    let outside_config = outside
        .server()
        .config_file()
        .expect("outside config is exposed through Server")
        .to_path_buf();
    assert_eq!(
        outside.socket_path().file_name(),
        Some(owned_socket_name.as_os_str())
    );
    assert_eq!(
        outside_config.file_name(),
        Some(owned_config_name.as_os_str())
    );

    fs::rename(&original_directory, &renamed_directory).expect("owned directory is renamed");
    symlink(outside_directory, &original_directory).expect("old name is replaced by a symlink");
    let sentinel = renamed_directory.join("caller-owned-sentinel");
    fs::write(&sentinel, b"preserve").expect("sentinel is created in the owned directory");

    drop(guard);

    assert_process_gone(owned_pid).await;
    assert!(!renamed_directory.join(owned_socket_name).exists());
    assert!(!renamed_directory.join(owned_config_name).exists());
    assert_eq!(fs::read(&sentinel).expect("sentinel remains"), b"preserve");
    assert!(
        fs::symlink_metadata(&original_directory)
            .expect("substitution remains")
            .file_type()
            .is_symlink()
    );
    let outside_reported = outside
        .server()
        .cmd(Command::new("display-message").arg("-p").arg("#{pid}"))
        .await
        .expect("outside daemon remains reachable");
    assert!(outside_reported.success());
    assert_eq!(
        outside_reported
            .stdout_utf8()
            .expect("outside PID is UTF-8")
            .trim(),
        outside_pid.to_string(),
    );
    assert!(outside_config.exists());

    fs::remove_file(&original_directory).expect("test removes its substitution");
    fs::remove_file(&sentinel).expect("test removes its sentinel");
    fs::remove_dir(&renamed_directory).expect("test removes renamed directory");
    outside
        .shutdown()
        .await
        .expect("outside fixture shuts down");
}

#[tokio::test]
async fn consuming_shutdown_refuses_parent_symlink_substitution() {
    let guard = TestServer::new().await.expect("owned tmux starts");
    let outside = TestServer::new().await.expect("outside tmux starts");
    let owned_pid = guard.daemon_pid();
    let outside_pid = outside.daemon_pid();
    let original_directory = guard
        .socket_path()
        .parent()
        .expect("owned socket has a directory")
        .to_path_buf();
    let renamed_directory = original_directory.with_extension("SENTINEL-renamed-shutdown");
    let owned_socket_name = guard
        .socket_path()
        .file_name()
        .expect("owned socket has a basename")
        .to_owned();
    let owned_config_name = guard
        .server()
        .config_file()
        .expect("owned config is exposed")
        .file_name()
        .expect("owned config has a basename")
        .to_owned();
    let outside_directory = outside
        .socket_path()
        .parent()
        .expect("outside socket has a directory");
    let outside_config = outside
        .server()
        .config_file()
        .expect("outside config is exposed")
        .to_path_buf();
    let outside_config_before = fs::read(&outside_config).expect("outside config exists");
    assert_eq!(
        outside.socket_path().file_name(),
        Some(owned_socket_name.as_os_str())
    );
    assert_eq!(
        outside_config.file_name(),
        Some(owned_config_name.as_os_str())
    );

    fs::rename(&original_directory, &renamed_directory).expect("owned directory is renamed");
    symlink(outside_directory, &original_directory).expect("old name is replaced by a symlink");

    let error = guard
        .shutdown()
        .await
        .expect_err("substituted parent entry is refused");

    assert_eq!(error.kind(), TestServerErrorKind::CleanupFailed);
    assert!(StdError::source(&error).is_none());
    assert!(!error.to_string().contains("SENTINEL"));
    assert!(!format!("{error:?}").contains("SENTINEL"));
    assert_process_gone(owned_pid).await;
    assert!(!renamed_directory.join(owned_socket_name).exists());
    assert!(!renamed_directory.join(owned_config_name).exists());
    assert!(
        fs::symlink_metadata(&original_directory)
            .expect("substitution remains")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read(&outside_config).expect("outside config remains"),
        outside_config_before,
    );
    let outside_reported = outside
        .server()
        .cmd(Command::new("display-message").arg("-p").arg("#{pid}"))
        .await
        .expect("outside daemon remains reachable");
    assert!(outside_reported.success());
    assert_eq!(
        outside_reported
            .stdout_utf8()
            .expect("outside PID is UTF-8")
            .trim(),
        outside_pid.to_string(),
    );

    fs::remove_file(&original_directory).expect("test removes its substitution");
    fs::remove_dir(&renamed_directory).expect("test removes renamed directory");
    outside
        .shutdown()
        .await
        .expect("outside fixture shuts down");
}

#[tokio::test]
async fn every_exposed_server_command_refuses_to_bootstrap_a_replacement() {
    let guard = TestServer::new().await.expect("real tmux starts");
    let daemon_pid = guard.daemon_pid();
    let socket = guard.socket_path().to_path_buf();
    let escaped_server = guard.server().clone();

    kill_process(pid(daemon_pid), Signal::TERM).expect("retained daemon receives SIGTERM");
    tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        loop {
            let probe = escaped_server
                .cmd(Command::new("display-message").arg("-p").arg("#{pid}"))
                .await
                .expect("no-start probe process executes");
            if !probe.success() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("no-start probe observes daemon exit");

    let start_capable = escaped_server
        .cmd(Command::new("new-session").arg("-d"))
        .await
        .expect("start-capable client process executes");
    assert!(!start_capable.success());

    let independent = ProcessCommand::new("tmux")
        .arg("-N")
        .arg("-S")
        .arg(&socket)
        .arg("display-message")
        .arg("-p")
        .arg("#{pid}")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("independent no-start client executes");
    assert!(!independent.status.success());
    assert!(independent.stdout.is_empty());

    guard.shutdown().await.expect("exited fixture is reaped");
    assert_process_gone(daemon_pid).await;
    assert!(!socket.exists());
}

#[tokio::test]
async fn consuming_shutdown_reports_descriptor_cleanup_failure_after_reaping() {
    let guard = TestServer::new().await.expect("real tmux starts");
    let daemon_pid = guard.daemon_pid();
    let socket = guard.socket_path().to_path_buf();
    let config = guard
        .server()
        .config_file()
        .expect("test config is exposed")
        .to_path_buf();
    let directory = socket
        .parent()
        .expect("test socket has a directory")
        .to_path_buf();
    let sentinel = directory.join("caller-owned-SENTINEL");
    fs::write(&sentinel, b"preserve").expect("caller sentinel is created");

    let error = guard
        .shutdown()
        .await
        .expect_err("unknown directory entry prevents final removal");

    assert_eq!(error.kind(), TestServerErrorKind::CleanupFailed);
    assert!(StdError::source(&error).is_none());
    assert!(!error.to_string().contains("SENTINEL"));
    assert!(!format!("{error:?}").contains("SENTINEL"));
    assert_process_gone(daemon_pid).await;
    assert!(!socket.exists());
    assert!(!config.exists());
    assert_eq!(fs::read(&sentinel).expect("sentinel remains"), b"preserve");

    fs::remove_file(&sentinel).expect("test removes its sentinel");
    fs::remove_dir(&directory).expect("test removes retained directory");
}

#[test]
fn caller_umask_is_overridden_to_exact_mode_0700() {
    if std::env::var_os(UMASK_CHILD).is_some() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("child runtime builds");
        runtime.block_on(async {
            let guard = TestServer::new().await.expect("real tmux starts");
            let directory = guard
                .socket_path()
                .parent()
                .expect("socket has an owned directory");
            let config = guard
                .server()
                .config_file()
                .expect("test server owns an empty config");
            let mode = fs::metadata(directory)
                .expect("owned directory exists")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700);
            let config_mode = fs::metadata(config)
                .expect("owned config exists")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(config_mode, 0o600);
            guard.shutdown().await.expect("fixture shuts down");
        });
        return;
    }

    let executable = std::env::current_exe().expect("test executable path");
    let status = ProcessCommand::new("sh")
        .arg("-c")
        .arg(
            "umask 777; exec \"$1\" --exact caller_umask_is_overridden_to_exact_mode_0700 --nocapture",
        )
        .arg("sh")
        .arg(executable)
        .env(UMASK_CHILD, "1")
        .status()
        .expect("restrictive-umask child starts");
    assert!(status.success());
}

#[test]
fn startup_removes_tmux_context_without_replacing_home() {
    if std::env::var_os(ENVIRONMENT_CHILD).is_some() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("child runtime builds");
        runtime.block_on(async {
            let guard = TestServer::new().await.expect("real tmux starts");
            for key in ["TMUX", "TMUX_PANE"] {
                let result = guard
                    .server()
                    .cmd(Command::new("show-environment").arg("-g").arg(key))
                    .await
                    .expect("environment query executes");
                assert!(!result.success(), "{key} must not reach the daemon");
            }
            let home = guard
                .server()
                .cmd(Command::new("show-environment").arg("-g").arg("HOME"))
                .await
                .expect("HOME query executes");
            assert!(home.success());
            assert_eq!(
                home.stdout_utf8().expect("HOME is UTF-8").trim(),
                "HOME=/tmp/libtmux-SENTINEL-home",
            );
            let term = guard
                .server()
                .cmd(Command::new("show-environment").arg("-g").arg("TERM"))
                .await
                .expect("TERM query executes");
            assert!(term.success());
            assert_eq!(
                term.stdout_utf8().expect("TERM is UTF-8").trim(),
                "TERM=xterm-256color",
            );
            guard.shutdown().await.expect("fixture shuts down");
        });
        return;
    }

    let executable = std::env::current_exe().expect("test executable path");
    let status = ProcessCommand::new(executable)
        .arg("--exact")
        .arg("startup_removes_tmux_context_without_replacing_home")
        .arg("--nocapture")
        .env(ENVIRONMENT_CHILD, "1")
        .env("TMUX", "/tmp/libtmux-SENTINEL-outside,111,0")
        .env("TMUX_PANE", "%999")
        .env("HOME", "/tmp/libtmux-SENTINEL-home")
        .env("TERM", "libtmux-SENTINEL-term")
        .status()
        .expect("isolated-environment child starts");
    assert!(status.success());
}

/// A fixture pane runs a fixed shell rather than whoever's `$SHELL` is set.
///
/// This is the property that keeps the suite's timing its own. When the
/// fixture left `default-shell` unset, tmux fell back to `$SHELL`, so every
/// pane sourced the developer's interactive startup files: measured on this
/// machine an interactive `zsh` reached a drawn prompt in ~1 s idle and 9.7 s
/// while other tmux servers were starting shells, against ~10 ms for the
/// pinned shell. Tests that wait for a pane to be ready then fail on a
/// machine that is merely busy, which reads as flakiness and is not.
#[tokio::test]
async fn a_fixture_pane_runs_a_fixed_shell_rather_than_the_callers() {
    let guard = TestServer::new().await.expect("real tmux starts");
    let server = guard.server();

    server
        .cmd(
            Command::new("new-session")
                .arg("-d")
                .arg("-s")
                .arg("hermetic"),
        )
        .await
        .expect("session is created");

    for option in ["default-shell", "default-command"] {
        let value = server
            .cmd(Command::new("show-options").arg("-gv").arg(option))
            .await
            .expect("tmux reports the option");
        assert_eq!(
            value.stdout_lossy().trim(),
            "/bin/sh",
            "{option} is pinned, so a pane never inherits the caller's shell",
        );
    }

    let running = server
        .cmd(
            Command::new("display-message")
                .arg("-p")
                .arg("-t")
                .arg("hermetic")
                .arg("#{pane_current_command}"),
        )
        .await
        .expect("tmux reports the pane");

    // The options above are the proof that the shell is pinned. This is the
    // runtime confirmation, and what tmux reports is whatever `/bin/sh`
    // actually is: `sh` where that is dash, `bash` on macOS, where `/bin/sh`
    // is bash in POSIX mode. What matters is that it is a plain POSIX shell
    // rather than the caller's `$SHELL`, which is what the fixture exists to
    // prevent.
    let reported = running.stdout_lossy().trim().to_owned();
    assert!(
        matches!(reported.as_str(), "sh" | "bash" | "dash"),
        "the pane runs whatever /bin/sh is here, not the caller's shell: {reported}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_sweep_spares_a_fixture_whose_owner_is_still_running() {
    let live = TestServer::new().await.expect("real tmux starts");
    let live_socket = live.socket_path().to_path_buf();

    // Something that merely looks like a fixture: right prefix, no socket.
    let root = Path::new("/tmp/libtmux-rs-test");
    fs::create_dir_all(root).expect("fixture root");
    let decoy = root.join(format!("decoy-{}", process::id()));
    fs::create_dir(&decoy).expect("decoy directory");
    fs::set_permissions(&decoy, fs::Permissions::from_mode(0o700)).expect("decoy mode");

    // A directory old enough to reap but not named like a fixture.
    let unrelated = root.join(format!("unrelated-{}", process::id()));
    fs::create_dir(&unrelated).expect("unrelated directory");

    // No minimum age at all, so nothing is spared by being young. The live
    // fixture survives because the process that made it is this one, which is
    // the whole point of recording an owner rather than reading a timestamp.
    let reaped = libtmux::test::reap_abandoned_servers(Duration::ZERO)
        .expect("a sweep reads the temporary root");

    assert!(
        live_socket.exists(),
        "a fixture whose owner is running is never reaped, however old",
    );
    assert!(
        !reaped.iter().any(|path| path == &decoy),
        "a directory without a socket is not a fixture",
    );
    assert!(
        !reaped.iter().any(|path| path == &unrelated),
        "a directory without the fixture name is never considered",
    );
    assert!(decoy.exists() && unrelated.exists(), "neither was removed");

    fs::remove_dir_all(&decoy).ok();
    fs::remove_dir_all(&unrelated).ok();
    live.shutdown().await.expect("the fixture shuts down");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_sweep_reaps_a_fixture_whose_owner_is_gone() {
    // An owner that is genuinely gone: a process run to completion and
    // reaped, so its id names nothing. Inventing a large number instead would
    // be a guess that a busy machine could falsify.
    let mut owner = ProcessCommand::new("true")
        .spawn()
        .expect("a short process");
    let owner_pid = owner.id();
    owner.wait().expect("it exits");

    let root = Path::new("/tmp/libtmux-rs-test");
    fs::create_dir_all(root).expect("fixture root");
    let abandoned = root.join(format!("abandoned-{}", process::id()));
    fs::create_dir(&abandoned).expect("abandoned directory");
    fs::set_permissions(&abandoned, fs::Permissions::from_mode(0o700)).expect("mode");
    fs::write(abandoned.join("owner"), owner_pid.to_string()).expect("owner record");
    fs::write(abandoned.join("s"), b"").expect("socket stand-in");

    libtmux::test::reap_abandoned_servers(Duration::ZERO)
        .expect("a sweep reads the temporary root");

    // Asserted on the directory rather than on this sweep's return value: a
    // sweep is server-wide, so a sweep in another test running beside this one
    // may be the one that reaps it. Either way it is gone, which is the
    // property under test.
    assert!(
        !abandoned.exists(),
        "a fixture whose owner is gone does not survive a sweep",
    );
}

/// A replacement daemon on the same socket is a different server, and saying
/// so is the only thing standing between a stale handle and the wrong object.
///
/// tmux makes this easy to get wrong: the socket file survives `kill-server`,
/// a replacement binds the same path, and ids restart from zero, so `%0`
/// resolves in both daemons and names different panes.
///
/// This runs two daemons in turn on one socket, so it owns the directory
/// rather than using `TestServer`, whose shutdown takes its fixture directory
/// with it and would leave the second daemon nowhere to bind.
#[tokio::test]
async fn a_replacement_daemon_on_the_same_socket_is_a_different_generation() {
    let root = Path::new("/tmp/libtmux-rs-test").join("generation-reuse");
    fs::create_dir_all(&root).expect("the fixture root is writable");
    let socket = root.join("s");
    let _ = fs::remove_file(&socket);

    let server = libtmux::Server::builder()
        .socket_path(&socket)
        .build()
        .expect("a server on this socket");

    let outcome = async {
        server.new_session("first").await?;
        let first = server.generation().await?;

        // Same daemon: the check is a no-op.
        server.require_generation(first).await?;
        let first_pane = server
            .panes()
            .await?
            .first()
            .map(|pane| pane.id().to_string());

        server.kill().await?;
        retry_until(Duration::from_secs(5), async || !server.is_alive().await)
            .await
            .expect("the first daemon goes away");

        server.new_session("second").await?;
        let second = server.generation().await?;
        let second_pane = server
            .panes()
            .await?
            .first()
            .map(|pane| pane.id().to_string());

        Ok::<_, libtmux::Error>((
            first,
            second,
            first_pane,
            second_pane,
            server.require_generation(first).await,
        ))
    }
    .await;

    let _ = server.kill().await;
    let _ = fs::remove_dir_all(&root);

    let (first, second, first_pane, second_pane, checked) =
        outcome.expect("both daemons run on the same socket");

    // The hazard this guards: the id is identical across the replacement, so
    // an id alone cannot tell a caller it is addressing a different object.
    assert_eq!(
        first_pane, second_pane,
        "both daemons issue the same first pane id, which is why the id is not enough",
    );
    assert_ne!(
        first, second,
        "a replacement daemon is a different generation"
    );

    let error = checked.expect_err("the replacement is not the captured daemon");
    assert!(
        matches!(&error, libtmux::Error::ServerGenerationChanged { expected, found }
            if *expected == first && *found == second),
        "the error names both daemons, got {error:?}",
    );

    // The decision it reduces to is the one a caller already writes for a
    // handle that has gone stale: look it up again. Reported as a refusal it
    // said the opposite, that the request was wrong and re-listing would not
    // help, which is the one thing that does.
    assert_eq!(
        error.kind(),
        libtmux::ErrorKind::ObjectGone,
        "a replaced daemon is every captured handle gone, got {error:?}",
    );
    assert!(error.is_object_gone(), "{error}");
}
