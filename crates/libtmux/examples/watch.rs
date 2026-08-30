//! React to what a tmux server does, as it does it.
//!
//! ```console
//! $ cargo run --example watch --features control-mode,test-support
//! ```
//!
//! Every other API in this crate spawns a tmux process per command. Control
//! mode opens one connection and keeps it: commands go down it and tmux
//! reports what happens on the server as it happens.

#![allow(clippy::print_stdout, reason = "an example")]

use std::time::Duration;

use libtmux::control::{ControlMode, Event};
use libtmux::test::unique_name;
use libtmux::{Command, Server};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // An example must not watch whatever server the reader happens to be
    // using, so this one gets a socket of its own under a root this workspace
    // owns.
    let root = std::path::Path::new("/tmp/libtmux-rs-dev");
    std::fs::create_dir_all(root)?;
    let socket = root.join(format!("{}.sock", unique_name("libtmux-watch")));
    let server = Server::builder().socket_path(&socket).build()?;

    let session = server.new_session(unique_name("watched").as_str()).await?;
    let (commands, mut events) = ControlMode::attach(&server, session.id()).await?.split();
    println!("one connection to {}", socket.display());

    // The watcher gets its own task. Sending and watching are separate
    // handles precisely so neither has to wait for the other, and a loop that
    // only reads is the shape that keeps the connection moving.
    let watcher = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(event) = events.next_event().await {
            match event {
                Event::WindowAdded { window } => {
                    println!("  <- window {window} appeared");
                    seen.push(window.to_string());
                }
                Event::WindowRenamed { window, name } => {
                    println!("  <- window {window} is now {}", name.to_string_lossy());
                    seen.push(window.to_string());
                }
                Event::Exit { .. } => break,
                // tmux reports far more than this; an event nobody matched is
                // still worth counting rather than dropping silently.
                _ => continue,
            }
            if seen.len() == 2 {
                break;
            }
        }
        (seen, events)
    });

    // Meanwhile, drive the server down the same connection. These spawn no
    // processes: they are lines on the socket the watcher is reading.
    println!("  -> new-window   (a line on that connection, not a process)");
    commands
        .send(Command::new("new-window").arg("-d").arg("-n").arg("built"))
        .await?;
    println!("  -> rename-window");
    commands
        .send(
            Command::new("rename-window")
                .arg("-t")
                .arg("built")
                .arg("renamed"),
        )
        .await?;

    let (seen, events) = tokio::time::timeout(Duration::from_secs(10), watcher).await??;
    println!(
        "{} events arrived while those commands were being sent, on the same socket",
        seen.len()
    );

    drop(commands);
    events.shutdown().await?;
    server.kill().await?;
    server.shutdown().await?;

    // tmux does not unlink its socket when the server exits, so whatever named
    // one owns removing it.
    std::fs::remove_file(&socket)?;
    Ok(())
}
