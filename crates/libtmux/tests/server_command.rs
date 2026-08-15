//! Public Server, capability, command-result, and lifecycle contracts.

// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::hash_map::DefaultHasher;
use std::error::Error as StdError;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Write as _;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{Duration, Instant};

use libtmux::{
    Command, CommandResult, EngineCapabilities, Error, Server, ServerBuilder,
    ServerConfigurationErrorKind, ServerIdentity,
};
use rustix::io::Errno;
use rustix::process::{Pid, test_kill_process};
use static_assertions::{assert_impl_all, assert_not_impl_any};

assert_impl_all!(Server: Clone, std::fmt::Debug, Eq, Hash, Send, Sync);
assert_impl_all!(ServerBuilder: Send, Sync);
assert_impl_all!(ServerIdentity: Send, Sync);
assert_impl_all!(Command: Send, Sync);
assert_impl_all!(CommandResult: std::fmt::Debug, Send, Sync);
assert_not_impl_any!(CommandResult: std::fmt::Display);
assert_impl_all!(EngineCapabilities: Send, Sync);
assert_impl_all!(ServerConfigurationErrorKind: Clone, Copy, std::fmt::Debug, Eq, Send, Sync);

fn hash(value: &Server) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn write_script(directory: &Path, name: &str, body: &str) -> PathBuf {
    let path = directory.join(name);
    let staging = directory.join(format!(".{name}.{}.tmp", process::id()));
    let mut file = fs::File::create(&staging).expect("staged script is creatable");
    file.write_all(
        format!(
            "#!/bin/sh\nif [ \"${{1-}}\" = __libtmux_fixture_ready__ ]; then\n    exit 0\nfi\nset -eu\n{body}\n"
        )
        .as_bytes(),
    )
    .expect("script is writable");
    file.sync_all().expect("script contents are durable");
    drop(file);
    let mut permissions = fs::metadata(&staging)
        .expect("script metadata is readable")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&staging, permissions).expect("staged script is executable");
    fs::rename(&staging, &path).expect("staged script is installed atomically");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match process::Command::new(&path)
            .arg("__libtmux_fixture_ready__")
            .status()
        {
            Ok(status) => {
                assert!(
                    status.success(),
                    "script readiness probe exited with {status}"
                );
                break;
            }
            Err(source) => {
                assert_eq!(
                    source.raw_os_error(),
                    Some(Errno::TXTBSY.raw_os_error()),
                    "script readiness probe failed"
                );
                assert!(
                    Instant::now() < deadline,
                    "script remained busy past the readiness deadline"
                );
                std::thread::yield_now();
            }
        }
    }
    path
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn fake_server(executable: &Path, socket: &Path) -> Server {
    Server::builder()
        .tmux_executable(executable)
        .socket_path(socket)
        .build()
        .expect("fake server configuration is valid")
}

fn pid(value: u32) -> Pid {
    Pid::from_raw(i32::try_from(value).expect("test PID fits i32")).expect("test PID is nonzero")
}

async fn read_pids(
    path: &Path,
    count: usize,
    dispatch: &mut tokio::task::JoinHandle<Result<CommandResult, Error>>,
) -> Vec<u32> {
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(value) = fs::read_to_string(path) {
                let pids = value
                    .lines()
                    .filter_map(|line| line.parse::<u32>().ok())
                    .collect::<Vec<_>>();
                if pids.len() == count {
                    return Ok(pids);
                }
            }
            tokio::select! {
                result = &mut *dispatch => return Err(result),
                () = tokio::task::yield_now() => {}
            }
        }
    })
    .await
    .expect("child publishes its PIDs before the readiness deadline");
    outcome.expect("dispatch remains active until its child publishes readiness")
}

async fn read_pid(
    path: &Path,
    dispatch: &mut tokio::task::JoinHandle<Result<CommandResult, Error>>,
) -> u32 {
    read_pids(path, 1, dispatch).await[0]
}

