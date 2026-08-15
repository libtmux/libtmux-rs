//! Serve tmux over MCP on stdio.

use std::process::ExitCode;

use libtmux::Server;
use rmcp::ServiceExt as _;
use rmcp::transport::stdio;
use tmux_mcp::cli::{HELP, Options, Stop};
use tmux_mcp::{Safety, TmuxTools};

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
    let mut builder = Server::builder();
    if let Some(path) = options.socket_path {
        builder = builder.socket_path(path);
    }
    if let Some(name) = options.socket_name {
        builder = builder.socket_name(name);
    }
    let server = builder.build()?;

    let safety = options.safety.unwrap_or_else(Safety::from_env);
    let tools = TmuxTools::builder(server).safety(safety).build();

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
