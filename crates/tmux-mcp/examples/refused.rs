//! What a client is told when it calls a tool this server left out.
//!
//! The server offers only the `inspect` toolset here, so `send_keys` is not
//! on its list. A client that calls it anyway -- one that cached an older
//! list, or guessed -- gets a refusal naming why and how an operator would
//! add it, not a silent failure. Both ends talk real MCP over an in-memory
//! pipe, against an isolated tmux under `/tmp/libtmux-rs-test/`.
//!
//! ```console
//! $ cargo run --example refused
//! ```

use libtmux::test::TestServer;
use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use serde_json::json;
use tmux_mcp::{Selection, TmuxTools};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    guard.session("watched").await?;
    let tools = TmuxTools::builder(guard.server().clone())
        .caller(None)
        .selection(Selection::parse(Some("inspect"), None, None)?)
        .build();

    let (client_side, server_side) = tokio::io::duplex(1 << 20);
    let server = tokio::spawn(async move {
        if let Ok(service) = tools.serve(server_side).await {
            let _ = service.waiting().await;
        }
    });
    let client = ().serve(client_side).await?;

    let offered = client.list_all_tools().await?;
    let has_send_keys = offered.iter().any(|tool| tool.name == "send_keys");
    println!(
        "offered {} tools, send_keys among them: {has_send_keys}",
        offered.len()
    );

    let request = CallToolRequestParams::new("send_keys").with_arguments(
        json!({"pane": "%0", "text": "echo hi", "enter": true})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    );
    match client.call_tool(request).await {
        Ok(result) if result.is_error == Some(true) => {
            let text = result
                .content
                .iter()
                .filter_map(|content| content.as_text().map(|text| text.text.clone()))
                .collect::<Vec<_>>()
                .join(" ");
            println!("send_keys refused: {text}");
        }
        Ok(_) => println!("send_keys ran, which it should not have"),
        Err(error) => println!("send_keys refused: {error}"),
    }

    client.cancel().await?;
    let _ = server.await;
    guard.shutdown().await?;
    Ok(())
}