async fn assert_process_gone(value: u32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(test_kill_process(pid(value)), Err(Errno::SRCH)) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("child is gone after terminal cleanup");
}

fn assert_configuration_error(
    error: &Error,
    expected: ServerConfigurationErrorKind,
    secrets: &[&str],
) {
    assert!(matches!(
        error,
        Error::InvalidServerConfiguration { kind, .. } if *kind == expected
    ));
    assert!(StdError::source(error).is_none());
    for diagnostic in [error.to_string(), format!("{error:?}")] {
        for secret in secrets {
            assert!(
                !diagnostic.contains(secret),
                "leaked {secret:?} in {diagnostic:?}"
            );
        }
    }
}

fn assert_error_path_safe(error: &Error, secret: &str) {
    let mut source: Option<&dyn StdError> = Some(error);
    while let Some(current) = source {
        assert!(!current.to_string().contains(secret));
        assert!(!format!("{current:?}").contains(secret));
        source = current.source();
    }
}

struct RealTmuxGuard {
    socket: PathBuf,
}

impl Drop for RealTmuxGuard {
    fn drop(&mut self) {
        drop(
            process::Command::new("tmux")
                .arg("-S")
                .arg(&self.socket)
                .arg("kill-server")
                .stdout(process::Stdio::null())
                .stderr(process::Stdio::null())
                .status(),
        );
    }
}

#[test]
fn construction_exposes_immutable_configuration_and_structural_identity() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("server.sock");
    let config = directory.path().join("tmux.conf");
    let timeout = Duration::from_secs(7);

    let left = Server::builder()
        .socket_path(&socket)
        .config_file(&config)
        .colors(256)
        .tmux_executable("tmux")
        .default_timeout(timeout)
        .build()
        .expect("valid server configuration");
    let right = Server::builder()
        .socket_path(&socket)
        .default_timeout(Duration::from_secs(1))
        .build()
        .expect("same endpoint is valid");

    assert_eq!(left, right);
    assert_eq!(hash(&left), hash(&right));
    assert_eq!(left.identity(), right.identity());
    assert_eq!(left.socket_path(), socket);
    assert_eq!(left.socket_name(), None);
    assert_eq!(left.config_file(), Some(config.as_path()));
    assert_eq!(left.colors(), Some(256));
    assert_eq!(left.tmux_executable(), OsStr::new("tmux"));
    assert_eq!(left.default_timeout(), timeout);
}

#[test]
fn builder_rejects_conflicting_selectors_and_invalid_colors_before_execution() {
    let conflict = Server::builder()
        .socket_name("sentinel-name")
        .socket_path("/tmp/sentinel-explicit")
        .build()
        .expect_err("selectors conflict");
    assert_configuration_error(
        &conflict,
        ServerConfigurationErrorKind::ConflictingSocketSelectors,
        &["sentinel-name", "sentinel-explicit"],
    );

    let invalid_name = Server::builder()
        .socket_name("../sentinel-name")
        .build()
        .expect_err("socket name is not one component");
    assert_configuration_error(
        &invalid_name,
        ServerConfigurationErrorKind::InvalidSocketName,
        &["sentinel-name"],
    );

    let invalid_path = Server::builder()
        .socket_path(PathBuf::from(OsString::from_vec(
            b"/tmp/sentinel-path\0tail".to_vec(),
        )))
        .build()
        .expect_err("socket path contains NUL");
    assert_configuration_error(
        &invalid_path,
        ServerConfigurationErrorKind::InvalidSocketPath,
        &["sentinel-path"],
    );

    let invalid_config = Server::builder()
        .config_file(PathBuf::from(OsString::from_vec(
            b"/tmp/sentinel-config\0tail".to_vec(),
        )))
        .build()
        .expect_err("config path contains NUL");
    assert_configuration_error(
        &invalid_config,
        ServerConfigurationErrorKind::InvalidConfigPath,
        &["sentinel-config"],
    );

    let invalid_colors = Server::builder()
        .colors(16)
        .build()
        .expect_err("only tmux color modes are accepted");
    assert_configuration_error(
        &invalid_colors,
        ServerConfigurationErrorKind::InvalidColorMode,
        &[],
    );
}

