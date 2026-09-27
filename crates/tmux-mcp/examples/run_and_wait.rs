//! The two things an agent asks a terminal most: run this and tell me whether
//! it worked, and tell me when that server says it is ready.
//!
//! Each call goes through the same tool an MCP client calls, with arguments
//! built from JSON as the protocol delivers them, against an isolated tmux
//! under `/tmp/libtmux-rs-test/` that is removed at the end.
//!
//! ```console
//! $ cargo run --example run_and_wait
//! ```

use libtmux::test::TestServer;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::json;
use tmux_mcp::{Reporter, Selection, TmuxTools};
use tokio_util::sync::CancellationToken;

fn args<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
) -> serde_json::Result<Parameters<T>> {
    serde_json::from_value(value).map(Parameters)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let tools = TmuxTools::builder(guard.server().clone())
        // Not the caller's own tmux: this example has none to protect.
        .caller(None)
        .selection(Selection::parse(Some("inspect,manage,execute"), None, None)?)
        .build();

    tools.create_session(args(json!({"name": "demo"}))?).await?;
    let pane = tools.list_panes().await?.0.panes.remove(0).id;

    // A real exit status, not a guess from the text on screen.
    let run = tools
        .run_command(
            args(json!({"pane": pane, "command": "ls /no/such/path", "seconds": 10}))?,
            CancellationToken::new(),
            Reporter::none(),
        )
        .await?
        .0;
    println!("ls exited {:?}: {}", run.exit_status, run.output.trim());

    // Start something that reports readiness later, then wait for that line
    // rather than polling the screen for it.
    tools
        .send_keys(args(json!({
            "pane": pane,
            "text": "(sleep 1; echo server is ready) &",
            "enter": true
        }))?)
        .await?;
    let wait = tools
        .wait_for_text(
            args(json!({"pane": pane, "patterns": ["server is ready"], "seconds": 10}))?,
            CancellationToken::new(),
            Reporter::none(),
        )
        .await?
        .0;
    println!("waited: {:?} on {:?}", wait.outcome, wait.matched_pattern);

    guard.shutdown().await?;
    Ok(())
}
