//! Send commands while another task watches the same tmux connection.
//!
//! ```console
//! $ cargo run -p libtmux-tokio --example watch --features control-mode,test-support
//! ```

#![allow(clippy::print_stdout, reason = "an example")]

use std::time::Duration;

use libtmux_tokio::Command;
use libtmux_tokio::control::{ControlMode, Event};
use libtmux_tokio::test::TestServer;
use tokio::sync::oneshot;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new().await?;
    let session = guard.session("watched").await?;
    let (commands, mut events) = ControlMode::attach(guard.server(), session.id())
        .await?
        .split();
    println!("one connection to {}", guard.socket_path().display());

    let (finished, received) = oneshot::channel();
    let watcher = tokio::spawn(async move {
        let outcome = async {
            let mut seen = Vec::new();
            while let Some(event) = events.next_event_timeout(Duration::from_secs(10)).await? {
                match event {
                    Event::WindowAdded { window } => {
                        println!("  <- window {window} appeared");
                        seen.push(window.to_string());
                    }
                    Event::WindowRenamed { window, name } if name.as_bytes() == b"renamed" => {
                        println!("  <- window {window} is now {}", name.to_string_lossy());
                        seen.push(window.to_string());
                    }
                    Event::Exit { .. } => break,
                    _ => continue,
                }
                if seen.len() == 2 {
                    break;
                }
            }
            Ok::<_, libtmux_tokio::Error>((seen, events))
        }
        .await;
        let _ = finished.send(outcome);
    });

    let outcome: Result<(), Box<dyn std::error::Error>> = async {
        println!("  -> new-window");
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
        let (seen, events) = tokio::time::timeout(Duration::from_secs(10), received).await???;
        events.shutdown().await?;
        if seen.len() != 2 {
            return Err(std::io::Error::other("connection ended before the window events").into());
        }
        println!("{} events arrived on the same connection", seen.len());
        Ok(())
    }
    .await;

    drop(commands);
    let cleanup = guard.shutdown().await;
    if let Err(error) = watcher.await {
        if error.is_panic() {
            std::panic::resume_unwind(error.into_panic());
        }
        return Err(error.into());
    }
    outcome?;
    cleanup?;
    Ok(())
}