#[test]
fn default_named_and_explicit_socket_selection_are_distinct() {
    let default = Server::new().expect("default server configuration");
    let named = Server::builder()
        .socket_name("libtmux-task-seven")
        .build()
        .expect("named server configuration");
    let explicit = Server::builder()
        .socket_path("relative.sock")
        .build()
        .expect("relative explicit path is captured");

    assert_eq!(default.socket_name(), None);
    assert!(default.socket_path().is_absolute());
    assert_eq!(named.socket_name(), Some(OsStr::new("libtmux-task-seven")));
    assert!(explicit.socket_path().is_absolute());
    assert_ne!(named, explicit);
}

#[test]
fn server_debug_redacts_socket_config_and_executable_paths() {
    let server = Server::builder()
        .socket_path("/tmp/sentinel-socket/server")
        .config_file("/tmp/sentinel-config/tmux.conf")
        .tmux_executable("/tmp/sentinel-executable/tmux")
        .build()
        .expect("configuration paths need not exist at construction");
    let diagnostic = format!("{server:?}");

    for secret in ["sentinel-socket", "sentinel-config", "sentinel-executable"] {
        assert!(!diagnostic.contains(secret));
    }
}

#[tokio::test]
async fn public_capability_raw_command_and_shutdown_boundary_is_usable() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let server = Server::builder()
        .socket_path(directory.path().join("absent.sock"))
        .build()
        .expect("an absent explicit socket is valid setup");

    let capabilities = server
        .capabilities()
        .await
        .expect("installed tmux is supported");
    assert!(!capabilities.tmux_version().raw().is_empty());

    let result = server
        .cmd(Command::new("list-sessions"))
        .await
        .expect("ordinary nonzero tmux status remains result data");
    assert_ne!(result.exit_code(), Some(0));
    assert_eq!(result.command().to_string(), r#""list-sessions""#);
    assert!(result.request_id() > 0);

    server.shutdown().await.expect("shutdown succeeds");
    server.shutdown().await.expect("shutdown is idempotent");
}

#[tokio::test]
async fn capability_probe_is_exact_shared_lazy_and_preserves_versions() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let log = directory.path().join("probe.log");
    let script = write_script(
        directory.path(),
        "fake-tmux",
        &format!(
            r#"
if [ "$#" -eq 1 ] && [ "$1" = "-V" ]; then
    printf '<%s>\n' "$1" >> {}
    printf 'renamed-tmux 3.2a\n'
    exit 0
fi
printf 'unexpected command\n' >&2
exit 91
"#,
            shell_quote(&log),
        ),
    );
    let socket = directory.path().join("socket");
    let config = directory.path().join("config");
    let server = Server::builder()
        .tmux_executable(&script)
        .socket_path(&socket)
        .config_file(&config)
        .colors(88)
        .build()
        .expect("server configuration is valid");

    assert!(!log.exists(), "construction is inert");
    let cloned = server.clone();
    let (first, second, third) = tokio::join!(
        server.capabilities(),
        server.capabilities(),
        cloned.capabilities(),
    );
    let first = first.expect("version probe succeeds");
    let second = second.expect("shared probe succeeds");
    let third = third.expect("cloned server shares probe");

    assert_eq!(first.tmux_version().raw(), "3.2a");
    assert!(std::ptr::eq(first, second));
    assert!(std::ptr::eq(first, third));
    assert_eq!(
        fs::read_to_string(&log).expect("probe was recorded"),
        "<-V>\n"
    );
    server.shutdown().await.expect("shutdown succeeds");

    let development = write_script(
        directory.path(),
        "development-tmux",
        "[ \"$#\" -eq 1 ] && [ \"$1\" = \"-V\" ]\nprintf 'tmux master\\n'",
    );
    let development = fake_server(&development, &directory.path().join("development.sock"));
    let capabilities = development
        .capabilities()
        .await
        .expect("master meets the minimum capability floor");
    assert_eq!(capabilities.tmux_version().raw(), "master");
    assert!(capabilities.tmux_version().is_development());
    development.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test]
