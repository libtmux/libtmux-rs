//! Report what a tmux server is running.
//!
//! ```console
//! $ cargo run --example inspect
//! ```
//!
//! Reads the default server, or the one named by `$TMUX` when run inside a
//! pane. Changes nothing.

use libtmux::{Server, TmuxText};

fn show(value: &TmuxText) -> String {
    value.to_string_lossy().into_owned()
}

/// The same, for a field tmux may genuinely not report.
fn show_optional(value: Option<&TmuxText>) -> String {
    value.map_or_else(|| "-".to_owned(), show)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Inside a pane, `$TMUX` names the server this process belongs to.
    // Outside one, fall back to the default socket.
    let server = Server::from_env().or_else(|_| Server::new())?;

    if !server.is_alive().await {
        println!("no tmux server at {}", server.socket_path().display());
        return Ok(());
    }

    // Three tmux commands, not one per object: walking down would cost a
    // command per session and per window.
    for branch in server.hierarchy().await? {
        let session = &branch.session;
        println!(
            "{session} {} ({} windows{})",
            show(session.name()),
            session.window_count(),
            if session.is_attached() {
                ", attached"
            } else {
                ""
            },
        );

        for built in &branch.windows {
            let window = &built.window;
            println!(
                "  {window} {}{}",
                show(window.name()),
                if window.is_active() { " *" } else { "" },
            );

            for pane in &built.panes {
                println!(
                    "    {pane} {} in {}",
                    show_optional(pane.current_command()),
                    show_optional(pane.current_path()),
                );
            }
        }
    }

    server.shutdown().await?;
    Ok(())
}
