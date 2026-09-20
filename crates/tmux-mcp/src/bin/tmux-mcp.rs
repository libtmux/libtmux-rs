//! Serve tmux over MCP on stdio.

use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use libtmux::{Command, ControlClientLimits, DispatchLimits, OutputLimits, Server};
use rmcp::model::{ErrorData, JsonRpcMessage};
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::{Transport, async_rw::AsyncRwTransport, stdio};
use rmcp::{RoleServer, ServiceExt as _};
use tmux_mcp::cli::{HELP, Options, Stop};
use tmux_mcp::{Selection, SocketProvenance, TmuxTools, environment_values_from_env};

const DEFAULT_SOCKET: &str = "libtmux-mcp";
const SOCKET_ENV: &str = "LIBTMUX_SOCKET";
const SOCKET_PATH_ENV: &str = "LIBTMUX_SOCKET_PATH";
const TMUX_CONFIG_ENV: &str = "LIBTMUX_TMUX_CONFIG";
const MINIMAL_CONFIG: &[u8] = include_bytes!("../../minimal.conf");
const LAUNCH_MARKER_OPTION: &str = "@libtmux_mcp_launch_nonce";

/// How many tmux commands this server runs at once.
const MAX_IN_FLIGHT: usize = 4;

/// How many live watchers and waits may keep a tmux client attached.
const MAX_CONTROL_CLIENTS: usize = 16;

/// How long a saturated observer lane holds an MCP request.
const CONTROL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(1);

/// How many bytes one tool's tmux command may read.
///
/// Well above any answer a tool returns -- the tool layer caps its own
/// responses far lower -- and far below the point where reading it is the
/// problem.
const MAX_TOOL_STDOUT_BYTES: usize = 8 * 1024 * 1024;

/// How many bytes of tmux's diagnostics one command may read.
const MAX_TOOL_STDERR_BYTES: usize = 256 * 1024;
const MAX_SERIALIZED_REQUEST_ID_BYTES: usize = 512 * 1024;

struct RequestIdTransport<T> {
    inner: T,
}

impl<T> RequestIdTransport<T> {
    const fn new(inner: T) -> Self {
        Self { inner }
    }
}

