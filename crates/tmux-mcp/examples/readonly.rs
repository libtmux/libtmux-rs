//! Serve tmux over MCP, offering only the tools that change nothing.
//!
//! The shipped binary reads its tier from `--safety` or `TMUX_MCP_SAFETY`.
//! Building the server yourself is how you decide it in code instead, which is
//! what you want when the surface is a property of the program rather than of
//! how someone launched it.
//!
//! ```console
//! $ cargo run --example readonly
//! ```
//!
//! Then talk MCP to it on stdin and stdout.

use libtmux::Server;
use rmcp::ServiceExt as _;
use rmcp::transport::stdio;
use tmux_mcp::{Safety, TmuxTools};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tools = TmuxTools::builder(Server::new()?)
        .safety(Safety::ReadOnly)
        .build();

    // stdout carries the protocol, so this goes to stderr.
    eprintln!("serving {} read-only tools", tools.offered().len());

    tools.serve(stdio()).await?.waiting().await?;
    Ok(())
}