async fn failed_and_nonzero_capability_probes_are_not_cached() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let attempts = directory.path().join("attempts");
    let script = write_script(
        directory.path(),
        "retry-tmux",
        &format!(
            r#"
if [ "$#" -ne 1 ] || [ "$1" != "-V" ]; then
    exit 92
fi
printf x >> {}
if [ "$(wc -c < {})" -le 2 ]; then
    printf 'tmux 3.7b\n'
    printf 'sentinel-probe-stderr\n' >&2
    exit 9
fi
printf 'tmux 3.7b\n'
"#,
            shell_quote(&attempts),
            shell_quote(&attempts),
        ),
    );
    let server = fake_server(&script, &directory.path().join("retry.sock"));

    let first = server
        .capabilities()
        .await
        .expect_err("parseable stdout with nonzero status is rejected");
    let first_request_id = match &first {
        Error::VersionProbeFailed {
            request_id,
            command,
            exit_code,
            signal,
            ..
        } => {
            assert!(*request_id > 0);
            assert_eq!(command.to_string(), r#""-V""#);
            assert_eq!(*exit_code, Some(9));
            assert_eq!(*signal, None);
            Some(*request_id)
        }
        _ => None,
    }
    .expect("first error is a failed version probe");
    assert!(StdError::source(&first).is_none());
    assert!(!first.to_string().contains("sentinel-probe-stderr"));
    assert!(!format!("{first:?}").contains("sentinel-probe-stderr"));

    let second = server
        .capabilities()
        .await
        .expect_err("a second failed initialization is retried");
    let second_request_id = match &second {
        Error::VersionProbeFailed { request_id, .. } => Some(*request_id),
        _ => None,
    }
    .expect("second error is a failed version probe");
    assert!(second_request_id > 0);
    assert_ne!(first_request_id, second_request_id);
    assert!(StdError::source(&second).is_none());
    assert!(!second.to_string().contains("sentinel-probe-stderr"));
    assert!(!format!("{second:?}").contains("sentinel-probe-stderr"));

    let third = server
        .capabilities()
        .await
        .expect("failed initializations are retried");
    assert_eq!(third.tmux_version().raw(), "3.7b");
    assert_eq!(fs::read(&attempts).expect("attempts are recorded"), b"xxx");
    server.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test]
async fn minimum_version_rejection_does_not_disable_the_raw_escape_hatch() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let marker = directory.path().join("command-ran");
    let script = write_script(
        directory.path(),
        "old-tmux",
        &format!(
            r#"
if [ "$#" -eq 1 ] && [ "$1" = "-V" ]; then
    printf 'tmux 3.2\n'
    exit 0
fi
: > {}
"#,
            shell_quote(&marker),
        ),
    );
    let server = fake_server(&script, &directory.path().join("old.sock"));

    let error = server
        .capabilities()
        .await
        .expect_err("tmux below 3.2a is rejected");
    assert!(matches!(error, Error::UnsupportedTmuxVersion { .. }));
    assert!(
        !marker.exists(),
        "capability probing has no command side effect"
    );

    let result = server
        .cmd(Command::new("display-message"))
        .await
        .expect("raw execution remains available without capability escalation");
    assert!(result.success());
    assert!(
        marker.exists(),
        "raw command reached the configured executable"
    );
    server.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test]
