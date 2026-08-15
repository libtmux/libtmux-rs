//! Find panes running a given command, without listing everything by hand.
//!
//! ```console
//! $ cargo run --example find -- nvim
//! ```

use libtmux::query::{Filterable as _, QueryIteratorExt as _};
use libtmux::{Pane, Server};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let wanted = std::env::args().nth(1).unwrap_or_else(|| "sh".to_owned());
    let server = Server::from_env().or_else(|_| Server::new())?;

    // A typed field handle: comparing this text field to a number would not
    // compile, so the only expressible predicates are ones tmux can answer.
    let fields = Pane::filter_fields();
    let running = fields.pane_current_command.eq(wanted.as_str());

    let panes = server.try_panes().await?;
    for pane in panes.iter().matching(&running) {
        println!("{} in window {}", pane.id(), pane.window_id());
    }

    // Cardinality is explicit: this distinguishes none from several rather
    // than silently taking the first.
    match panes.iter().matching(&running).exactly_one() {
        Ok(pane) => println!("exactly one: {}", pane.id()),
        Err(error) => println!("not exactly one: {error}"),
    }

    server.shutdown().await?;
    Ok(())
}
