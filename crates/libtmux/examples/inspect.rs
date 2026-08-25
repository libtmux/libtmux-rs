//! Report what a tmux server is running.
//!
//! ```console
//! $ cargo run --example inspect
//! ```
//!
//! Reads the default server, or the one named by `$TMUX` when run inside a
//! pane. Changes nothing.

use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
};

use libtmux::{Server, TmuxText};

const ARENA_ARTIFACT: &str = "rust-inspect";

#[derive(Clone, Debug)]
struct ArenaEnvironment {
    descriptor: Option<OsString>,
    artifact: Option<OsString>,
    socket_path: Option<OsString>,
    tmux_executable: Option<OsString>,
    tmux: Option<OsString>,
}

impl ArenaEnvironment {
    fn capture() -> Self {
        Self {
            descriptor: std::env::var_os("LIBTMUX_ARENA_DESCRIPTOR"),
            artifact: std::env::var_os("LIBTMUX_ARENA_ARTIFACT"),
            socket_path: std::env::var_os("LIBTMUX_SOCKET_PATH"),
            tmux_executable: std::env::var_os("LIBTMUX_TMUX_BIN"),
            tmux: std::env::var_os("TMUX"),
        }
    }
}

#[derive(Clone, Debug)]
struct ArenaContract {
    socket_path: PathBuf,
    tmux_executable: OsString,
}

impl ArenaContract {
    fn from_environment(environment: &ArenaEnvironment) -> Result<Option<Self>, std::io::Error> {
        if environment
            .descriptor
            .as_deref()
            .is_none_or(OsStr::is_empty)
        {
            return Ok(None);
        }

        if environment.artifact.as_deref() != Some(OsStr::new(ARENA_ARTIFACT)) {
            return Err(arena_error(
                "arena descriptor requires LIBTMUX_ARENA_ARTIFACT=rust-inspect",
            ));
        }
        let socket_path = required_arena_value(
            environment.socket_path.clone(),
            "arena descriptor requires LIBTMUX_SOCKET_PATH",
        )?;
        let tmux_executable = required_arena_value(
            environment.tmux_executable.clone(),
            "arena descriptor requires LIBTMUX_TMUX_BIN",
        )?;

        Ok(Some(Self {
            socket_path: socket_path.into(),
            tmux_executable,
        }))
    }
}

fn arena_error(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
}

fn required_arena_value(
    value: Option<OsString>,
    message: &'static str,
) -> Result<OsString, std::io::Error> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| arena_error(message))
}

fn select_server(
    environment: &ArenaEnvironment,
) -> Result<(Server, Option<ArenaContract>), Box<dyn std::error::Error>> {
    let mut arena = ArenaContract::from_environment(environment)?;
    let server = if let Some(arena) = &mut arena {
        let server = Server::builder()
            .socket_path(&arena.socket_path)
            .tmux_executable(arena.tmux_executable.clone())
            .build()?;
        arena.socket_path = server.socket_path().to_path_buf();
        server
    } else {
        Server::from_env_value(environment.tmux.clone()).or_else(|_| Server::new())?
    };

    Ok((server, arena))
}

fn show(value: &TmuxText) -> String {
    value.to_string_lossy().into_owned()
}

/// The same, for a field tmux may genuinely not report.
fn show_optional(value: Option<&TmuxText>) -> String {
    value.map_or_else(|| "-".to_owned(), show)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(evidence) = inspect(ArenaEnvironment::capture()).await? {
        println!("LIBTMUX_ARENA_EVIDENCE={evidence}");
    }
    Ok(())
}

async fn inspect(
    environment: ArenaEnvironment,
) -> Result<Option<serde_json::Value>, Box<dyn std::error::Error>> {
    let (server, arena) = select_server(&environment)?;
    // Inside a pane, `$TMUX` names the server this process belongs to.
    // Outside one, fall back to the default socket.
    if !server.is_alive().await {
        if arena.is_some() {
            server.shutdown().await?;
            return Err(arena_error("arena server is not alive").into());
        }
        println!("no tmux server at {}", server.socket_path().display());
        return Ok(None);
    }

    // Three tmux commands, not one per object: walking down would cost a
    // command per session and per window.
    for branch in server.hierarchy().await? {
        let session = &branch.session;
        println!(
            "{session} {} ({} windows{})",
            show(session.name()),
            session.window_count(),
            if session.is_attached() {
                ", attached"
            } else {
                ""
            },
        );

        for built in &branch.windows {
            let window = &built.window;
            println!(
                "  {window} {}{}",
                show(window.name()),
                if window.is_active() { " *" } else { "" },
            );

            for pane in &built.panes {
                println!(
                    "    {pane} {} in {}",
                    show_optional(pane.current_command()),
                    show_optional(pane.current_path()),
                );
            }
        }
    }

    let evidence = if let Some(arena) = &arena {
        Some(arena_evidence(&server, arena).await?)
    } else {
        None
    };
    server.shutdown().await?;
    Ok(evidence)
}

