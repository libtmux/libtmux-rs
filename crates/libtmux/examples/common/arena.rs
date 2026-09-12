//! The arena contract: `LIBTMUX_ARENA_DESCRIPTOR` is the only activation
//! signal. Once it is set, a matching `LIBTMUX_ARENA_ARTIFACT` plus
//! `LIBTMUX_SOCKET_PATH` and `LIBTMUX_TMUX_BIN` are all required, and a
//! missing or mismatched one fails closed rather than falling back. An
//! activated example checks that the server it built reports the socket path
//! it was given, then prints one `LIBTMUX_ARENA_EVIDENCE={json}` line --
//! schema 1, artifact, challenge (the `@libtmux_arena_challenge` option),
//! `server_pid`, and `socket_path`.
//!
//! Cargo compiles each example file as its own crate, so this is included
//! with `#[path = "common/arena.rs"] mod arena;` rather than named as a plain
//! module path, and every caller passes its own artifact id.

use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
};

use libtmux::Server;

#[derive(Clone, Debug)]
pub(crate) struct ArenaEnvironment {
    pub(crate) descriptor: Option<OsString>,
    pub(crate) artifact: Option<OsString>,
    pub(crate) socket_path: Option<OsString>,
    pub(crate) tmux_executable: Option<OsString>,
    pub(crate) tmux: Option<OsString>,
}

impl ArenaEnvironment {
    pub(crate) fn capture() -> Self {
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
pub(crate) struct ArenaContract {
    pub(crate) socket_path: PathBuf,
    pub(crate) tmux_executable: OsString,
}

impl ArenaContract {
    pub(crate) fn from_environment(
        environment: &ArenaEnvironment,
        artifact_id: &str,
    ) -> Result<Option<Self>, std::io::Error> {
        if environment
            .descriptor
            .as_deref()
            .is_none_or(OsStr::is_empty)
        {
            return Ok(None);
        }

        if environment.artifact.as_deref() != Some(OsStr::new(artifact_id)) {
            return Err(arena_error(format!(
                "arena descriptor requires LIBTMUX_ARENA_ARTIFACT={artifact_id}"
            )));
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

pub(crate) fn arena_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

fn required_arena_value(
    value: Option<OsString>,
    message: &'static str,
) -> Result<OsString, std::io::Error> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| arena_error(message))
}

/// Resolve the server this run addresses: the arena's endpoint when
/// activated, or this example's own fallback otherwise.
pub(crate) fn select_server(
    environment: &ArenaEnvironment,
    artifact_id: &str,
) -> Result<(Server, Option<ArenaContract>), Box<dyn std::error::Error>> {
    let mut arena = ArenaContract::from_environment(environment, artifact_id)?;
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

/// The one evidence line an activated run owes the arena that lent it a
/// server.
pub(crate) async fn arena_evidence(
    server: &Server,
    arena: &ArenaContract,
    artifact_id: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if server.socket_path() != arena.socket_path {
        return Err(arena_error("arena server resolved a different socket").into());
    }
    let challenge = server
        .get_global_option("@libtmux_arena_challenge")
        .await?
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| arena_error("arena challenge is missing"))?;
    let server_pid = server.generation().await?.pid();

    Ok(serde_json::json!({
        "artifact": artifact_id,
        "challenge": challenge,
        "schema": 1,
        "server_pid": server_pid,
        "socket_path": server.socket_path(),
    }))
}
