//! Kill an existing daemon at an explicitly supplied disposable socket.
//!
//! ```console
//! $ cargo run --example adopt_server -- /absolute/disposable/socket
//! ```

use libtmux::{Error, ScopeError, Server};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = std::env::args_os().nth(1).ok_or(Error::LifecycleInput {
        reason: "supply an absolute disposable socket to adopt and kill",
    })?;
    let server = Server::builder().socket_path(socket).build()?;
    let owner = server.adopt().await?;
    let outcome = owner
        .scope(async |server| {
            println!(
                "accepted daemon {}; sessions {}",
                owner.generation(),
                server.sessions().await?.len()
            );
            Ok::<(), Error>(())
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