async fn arena_evidence(
    server: &Server,
    arena: &ArenaContract,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if server.socket_path() != arena.socket_path {
        return Err(arena_error("arena server resolved a different socket").into());
    }
    let challenge = server
        .get_global_option("@libtmux_arena_challenge")
        .await?
        .map(|value| show(&value))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| arena_error("arena challenge is missing"))?;
    let server_pid = server.generation().await?.pid();

    Ok(serde_json::json!({
        "artifact": ARENA_ARTIFACT,
        "challenge": challenge,
        "schema": 1,
        "server_pid": server_pid,
        "socket_path": server.socket_path(),
    }))
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::{OsStr, OsString},
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        process::Command,
    };

    use libtmux::{
        Server,
        test::{DaemonState, TestServer},
    };

    use super::{
        ARENA_ARTIFACT, ArenaContract, ArenaEnvironment, arena_evidence, inspect, select_server,
    };

    const CHILD: &str = "LIBTMUX_INSPECT_TEST_CHILD";

    fn tmux_wrapper(directory: &Path) -> Result<(PathBuf, PathBuf), std::io::Error> {
        let executable = directory.join("arena-tmux");
        let marker = directory.join("arena-tmux.used");
        fs::write(
            &executable,
            "#!/bin/sh\n: > \"${0}.used\"\nexec tmux \"$@\"\n",
        )?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
        Ok((executable, marker))
    }

    fn environment(
        descriptor: Option<&str>,
        artifact: Option<&str>,
        socket_path: Option<&str>,
        tmux_executable: Option<&str>,
        tmux: Option<OsString>,
    ) -> ArenaEnvironment {
        ArenaEnvironment {
            descriptor: descriptor.map(Into::into),
            artifact: artifact.map(Into::into),
            socket_path: socket_path.map(Into::into),
            tmux_executable: tmux_executable.map(Into::into),
            tmux,
        }
    }

    #[test]
    fn activated_partial_contract_is_rejected() {
        if std::env::var_os(CHILD).is_some() {
            assert!(super::main().is_err());
            return;
        }

        let output = Command::new(std::env::current_exe().expect("test executable"))
            .arg("--exact")
            .arg("tests::activated_partial_contract_is_rejected")
            .arg("--nocapture")
            .env(CHILD, "1")
            .env("LIBTMUX_ARENA_DESCRIPTOR", "arena")
            .env_remove("LIBTMUX_ARENA_ARTIFACT")
            .env_remove("LIBTMUX_SOCKET_PATH")
            .env_remove("LIBTMUX_TMUX_BIN")
            .env_remove("TMUX")
            .output()
            .expect("child test starts");

        assert!(
            output.status.success(),
            "partial arena contract was accepted:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn activated_empty_partial_and_mismatched_contracts_fail_closed() {
        let cases = [
            environment(Some("arena"), None, Some("/socket"), Some("tmux"), None),
            environment(Some("arena"), Some(""), Some("/socket"), Some("tmux"), None),
            environment(
                Some("arena"),
                Some(ARENA_ARTIFACT),
                None,
                Some("tmux"),
                None,
            ),
            environment(
                Some("arena"),
                Some(ARENA_ARTIFACT),
                Some(""),
                Some("tmux"),
                None,
            ),
            environment(
                Some("arena"),
                Some(ARENA_ARTIFACT),
                Some("/socket"),
                None,
                None,
            ),
            environment(
                Some("arena"),
                Some(ARENA_ARTIFACT),
                Some("/socket"),
                Some(""),
                None,
            ),
            environment(
                Some("arena"),
                Some("other-artifact"),
                Some("/socket"),
                Some("tmux"),
                None,
            ),
        ];

        for environment in cases {
            assert!(ArenaContract::from_environment(&environment).is_err());
            assert!(select_server(&environment).is_err());
        }
    }

    #[tokio::test]
    async fn aliases_without_a_descriptor_keep_tmux_selection()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut guard = TestServer::new().await?;
        let tmux = OsString::from(format!("{},0,0", guard.socket_path().display()));
        let environment = environment(
            None,
            Some("other-artifact"),
            Some("/ignored.socket"),
            Some("not-tmux"),
            Some(tmux),
        );

        let (server, arena) = select_server(&environment)?;
        assert!(arena.is_none());
        assert_eq!(server.socket_path(), guard.socket_path());
        assert_eq!(server.tmux_executable(), OsStr::new("tmux"));
        assert!(server.is_alive().await);
        server.shutdown().await?;
        assert_eq!(guard.daemon_state(), DaemonState::Running);
        guard.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn owned_endpoint_produces_json_evidence_without_stopping_the_daemon()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut guard = TestServer::new().await?;
        let wrapper_directory = tempfile::tempdir()?;
        let (tmux_executable, wrapper_marker) = tmux_wrapper(wrapper_directory.path())?;
        guard.session("inspect").await?;
        guard
            .server()
            .set_global_option("@libtmux_arena_challenge", "inspect-challenge")
            .await?;
        let socket_path = guard.socket_path().to_path_buf();
        let environment = ArenaEnvironment {
            descriptor: Some("arena".into()),
            artifact: Some(ARENA_ARTIFACT.into()),
            socket_path: Some(socket_path.clone().into_os_string()),
            tmux_executable: Some(tmux_executable.into_os_string()),
            tmux: None,
        };

        let evidence = inspect(environment).await?.expect("arena evidence");
        assert_eq!(evidence["artifact"], ARENA_ARTIFACT);
        assert_eq!(evidence["challenge"], "inspect-challenge");
        assert_eq!(evidence["server_pid"], guard.daemon_pid());
        assert_eq!(
            evidence["socket_path"],
            socket_path.to_string_lossy().as_ref()
        );
        assert!(serde_json::from_str::<serde_json::Value>(&evidence.to_string()).is_ok());
        assert!(wrapper_marker.is_file());
        assert_eq!(guard.daemon_state(), DaemonState::Running);
        assert!(guard.server().is_alive().await);
        guard.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn relative_owned_endpoint_produces_json_evidence()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut guard = TestServer::new().await?;
        guard.session("inspect").await?;
        guard
            .server()
            .set_global_option("@libtmux_arena_challenge", "relative-challenge")
            .await?;
        let working_directory = std::env::current_dir()?;
        let link_root = tempfile::Builder::new()
            .prefix("inspect-")
            .tempdir_in(&working_directory)?;
        let socket_directory = guard.socket_path().parent().expect("socket parent");
        let linked_directory = link_root.path().join("endpoint");
        std::os::unix::fs::symlink(socket_directory, &linked_directory)?;
        let relative_socket = linked_directory
            .join(guard.socket_path().file_name().expect("socket filename"))
            .strip_prefix(&working_directory)?
            .to_path_buf();
        assert!(relative_socket.is_relative());
        let expected_socket_path = working_directory.join(&relative_socket);
        let environment = ArenaEnvironment {
            descriptor: Some("arena".into()),
            artifact: Some(ARENA_ARTIFACT.into()),
            socket_path: Some(relative_socket.into_os_string()),
            tmux_executable: Some(guard.server().tmux_executable().to_os_string()),
            tmux: None,
        };

        let evidence = inspect(environment).await?.expect("arena evidence");
        assert_eq!(evidence["challenge"], "relative-challenge");
        assert_eq!(
            evidence["socket_path"],
            expected_socket_path.to_string_lossy().as_ref()
        );
        assert_eq!(guard.daemon_state(), DaemonState::Running);
        guard.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn evidence_rejects_a_socket_other_than_the_requested_endpoint()
    -> Result<(), Box<dyn std::error::Error>> {
        let guard = TestServer::new().await?;
        guard
            .server()
            .set_global_option("@libtmux_arena_challenge", "inspect-challenge")
            .await?;
        let server = Server::builder()
            .socket_path(guard.socket_path())
            .tmux_executable(guard.server().tmux_executable())
            .build()?;
        let arena = ArenaContract {
            socket_path: Path::new("/different.socket").into(),
            tmux_executable: guard.server().tmux_executable().to_os_string(),
        };

        assert!(arena_evidence(&server, &arena).await.is_err());
        server.shutdown().await?;
        guard.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn evidence_requires_a_nonempty_challenge() -> Result<(), Box<dyn std::error::Error>> {
        let guard = TestServer::new().await?;
        let server = Server::builder()
            .socket_path(guard.socket_path())
            .tmux_executable(guard.server().tmux_executable())
            .build()?;
        let arena = ArenaContract {
            socket_path: guard.socket_path().to_path_buf(),
            tmux_executable: guard.server().tmux_executable().to_os_string(),
        };

        assert!(arena_evidence(&server, &arena).await.is_err());
        server.shutdown().await?;
        guard.shutdown().await?;
        Ok(())
    }
}
