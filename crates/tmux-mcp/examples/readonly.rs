//! Serve tmux over MCP, offering only the inspect toolset.
//!
//! The shipped binary reads `LIBTMUX_TOOLSETS` at startup. Building the server
//! yourself is how you decide it in code instead, which is
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
use tmux_mcp::{Selection, TmuxTools};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tools = TmuxTools::builder(Server::new()?)
        .selection(Selection::parse(Some("inspect"), None, None)?)
        .build();

    // stdout carries the protocol, so this goes to stderr.
    eprintln!("serving {} inspect tools", tools.offered().len());

    tools.serve(stdio()).await?.waiting().await?;
    Ok(())
}
