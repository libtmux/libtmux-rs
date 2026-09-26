//! Follow a pane across calls without missing what scrolled past.
//!
//! A screen capture sees the rows on screen now; anything that scrolled off
//! between two looks is gone from it. `capture_since` hands back a cursor and,
//! on the next call, everything the pane wrote after it. Here 200 lines go
//! through a 24-row pane between two calls, and all 200 come back.
//!
//! ```console
//! $ cargo run --example follow
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

    tools
        .create_session(args(json!({"name": "follow"}))?)
        .await?;
    let pane = tools.list_panes().await?.0.panes.remove(0).id;

    // The first call starts watching and returns the screen and a cursor.
    let start = tools.capture_since(args(json!({"pane": pane}))?).await?.0;

    tools
        .send_keys(args(json!({
            "pane": pane,
            "text": "for i in $(seq 1 200); do echo line-$i; done",
            "enter": true
        }))?)
        .await?;
    tools
        .wait_for_text(
            args(json!({"pane": pane, "patterns": ["line-200"], "seconds": 10}))?,
            CancellationToken::new(),
            Reporter::none(),
        )
        .await?;

    // The next call, with that cursor, returns everything written since.
    let since = tools
        .capture_since(args(json!({"pane": pane, "cursor": start.cursor}))?)
        .await?
        .0;
    let lines = since
        .text
        .lines()
        .filter(|line| line.starts_with("line-"))
        .count();
    println!("saw {lines} of 200 lines, missed: {}", since.missed);

    guard.shutdown().await?;
    Ok(())
}