impl<T> Transport<RoleServer> for RequestIdTransport<T>
where
    T: Transport<RoleServer>,
{
    type Error = T::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            let message = self.inner.receive().await?;
            let oversized = match &message {
                JsonRpcMessage::Request(request) => serde_json::to_vec(&request.id)
                    .map_or(true, |id| id.len() > MAX_SERIALIZED_REQUEST_ID_BYTES),
                _ => false,
            };
            if !oversized {
                return Some(message);
            }
            let response = TxJsonRpcMessage::<RoleServer>::error(
                ErrorData::invalid_request("Request ID exceeds the response framing limit", None),
                None,
            );
            if self.inner.send(response).await.is_err() {
                return None;
            }
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    // stdout carries the protocol, so everything meant for a person goes to
    // stderr, which is where an MCP client collects a server's log.
    let options = match Options::parse(std::env::args_os().skip(1)) {
        Ok(options) => options,
        Err(Stop::Help) => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(Stop::Version) => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(Stop::Misuse(reason)) => {
            eprintln!("tmux-mcp: {reason}");
            eprintln!("try `tmux-mcp --help`");
            return ExitCode::FAILURE;
        }
    };

    match serve(options).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tmux-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Build the server the options describe, and run it until stdin closes.
#[allow(
    clippy::too_many_lines,
    reason = "startup freezes socket, provenance, and tool selection in one auditable flow"
)]
async fn serve(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    // Bound concurrency and core reads before the tool layer truncates replies.
    let mut builder = Server::builder()
        .dispatch_limits(DispatchLimits::default().max_in_flight(MAX_IN_FLIGHT))
        .control_client_limits(
            ControlClientLimits::default()
                .max_clients(MAX_CONTROL_CLIENTS)
                .acquire_timeout(Some(CONTROL_ACQUIRE_TIMEOUT)),
        )
        .output_limits(
            OutputLimits::default()
                .max_stdout_bytes(MAX_TOOL_STDOUT_BYTES)
                .max_stderr_bytes(MAX_TOOL_STDERR_BYTES),
        );
    let cli_selected = options.socket_path.is_some() || options.socket_name.is_some();
    let env_path = (!cli_selected)
        .then(|| std::env::var_os(SOCKET_PATH_ENV))
        .flatten();
    let env_name = (!cli_selected)
        .then(|| std::env::var_os(SOCKET_ENV))
        .flatten();
    if env_path.is_some() && env_name.is_some() {
        return Err(format!("set either {SOCKET_ENV} or {SOCKET_PATH_ENV}, not both").into());
    }
    let socket_path = options.socket_path.or_else(|| env_path.map(PathBuf::from));
    let socket_name = options.socket_name.or(env_name);
    let default_socket = socket_path.is_none() && socket_name.is_none();
    if let Some(path) = socket_path {
        builder = builder.socket_path(path);
    } else if let Some(name) = socket_name {
        builder = builder.socket_name(name);
    } else {
        builder = builder.socket_name(DEFAULT_SOCKET);
    }

    let configured_tmux = std::env::var_os(TMUX_CONFIG_ENV).map(PathBuf::from);
    if configured_tmux
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty())
    {
        return Err(format!("{TMUX_CONFIG_ENV} must not be empty").into());
    }
    if configured_tmux
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err(format!("{TMUX_CONFIG_ENV} must be an absolute path").into());
    }
    let user_configured = configured_tmux.is_some();
    let default_config = default_socket && !user_configured;
    let minimal_config = if default_config {
        Some(ShippedMinimalConfig::materialize()?)
    } else {
        None
    };
    if let Some(path) = configured_tmux {
        builder = builder.config_file(path);
    } else if let Some(config) = &minimal_config {
        builder = builder.config_file(config.path());
    }
    let server = builder.build()?;

    // Validate the frozen surface before the first tmux command. Its default
    // does not matter for validation; the liveness result below supplies it.
    let validation_selection = Selection::from_env(false)?;
    TmuxTools::builder(server.clone())
        .selection(validation_selection)
        .try_build()?;
    let environment_values = environment_values_from_env()?;

    // Held before the liveness check, so an owner cannot stop the daemon
    // between this process finding it and starting to use it.
    let dedicated = Server::builder().socket_name(DEFAULT_SOCKET).build()?;
    let lease = if server.socket_path() == dedicated.socket_path() {
        Some(
            SocketLease::share(server.socket_path())
                .await
                .map_err(|error| {
                    format!(
                        "cannot lease the dedicated tmux socket at {}: {error}",
                        server.socket_path().display()
                    )
                })?,
        )
    } else {
        None
    };

    let existing_before = match server.check_alive().await {
        Ok(()) => true,
        Err(libtmux::Error::ServerGone { .. }) => false,
        Err(error) => {
            return Err(format!("cannot inspect selected tmux daemon: {error}").into());
        }
    };
    let created_dedicated = if default_config && !existing_before {
        // `check_alive` just reported the endpoint unreachable, so whatever
        // holds the path answers for no daemon -- a socket a killed server
        // left behind, or something else entirely. tmux will not bind over
        // it, and clears it on the way in only on Linux, so a macOS start
        // failed with "no server for start-server" against a path this
        // process is about to own. Only the socket this process chose is
        // cleared; a caller that named one keeps whatever is there.
        if default_socket {
            match std::fs::remove_file(server.socket_path()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "cannot clear the unreachable tmux endpoint at {}: {error}",
                        server.socket_path().display()
                    )
                    .into());
                }
            }
        }
        server
            .start()
            .await
            .map_err(|error| format!("cannot start dedicated tmux daemon: {error}"))?;
        let marker = server
            .cmd(
                Command::new("show-options")
                    .arg("-gv")
                    .arg(LAUNCH_MARKER_OPTION),
            )
            .await
            .map_err(|error| format!("cannot authenticate dedicated tmux daemon: {error}"))?;
        marker.success()
            && minimal_config.as_ref().is_some_and(|config| {
                String::from_utf8_lossy(marker.stdout()).trim() == config.nonce()
            })
    } else {
        false
    };
    let existing = existing_before || default_config;
    let provenance = match (default_config, user_configured, existing, created_dedicated) {
        (true, _, true, true) => SocketProvenance::DedicatedMinimal,
        (true, _, true, false) => SocketProvenance::DedicatedExisting,
        (true, _, false, _) => {
            return Err("dedicated tmux daemon did not remain live after startup".into());
        }
        (false, true, false, _) => SocketProvenance::UserConfigured,
        (false, true, true, _) => SocketProvenance::UserConfiguredExisting,
        (false, false, false, _) => SocketProvenance::OperatorSelectedAbsent,
        (false, false, true, _) => SocketProvenance::OperatorSelected,
    };
    let selection = Selection::from_env(provenance.defaults_to_teardown())?;
    let tools = TmuxTools::builder(server.clone())
        .selection(selection)
        .socket_provenance(provenance)
        .environment_values(environment_values)
        .try_build()?;

    // Log the frozen surface and socket choice once at startup.
    eprintln!(
        "tmux-mcp {} serving {} tools on one {} socket{}",
        env!("CARGO_PKG_VERSION"),
        tools.offered().len(),
        provenance_label(provenance),
        tools
            .caller_pane()
            .map(|pane| format!(", from pane {pane}"))
            .unwrap_or_default(),
    );
    if tools.caller_is_malformed() {
        eprintln!(
            "tmux-mcp: TMUX and TMUX_PANE do not describe one tmux pane, so pane-input and \
             teardown tools will refuse every call; unset both, or start tmux-mcp from a tmux \
             pane"
        );
    }

    let (stdin, stdout) = stdio();
    let transport = RequestIdTransport::new(AsyncRwTransport::<RoleServer, _, _>::new_server(
        stdin, stdout,
    ));
    let service = tools.serve(transport).await;
    let result: Result<(), Box<dyn std::error::Error>> = match service {
        Ok(service) => service
            .waiting()
            .await
            .map(|_| ())
            .map_err(Box::<dyn std::error::Error>::from),
        Err(error) => Err(Box::new(error)),
    };
    let alone = lease.as_ref().is_some_and(SocketLease::try_exclusive);
    if created_dedicated && alone {
        let killed = server.kill().await;
        let shutdown = server.shutdown().await;
        drop(lease);
        result?;
        killed?;
        shutdown?;
    } else {
        if created_dedicated {
            eprintln!(
                "tmux-mcp: leaving the dedicated tmux daemon running, because another \
                 tmux-mcp still uses it"
            );
        }
        result?;
    }
    Ok(())
}

