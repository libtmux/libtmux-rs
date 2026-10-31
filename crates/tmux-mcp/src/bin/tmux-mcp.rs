//! Serve tmux over MCP on stdio.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use libtmux::{ControlClientLimits, DispatchLimits, OutputLimits, Server};
use rmcp::model::{ErrorData, JsonRpcMessage};
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::{Transport, async_rw::AsyncRwTransport, stdio};
use rmcp::{RoleServer, ServiceExt as _};
use tmux_mcp::cli::{HELP, Options, Stop};
use tmux_mcp::{Selection, SocketProvenance, TmuxTools, environment_values_from_env};

const TMUX_CONFIG_ENV: &str = "LIBTMUX_TMUX_CONFIG";

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
    // A socket named on the command line wins. Otherwise the core reads
    // `LIBTMUX_SOCKET_PATH`, then `LIBTMUX_SOCKET`, and with neither set tmux
    // picks the server itself.
    if let Some(path) = options.socket_path {
        builder = builder.socket_path(path);
    } else if let Some(name) = options.socket_name {
        builder = builder.socket_name(name);
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
    if let Some(path) = configured_tmux {
        builder = builder.config_file(path);
    }
    let server = builder.build()?;

    // Validate the frozen surface before the first tmux command. Its default
    // does not matter for validation; the liveness result below supplies it.
    let validation_selection = Selection::from_env(false)?;
    TmuxTools::builder(server.clone())
        .selection(validation_selection)
        .try_build()?;
    let environment_values = environment_values_from_env()?;

    let existing = match server.check_alive().await {
        Ok(()) => true,
        Err(libtmux::Error::ServerGone { .. }) => false,
        Err(error) => {
            return Err(format!("cannot inspect selected tmux daemon: {error}").into());
        }
    };
    let provenance = match (user_configured, existing) {
        (true, false) => SocketProvenance::UserConfigured,
        (true, true) => SocketProvenance::UserConfiguredExisting,
        (false, false) => SocketProvenance::OperatorSelectedAbsent,
        (false, true) => SocketProvenance::OperatorSelected,
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
    match service {
        Ok(service) => service
            .waiting()
            .await
            .map(|_| ())
            .map_err(Box::<dyn std::error::Error>::from),
        Err(error) => Err(Box::new(error)),
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
