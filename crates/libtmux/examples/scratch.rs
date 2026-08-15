//! Build a throwaway session, use it, and leave nothing behind.
//!
//! ```console
//! $ cargo run --example scratch
//! ```

use std::time::Duration;

use libtmux::test::{retry_until, unique_name};
use libtmux::{NewWindowOptions, Server, SplitDirection, SplitOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // An example must not build sessions on whatever server the reader
    // happens to be using, so this one gets a socket of its own.
    let socket = std::env::temp_dir().join(format!("{}.sock", unique_name("libtmux-scratch")));
    let server = Server::builder().socket_path(&socket).build()?;

    // The scope kills the session whether the body succeeds or fails, so a
    // failure partway through does not leave a session behind.
    let output = server
        .with_session(unique_name("scratch").as_str(), async |session| {
            let window = session
                .new_window(NewWindowOptions::new("work").command("sh"))
                .await?;
            window
                .split(SplitOptions::new(SplitDirection::Below).command("sh"))
                .await?;

            // A window always has an active pane, but saying so with a panic
            // would be a worse example than handling it.
            let Some(pane) = window.active_pane().await? else {
                return Ok(0);
            };
            pane.send_keys("printf 'hello from tmux\\n'").await?;
            pane.send_key_names(["Enter"]).await?;

            // tmux runs the shell asynchronously, so wait for the output
            // rather than sleeping and hoping.
            retry_until(Duration::from_secs(5), async || {
                pane.capture().await.is_ok_and(|lines| {
                    lines
                        .iter()
                        .any(|line| line.as_bytes().windows(5).any(|window| window == b"hello"))
                })
            })
            .await?;

            Ok::<_, Box<dyn std::error::Error>>(pane.capture().await?.len())
        })
        .await?;

    println!("captured {output} lines");
    assert!(
        server.try_sessions().await?.is_empty(),
        "the scope cleaned up"
    );

    server.shutdown().await?;
    Ok(())
}
