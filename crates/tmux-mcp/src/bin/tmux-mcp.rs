//! Serve tmux over MCP on stdio.

use std::process::ExitCode;
use std::time::Duration;

use libtmux::{ControlClientLimits, DispatchLimits, OutputLimits, Server};
use rmcp::ServiceExt as _;
use rmcp::transport::stdio;
use tmux_mcp::cli::{HELP, Options, Stop};
use tmux_mcp::{Safety, TmuxTools};

/// How many tmux commands this server runs at once.
///
/// Small on purpose. tmux serializes commands on its own thread, so more
/// clients buy queueing rather than throughput, and an agent that fans out
/// should meet a bounded queue rather than a fork bomb.
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
    // An agent is the caller most able to ask for too much at once, and the
    // least able to notice it did. These bound the tmux side of that: how many
    // client processes may run at once, and how many bytes one command may
    // read before the answer stops being useful anyway.
    //
    // The tool layer's own caps do not cover this. Truncating a response after
    // the fact does not unspend the memory the core already allocated to read
    // it.
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
    if let Some(path) = options.socket_path {
        builder = builder.socket_path(path);
    }
    if let Some(name) = options.socket_name {
        builder = builder.socket_name(name);
    }
    let server = builder.build()?;

    let safety = options.safety.unwrap_or_else(Safety::from_env);
    let confirm = options.confirm || tmux_mcp::confirm_from_env();
    let tools = TmuxTools::builder(server)
        .safety(safety)
        .confirm(confirm)
        .build();

    // One line, once, naming what a later question about this process will
    // want: which tmux it chose, how much it will do, and where it thinks it
    // is. Silence here is what makes a misconfigured server hard to explain.
    eprintln!(
        "tmux-mcp {} serving {} tools at the {} tier{}",
        env!("CARGO_PKG_VERSION"),
        tools.offered().len(),
        safety.name(),
        tools
            .caller_pane()
            .map(|pane| format!(", from pane {pane}"))
            .unwrap_or_default(),
    );

    tools.serve(stdio()).await?.waiting().await?;
    Ok(())
}