async fn raw_dispatch_preserves_global_argv_and_logical_summary() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("sentinel-socket;");
    let config = directory.path().join("sentinel-config;");
    let script = write_script(
        directory.path(),
        "argv-tmux",
        r#"
if [ "$#" -eq 1 ] && [ "$1" = "-V" ]; then
    printf 'tmux 3.7b\n'
    exit 0
fi
for argument do
    printf '<%s>\n' "$argument"
done
printf '<TMUX=%s>\n' "${TMUX-unset}"
printf '<TMUX_PANE=%s>\n' "${TMUX_PANE-unset}"
"#,
    );
    let server = Server::builder()
        .tmux_executable(&script)
        .socket_path(&socket)
        .config_file(&config)
        .colors(256)
        .build()
        .expect("server configuration is valid");
    let command = Command::new("display-message").arg("value;");
    let result = server.cmd(command).await.expect("fake command succeeds");
    let stdout = result.stdout_utf8().expect("fixture output is UTF-8");

    assert_eq!(
        stdout,
        format!(
            "<-S>\n<{}>\n<-f>\n<{}>\n<-2>\n<display-message>\n<value\\;>\n<TMUX=unset>\n<TMUX_PANE=unset>\n",
            socket.display(),
            config.display(),
        )
    );
    assert_eq!(
        result.command().to_string(),
        r#""display-message" "value;""#
    );
    assert!(!format!("{result:?}").contains("sentinel-socket"));
    assert!(!format!("{result:?}").contains("sentinel-config"));
    server.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test]
async fn logical_nul_inputs_are_classified_without_spawning_the_executable() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let marker = directory.path().join("spawned");
    let script = write_script(
        directory.path(),
        "nul-tmux",
        &format!(": > {}", shell_quote(&marker)),
    );
    let server = fake_server(&script, &directory.path().join("nul.sock"));
    let cases = [
        (
            Command::new(OsString::from_vec(b"sentinel-subcommand\0tail".to_vec())),
            "tmux subcommand",
            "sentinel-subcommand",
        ),
        (
            Command::new("display-message")
                .arg(OsString::from_vec(b"sentinel-argument\0tail".to_vec())),
            "tmux argument",
            "sentinel-argument",
        ),
    ];

    let mut request_ids = Vec::new();
    for (command, expected_input, sentinel) in cases {
        let error = server
            .cmd(command)
            .await
            .expect_err("NUL input is rejected before spawn");
        let request_id = match &error {
            Error::InvalidCommandInput {
                request_id, input, ..
            } => {
                assert_eq!(*input, expected_input);
                Some(*request_id)
            }
            _ => None,
        }
        .expect("NUL input produces an invalid-command error");
        assert!(request_id > 0);
        request_ids.push(request_id);
        let mut source: Option<&dyn StdError> = Some(&error);
        while let Some(current) = source {
            assert!(!current.to_string().contains(sentinel));
            assert!(!format!("{current:?}").contains(sentinel));
            source = current.source();
        }
    }
    assert_ne!(request_ids[0], request_ids[1]);
    assert!(!marker.exists(), "invalid logical input never spawns");
    server.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test]
async fn command_results_expose_exact_status_bytes_and_distinct_request_ids() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let script = write_script(
        directory.path(),
        "result-tmux",
        r#"
if [ "$#" -eq 1 ] && [ "$1" = "-V" ]; then
    printf 'tmux 3.7b\n'
    exit 0
fi
last=''
for argument do last=$argument; done
case "$last" in
    raw)
        printf 'stdout\n\n\377'
        printf 'stderr\n\n\376' >&2
        exit 7
        ;;
    signal)
        kill -TERM $$
        ;;
    secret)
        printf 'sentinel-result-output'
        printf 'sentinel-result-error' >&2
        ;;
    *)
        printf 'ok\n'
        ;;
