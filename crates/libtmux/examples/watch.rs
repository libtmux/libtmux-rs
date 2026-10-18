//! Send commands while another thread watches the same tmux connection.
//!
//! ```console
//! $ cargo run -p libtmux --example watch --features control-mode,test-support
//! ```

#![allow(clippy::print_stdout, reason = "an example")]

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use libtmux::Command;
use libtmux::control::{ControlMode, Event};
use libtmux::test::TestServer;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = TestServer::new()?;
    let session = guard.session("watched")?;
    let (commands, mut events) = ControlMode::attach(guard.server(), session.id())?.split();
    println!("one connection to {}", guard.socket_path().display());

    let (finished, received) = mpsc::sync_channel(1);
    let watcher = thread::spawn(move || {
        let outcome = (|| {
            let mut seen = Vec::new();
            while let Some(event) = events.next_event_timeout(Duration::from_secs(10))? {
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
            Ok::<_, libtmux::Error>((seen, events))
        })();
        let _ = finished.send(outcome);
    });

    let outcome = (|| -> Result<_, Box<dyn std::error::Error>> {
        println!("  -> new-window");
        commands.send(Command::new("new-window").arg("-d").arg("-n").arg("built"))?;
        println!("  -> rename-window");
        commands.send(
            Command::new("rename-window")
                .arg("-t")
                .arg("built")
                .arg("renamed"),
        )?;
        let (seen, events) = received.recv_timeout(Duration::from_secs(10))??;
        events.shutdown()?;
        if seen.len() != 2 {
            return Err(std::io::Error::other("connection ended before the window events").into());
        }
        println!("{} events arrived on the same connection", seen.len());
        Ok(())
    })();

    drop(commands);
    let cleanup = guard.shutdown();
    if let Err(panic) = watcher.join() {
        std::panic::resume_unwind(panic);
    }
    outcome?;
    cleanup?;
    Ok(())
}
