//! Report what a tmux server is running.
//!
//! ```console
//! $ cargo run --example inspect
//! ```
//!
//! Reads the default server, or the one named by `$TMUX` when run inside a
//! pane. Changes nothing.

use libtmux::TmuxText;

#[path = "common/arena.rs"]
mod arena;

use arena::{ArenaEnvironment, arena_error, arena_evidence, select_server};

const ARENA_ARTIFACT: &str = "rust-inspect";

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
    let (server, arena) = select_server(&environment, ARENA_ARTIFACT)?;
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
        Some(arena_evidence(&server, arena, ARENA_ARTIFACT).await?)
    } else {
        None
    };
    server.shutdown().await?;
    Ok(evidence)
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

    use super::arena::{ArenaContract, ArenaEnvironment, arena_evidence, select_server};
    use super::{ARENA_ARTIFACT, inspect};

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
            assert!(ArenaContract::from_environment(&environment, ARENA_ARTIFACT).is_err());
            assert!(select_server(&environment, ARENA_ARTIFACT).is_err());
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

        let (server, arena) = select_server(&environment, ARENA_ARTIFACT)?;
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

        assert!(
            arena_evidence(&server, &arena, ARENA_ARTIFACT)
                .await
                .is_err()
        );
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

        assert!(
            arena_evidence(&server, &arena, ARENA_ARTIFACT)
                .await
                .is_err()
        );
        server.shutdown().await?;
        guard.shutdown().await?;
        Ok(())
    }
}