esac
"#,
    );
    let server = fake_server(&script, &directory.path().join("result.sock"));

    let result = server
        .cmd(Command::new("raw"))
        .await
        .expect("nonzero status remains result data");
    assert!(!result.success());
    assert_eq!(result.exit_code(), Some(7));
    assert_eq!(result.signal(), None);
    assert_eq!(result.stdout(), b"stdout\n\n\xff");
    assert_eq!(result.stderr(), b"stderr\n\n\xfe");
    assert!(result.stdout_utf8().is_err());
    assert!(result.stderr_utf8().is_err());
    assert!(result.stdout_lossy().contains("stdout\n\n"));
    assert!(result.stderr_lossy().contains("stderr\n\n"));

    let signal = server
        .cmd(Command::new("signal"))
        .await
        .expect("signal status remains result data");
    assert!(!signal.success());
    assert_eq!(signal.exit_code(), None);
    assert_eq!(signal.signal(), Some(15));

    let secret = server
        .cmd(Command::new("secret"))
        .await
        .expect("secret fixture succeeds");
    let diagnostic = format!("{secret:?}");
    assert!(!diagnostic.contains("sentinel-result-output"));
    assert!(!diagnostic.contains("sentinel-result-error"));

    let logical = Command::new("ok");
    let sequential = server
        .cmd(logical.clone())
        .await
        .expect("sequential command succeeds");
    let cloned_server = server.clone();
    let (first, second) = tokio::join!(
        server.cmd(logical.clone()),
        cloned_server.cmd(logical.clone()),
    );
    let first = first.expect("first concurrent command succeeds");
    let second = second.expect("second concurrent command succeeds");
    let ids = [
        result.request_id(),
        signal.request_id(),
        sequential.request_id(),
        first.request_id(),
        second.request_id(),
    ];
    for (index, id) in ids.iter().enumerate() {
        assert!(!ids[..index].contains(id), "request IDs are distinct");
    }

    let streams = server
        .cmd(Command::new("raw"))
        .await
        .expect("second raw command succeeds")
        .into_streams();
    assert_eq!(streams.0, b"stdout\n\n\xff");
    assert_eq!(streams.1, b"stderr\n\n\xfe");
    server.shutdown().await.expect("shutdown succeeds");
}

#[tokio::test]
async fn shared_shutdown_cancels_active_work_and_closes_every_clone() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let script = write_script(
        directory.path(),
        "blocking-tmux",
        r#"
if [ "$#" -eq 1 ] && [ "$1" = "-V" ]; then
    printf 'tmux 3.7b\n'
    exit 0
fi
previous=''
for argument do
    if [ "$previous" = block ]; then
        sleep 60 &
        descendant=$!
        printf '%s\n%s\n' "$$" "$descendant" > "$argument"
        wait "$descendant"
    fi
    if [ "$previous" = rejected ]; then
        : > "$argument"
        exit 0
    fi
    previous=$argument
done
exit 93
"#,
    );
    let pid_path = directory.path().join("active.pid");
    let rejected_marker = directory.path().join("rejected");
    let server = fake_server(&script, &directory.path().join("shutdown.sock"));
    let dispatch_server = server.clone();
    let dispatch_pid_path = pid_path.clone();
    let mut dispatch = tokio::spawn(async move {
        dispatch_server
            .cmd(Command::new("block").arg(dispatch_pid_path.into_os_string()))
            .await
    });
    let child_pids = read_pids(&pid_path, 2, &mut dispatch).await;

    let first = server.clone();
    let second = server.clone();
    let (first_shutdown, second_shutdown) = tokio::join!(first.shutdown(), second.shutdown());
    first_shutdown.expect("first shutdown succeeds");
    second_shutdown.expect("concurrent shutdown succeeds");
    server.shutdown().await.expect("repeated shutdown succeeds");

    assert!(matches!(
        dispatch.await.expect("dispatch task remains healthy"),
        Err(Error::ExecutorShutdown { .. })
    ));
    for child_pid in child_pids {
        assert_process_gone(child_pid).await;
    }
    assert!(matches!(
        server
            .clone()
            .cmd(Command::new("rejected").arg(rejected_marker.clone().into_os_string()))
            .await,
        Err(Error::ExecutorShutdown { .. })
    ));
    assert!(
        !rejected_marker.exists(),
        "closed executor never spawns again"
    );
}

