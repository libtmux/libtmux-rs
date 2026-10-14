//! Adopt an existing session selected by name on the ordinary endpoint.
//!
//! ```console
//! $ cargo run --example adopt_resources -- disposable-session
//! ```

use libtmux::{Error, ScopeError, Server, SplitDirection, SplitOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let name = std::env::args().nth(1).ok_or(Error::LifecycleInput {
        reason: "supply the name of an existing disposable session to adopt",
    })?;
    let server = Server::new()?;
    let session = server
        .session(name.as_bytes())
        .await?
        .ok_or(Error::LifecycleInput {
            reason: "the named session does not exist",
        })?;
    let owner = session.adopt().await?;
    let outcome = owner
        .scope(async |session| {
            let window = session.new_window("adopt-existing-window").await?;
            let owner = window.adopt().await?;
            owner
                .scope(async |window| {
                    let pane = window
                        .split(SplitOptions::new(SplitDirection::Below).command("sleep 30"))
                        .await?;
                    let owner = pane.adopt().await?;
                    owner
                        .scope(async |pane| {
                            println!("adopted session, window and pane {}", pane.id());
                            Ok::<(), Error>(())
                        })
                        .await?;
                    Ok::<(), Box<dyn std::error::Error>>(())
                })
                .await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await;
    let shutdown = server.shutdown().await;
    match (outcome, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(operation), Ok(())) => Err(operation.into()),
        (Ok(()), Err(cleanup)) => Err(cleanup.into()),
        (Err(operation), Err(cleanup)) => {
            Err(ScopeError::<(), _>::OperationAndCleanup { operation, cleanup }.into())
        }
    }
}