/// A share in the dedicated socket, held for this process's whole life.
///
/// Every process on the dedicated socket holds a shared `flock` on a file
/// beside it, and the process that started the daemon stops it only when an
/// exclusive lock succeeds, which proves no other process holds a share. The
/// kernel releases a dead process's share, so a crash leaves no stale lease.
struct SocketLease(std::fs::File);

impl SocketLease {
    /// Take a share, waiting out an owner that is stopping the daemon.
    async fn share(socket: &Path) -> io::Result<Self> {
        let directory = socket
            .parent()
            .ok_or_else(|| io::Error::other("the socket path has no directory"))?
            .to_path_buf();
        let path = socket.with_file_name(format!("{DEFAULT_SOCKET}.lease"));
        tokio::task::spawn_blocking(move || {
            // tmux refuses a socket directory that others can read, and
            // creates this one 0700 itself when it gets there first.
            match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&path)?;
            rustix::fs::flock(&file, rustix::fs::FlockOperation::LockShared)?;
            Ok(Self(file))
        })
        .await
        .map_err(io::Error::other)?
    }

    /// Whether this process is the only one holding a share.
    ///
    /// On success the share becomes exclusive, so a process starting now
    /// waits in [`Self::share`] until the daemon is gone and then starts its
    /// own. On failure the share may be gone too, which matters to nothing:
    /// the caller is exiting.
    fn try_exclusive(&self) -> bool {
        rustix::fs::flock(
            &self.0,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .is_ok()
    }
}

const fn provenance_label(provenance: SocketProvenance) -> &'static str {
    match provenance {
        SocketProvenance::DedicatedMinimal => "dedicated minimal",
        SocketProvenance::DedicatedExisting => "existing dedicated",
        SocketProvenance::OperatorSelected => "operator-selected",
        SocketProvenance::OperatorSelectedAbsent => "absent operator-selected",
        SocketProvenance::UserConfigured => "user-configured",
        SocketProvenance::UserConfiguredExisting => "existing user-configured",
        SocketProvenance::Unknown => "unknown-provenance",
    }
}

struct ShippedMinimalConfig {
    path: PathBuf,
    nonce: String,
}

impl ShippedMinimalConfig {
    fn materialize() -> io::Result<Self> {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let suffix = u128::from_le_bytes(random);
        let nonce = format!("{suffix:032x}");
        let path = std::env::temp_dir().join(format!("libtmux-mcp-{nonce}.conf"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let written = file
            .write_all(MINIMAL_CONFIG)
            .and_then(|()| writeln!(file, "set -g {LAUNCH_MARKER_OPTION} {nonce}"))
            .and_then(|()| file.flush());
        if let Err(error) = written {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        Ok(Self { path, nonce })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn nonce(&self) -> &str {
        &self.nonce
    }
}

impl Drop for ShippedMinimalConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
