//! Create a scoped session on the ordinary endpoint.
//!
//! ```console
//! $ cargo run --example default_session
//! ```

use libtmux::{Error, ScopeError, Server};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = Server::new()?;
    let name = format!("libtmux-example-{}", std::process::id());
    let outcome = server
        .with_session(name, async |session| {
            println!("created scoped session {}", session.id());
            println!("windows: {}", session.windows().await?.len());
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
