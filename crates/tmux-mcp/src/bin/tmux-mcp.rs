//! Serve tmux over MCP on stdio.

use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use libtmux::{Command, ControlClientLimits, DispatchLimits, OutputLimits, Server};
use rmcp::model::{ErrorData, JsonRpcMessage};
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::{Transport, async_rw::AsyncRwTransport, stdio};
use rmcp::{RoleServer, ServiceExt as _};
use tmux_mcp::cli::{HELP, Options, Stop};
use tmux_mcp::{Selection, SocketProvenance, TmuxTools};

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

    let existing_before = match server.check_alive().await {
        Ok(()) => true,
        Err(libtmux::Error::ServerGone { .. }) => false,
        Err(error) => {
            return Err(format!("cannot inspect selected tmux daemon: {error}").into());
        }
    };
    let created_dedicated = if default_config && !existing_before {
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
    if created_dedicated {
        let killed = server.kill().await;
        let shutdown = server.shutdown().await;
        result?;
        killed?;
        shutdown?;
    } else {
        result?;
    }
    Ok(())
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