#[tokio::test]
async fn configured_default_timeout_is_applied_and_reaps_the_child() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let script = write_script(
        directory.path(),
        "timeout-tmux",
        r#"
if [ "$#" -eq 1 ] && [ "$1" = "-V" ]; then
    printf 'tmux 3.7b\n'
    exit 0
fi
previous=''
for argument do
    if [ "$previous" = block ]; then
        printf '%s\n' "$$" > "$argument"
        while :; do sleep 60; done
    fi
    previous=$argument
done
exit 94
"#,
    );
    let pid_path = directory.path().join("timeout.pid");
    let server = Server::builder()
        .tmux_executable(&script)
        .socket_path(directory.path().join("timeout.sock"))
        .default_timeout(Duration::from_secs(2))
        .build()
        .expect("server configuration is valid");
    let dispatch_server = server.clone();
    let dispatch_pid_path = pid_path.clone();
    let mut dispatch = tokio::spawn(async move {
        dispatch_server
            .cmd(Command::new("block").arg(dispatch_pid_path.into_os_string()))
            .await
    });
    let child_pid = read_pid(&pid_path, &mut dispatch).await;
    let error = dispatch
        .await
        .expect("dispatch task remains healthy")
        .expect_err("blocking command reaches configured timeout");

    assert!(matches!(error, Error::Timeout { .. }));
    assert_process_gone(child_pid).await;
    server.shutdown().await.expect("shutdown remains clean");
}

#[tokio::test]
async fn missing_executable_is_inert_until_use_and_errors_are_path_safe() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let available = write_script(
        directory.path(),
        "available-tmux",
        "[ \"$#\" -eq 1 ] && [ \"$1\" = \"-V\" ]\nprintf 'tmux 3.7b\\n'",
    );
    let executable = directory.path().join("sentinel-missing-tmux");
    let server = Server::builder()
        .tmux_executable(&executable)
        .socket_path(directory.path().join("missing.sock"))
        .build()
        .expect("a missing configured executable is valid inert setup");

    let command_error = server
        .cmd(Command::new("display-message"))
        .await
        .expect_err("first command reports the missing executable");
    let capability_error = server
        .capabilities()
        .await
        .expect_err("capability probing reports the missing executable");
    assert!(matches!(&command_error, Error::ExecutableNotFound { .. }));
    assert!(matches!(
        &capability_error,
        Error::ExecutableNotFound { .. }
    ));
    assert_error_path_safe(&command_error, "sentinel-missing-tmux");
    assert_error_path_safe(&capability_error, "sentinel-missing-tmux");
    symlink(&available, &executable).expect("missing executable path becomes available atomically");
    assert_eq!(
        server
            .capabilities()
            .await
            .expect("failed missing-executable probe is retried")
            .tmux_version()
            .raw(),
        "3.7b"
    );
    server.shutdown().await.expect("shutdown succeeds");
}

