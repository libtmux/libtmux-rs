//! Print what this server offers, without running one.
//!
//! Useful for seeing what a client will be shown at each tier, and what an
//! agent reads before it chooses: the title, what the tool does to the server,
//! and the shape of its answer.
//!
//! ```console
//! $ cargo run --example surface
//! $ cargo run --example surface -- destructive
//! ```

use tmux_mcp::{Safety, TmuxTools};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tier = std::env::args()
        .nth(1)
        .as_deref()
        .and_then(Safety::parse)
        .unwrap_or_default();

    // A server is needed to build the tools, but nothing here talks to tmux:
    // the surface is decided before any command runs.
    let tools = TmuxTools::builder(libtmux::Server::new()?)
        .safety(tier)
        .build();

    let offered = tools.offered();
    println!("{} tools at the {} tier\n", offered.len(), tier.name());

    for tool in &offered {
        let hints = tool.annotations.as_ref();
        let does = if hints.and_then(|h| h.read_only_hint) == Some(true) {
            "reads"
        } else if hints.and_then(|h| h.destructive_hint) == Some(true) {
            "DESTROYS"
        } else if hints.and_then(|h| h.open_world_hint) == Some(true) {
            "runs what you give it"
        } else {
            "changes"
        };
        let answers = tool
            .output_schema
            .as_ref()
            .and_then(|schema| serde_json::to_value(schema).ok())
            .and_then(|schema| {
                schema
                    .get("properties")
                    .and_then(|fields| fields.as_object())
                    .map(|fields| fields.keys().cloned().collect::<Vec<_>>().join(", "))
            })
            .unwrap_or_default();

        println!(
            "{:<22} {:<22} {}",
            tool.name,
            does,
            tool.title.as_deref().unwrap_or("")
        );
        if !answers.is_empty() {
            println!("{:<22} answers with: {answers}", "");
        }
    }

    Ok(())
}
