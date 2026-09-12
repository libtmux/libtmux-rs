//! Find panes running a given command, without listing everything by hand.
//!
//! ```console
//! $ cargo run --example find -- nvim
//! ```

use libtmux::Pane;
use libtmux::query::{Filterable as _, QueryIteratorExt as _};

#[path = "common/arena.rs"]
mod arena;

use arena::{ArenaEnvironment, arena_error, arena_evidence, select_server};

const ARENA_ARTIFACT: &str = "rust-find";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let wanted = std::env::args().nth(1).unwrap_or_else(|| "sh".to_owned());
    let environment = ArenaEnvironment::capture();
    let (server, arena) = select_server(&environment, ARENA_ARTIFACT)?;
    if arena.is_some() && !server.is_alive().await {
        server.shutdown().await?;
        return Err(arena_error("arena server is not alive").into());
    }

    // A typed field handle: comparing this text field to a number would not
    // compile, so the only expressible predicates are ones tmux can answer.
    let fields = Pane::filter_fields();
    let running = fields.pane_current_command.eq(wanted.as_str());

    let panes = server.panes().await?;
    let found = panes.iter().matching(&running).count();
    for pane in panes.iter().matching(&running) {
        println!("{} in window {}", pane.id(), pane.window_id());
    }

    // Cardinality is explicit: this distinguishes none from several rather
    // than silently taking the first.
    match panes.iter().matching(&running).exactly_one() {
        Ok(pane) => println!("exactly one: {}", pane.id()),
        Err(error) => println!("not exactly one: {error}"),
    }

    // A search that matches nothing is the likeliest first run, so say what to
    // search for instead of leaving the reader with a cardinality error.
    if found == 0 {
        println!("\nnothing here is running {wanted:?}. This server is running:");
        let mut commands: Vec<_> = panes
            .iter()
            .filter_map(|pane| pane.current_command())
            .map(|command| command.to_string_lossy().into_owned())
            .collect();
        commands.sort();
        commands.dedup();
        for command in &commands {
            println!("  {command}");
        }
        if let Some(command) = commands.first() {
            println!("\n  cargo run --example find --features query -- {command}");
        }
    }

    let evidence = if let Some(arena) = &arena {
        Some(arena_evidence(&server, arena, ARENA_ARTIFACT).await?)
    } else {
        None
    };
    server.shutdown().await?;
    if let Some(evidence) = evidence {
        println!("LIBTMUX_ARENA_EVIDENCE={evidence}");
    }
    Ok(())
}