#[test]
fn server_operations_run_on_a_current_thread_runtime_created_after_construction() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let script = write_script(
        directory.path(),
        "current-thread-tmux",
        r#"
if [ "$#" -eq 1 ] && [ "$1" = "-V" ]; then
    printf 'tmux 3.7b\n'
    exit 0
fi
printf 'current-thread\n'
"#,
    );
    let server = fake_server(&script, &directory.path().join("current-thread.sock"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime builds");

    runtime.block_on(async {
        assert_eq!(
            server
                .capabilities()
                .await
                .expect("capabilities succeed")
                .tmux_version()
                .raw(),
            "3.7b"
        );
        let result = server
            .cmd(Command::new("display-message"))
            .await
            .expect("raw command succeeds");
        assert_eq!(result.stdout(), b"current-thread\n");
        server.shutdown().await.expect("shutdown succeeds");
    });
}

#[test]
fn extreme_default_timeouts_do_not_overflow_current_thread_dispatch() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let script = write_script(
        directory.path(),
        "extreme-timeout-tmux",
        "printf 'extreme-timeout-command\\n'",
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime builds");

    for (name, timeout) in [
        ("huge-finite", Duration::from_secs(u64::MAX / 2)),
        ("maximum", Duration::MAX),
    ] {
        let server = Server::builder()
            .tmux_executable(&script)
            .socket_path(directory.path().join(format!("{name}.sock")))
            .default_timeout(timeout)
            .build()
            .expect("extreme timeout is valid configuration");

        runtime.block_on(async {
            let result = server
                .cmd(Command::new("display-message"))
                .await
                .expect("extreme timeout command succeeds");
            assert!(result.success());
            assert_eq!(result.stdout(), b"extreme-timeout-command\n");
            server.shutdown().await.expect("shutdown succeeds");
        });
    }
}

#[tokio::test]
async fn real_tmux_preserves_literal_semicolon_effects_on_an_isolated_socket() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let socket = directory.path().join("socket;");
    let config = directory.path().join("config;");
    fs::write(&config, "set -g status off\n").expect("config is writable");
    let _guard = RealTmuxGuard {
        socket: socket.clone(),
    };
    let server = Server::builder()
        .socket_path(&socket)
        .config_file(&config)
        .build()
        .expect("isolated real-tmux server configuration");

    let started = server
        .cmd(
            Command::new("new-session")
                .arg("-d")
                .arg("-s")
                .arg("libtmux-foundation"),
        )
        .await
        .expect("tmux starts on the isolated socket");
    assert!(
        started.success(),
        "tmux stderr: {:?}",
        started.stderr_lossy()
    );
    let configured = server
        .cmd(Command::new("show-options").arg("-gv").arg("status"))
        .await
        .expect("show-options executes");
    assert_eq!(configured.stdout(), b"off\n");

    let cases = [
        ("LIBTMUX_SEMICOLON", ";"),
        ("LIBTMUX_TRAILING", "value;"),
        ("LIBTMUX_INTERIOR", "a;b"),
        ("LIBTMUX_BACKSLASH", "\\;"),
    ];
    for (name, value) in cases {
        let set = server
            .cmd(
                Command::new("set-environment")
                    .arg("-g")
                    .arg(name)
                    .arg(value),
            )
            .await
            .expect("set-environment executes");
        assert!(set.success(), "tmux stderr: {:?}", set.stderr_lossy());

        let shown = server
            .cmd(Command::new("show-environment").arg("-g").arg(name))
            .await
            .expect("show-environment executes");
        assert!(shown.success(), "tmux stderr: {:?}", shown.stderr_lossy());
        assert_eq!(
            shown.stdout(),
            format!("{name}={value}\n").as_bytes(),
            "logical value {value:?} changed after tmux parsing",
        );
    }

    server.shutdown().await.expect("executor shutdown succeeds");
    drop(server);
    let daemon_status = process::Command::new("tmux")
        .arg("-S")
        .arg(&socket)
        .arg("has-session")
        .arg("-t")
        .arg("libtmux-foundation")
        .status()
        .expect("external tmux client starts");
    assert!(
        daemon_status.success(),
        "client shutdown leaves the isolated tmux daemon running"
    );
}

#[test]
fn an_absent_tmux_variable_is_told_apart_from_a_malformed_one() {
    // No variable at all: the ordinary state of a process nobody started
    // inside tmux, which a caller may reasonably branch on.
    let outside = Server::from_env_value(None::<OsString>).expect_err("not inside tmux");
    assert_configuration_error(&outside, ServerConfigurationErrorKind::NotInsideTmux, &[]);

    // Present but not tmux's triple: something rewrote it, which is a broken
    // environment rather than a state to branch on. Collapsing the two into
    // one variant loses exactly that distinction.
    for broken in ["", ",7,$0"] {
        let error = Server::from_env_value(Some(broken)).expect_err("malformed value");
        assert_configuration_error(
            &error,
            ServerConfigurationErrorKind::MalformedTmuxVariable,
            &[],
        );
    }

    // And a well-formed value still resolves.
    Server::from_env_value(Some("/tmp/libtmux-rs-dev/from-env.sock,7,$0"))
        .expect("a triple names a socket");
}
