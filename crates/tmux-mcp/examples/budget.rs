//! Measure what a client downloads before it can call anything.
//!
//! A client fetches every tool's schema at `tools/list`, and Claude Code stops
//! sending them to the model once they crowd the context. That budget is the
//! reason `alwaysLoad` exists here, and the reason adding a tool is a decision
//! rather than a habit -- so it is measured rather than guessed at.
//!
//! ```console
//! $ cargo run --example budget
//! ```

use tmux_mcp::{Selection, TmuxTools};

/// How many of the largest tools to name.
const WORST: usize = 8;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for toolsets in [
        "inspect",
        "inspect,manage,execute",
        "inspect,manage,execute,teardown",
    ] {
        // Nothing here talks to tmux: the surface is decided before any
        // command runs, so a server is needed only to build the tools.
        let tools = TmuxTools::builder(libtmux::Server::new()?)
            .selection(Selection::parse(Some(toolsets), None, None)?)
            .build();
        let offered = tools.offered();

        let whole = serde_json::to_string(&offered)?.len();
        let outputs: usize = offered
            .iter()
            .filter_map(|tool| tool.output_schema.as_ref())
            .filter_map(|schema| serde_json::to_string(schema).ok())
            .map(|schema| schema.len())
            .sum();

        println!(
            "{:<12} {:>3} tools  {:>6} B total, {:>6} B of it output schemas",
            toolsets,
            offered.len(),
            whole,
            outputs,
        );
    }

    let tools = TmuxTools::builder(libtmux::Server::new()?)
        .selection(Selection::parse(
            Some("inspect,manage,execute,teardown"),
            None,
            None,
        )?)
        .build();
    let mut sizes: Vec<_> = tools
        .offered()
        .iter()
        .map(|tool| {
            let bytes = serde_json::to_string(tool).map_or(0, |rendered| rendered.len());
            (bytes, tool.name.to_string())
        })
        .collect();
    sizes.sort_by_key(|(bytes, _)| std::cmp::Reverse(*bytes));

    println!("\nthe {WORST} tools that cost the most:");
    for (bytes, name) in sizes.iter().take(WORST) {
        println!("  {bytes:>6} B  {name}");
    }

    Ok(())
}
