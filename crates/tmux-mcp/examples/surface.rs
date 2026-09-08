//! Print what this server offers, without running one.
//!
//! Useful for seeing what a client will be shown for each toolset selection,
//! including each tool's controlled description and answer shape.
//!
//! ```console
//! $ cargo run --example surface
//! $ cargo run --example surface -- inspect,execute
//! $ cargo run --example surface -- '' call_read_tools_batch show_environment
//! ```
//!
//! Positional arguments are `toolsets`, `included tools`, and `excluded
//! tools`. Each is the same comma-separated value accepted by the startup
//! environment. The empty first argument selects the zero-toolset subset.

use tmux_mcp::{Selection, TmuxTools};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let requested = arguments
        .first()
        .map_or("inspect,manage,execute,teardown", String::as_str);
    let included = arguments.get(1).map(String::as_str);
    let excluded = arguments.get(2).map(String::as_str);
    let selection = Selection::parse(Some(requested), included, excluded)?;

    // A server is needed to build the tools, but nothing here talks to tmux:
    // the surface is decided before any command runs.
    let tools = TmuxTools::builder(libtmux::Server::new()?)
        .selection(selection)
        .build();

    let offered = tools.offered();
    println!(
        "{} tools for toolsets={requested:?} include={included:?} exclude={excluded:?}\n",
        offered.len()
    );

    for tool in &offered {
        let capability = tool
            .meta
            .as_ref()
            .and_then(|meta| meta.0.get("com.git-pull.libtmux-mcp/capability"))
            .ok_or_else(|| std::io::Error::other("offered tool has no capability row"))?;
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

        println!("{:<22} {}", tool.name, tool.title.as_deref().unwrap_or(""));
        if let Some(description) = &tool.description {
            println!("{:<22} {description}", "");
        }
        if !answers.is_empty() {
            println!("{:<22} answers with: {answers}", "");
        }
        println!(
            "{:<22} toolset={} reach={} effects={} outputs={}",
            "",
            capability["toolset"],
            capability["processReach"],
            capability["tmuxEffects"],
            capability["outputClasses"],
        );
        let nested = &capability["nestedAuthority"];
        if nested.as_array().is_some_and(|names| !names.is_empty()) {
            println!("{:<22} nested authority: {nested}", "");
        }
    }

    Ok(())
}
