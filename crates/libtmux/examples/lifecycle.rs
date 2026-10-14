//! Adopt resources, distinguish created/reused matches and discover sockets.
//!
//! The example uses the ordinary endpoint. Its external harness supplies
//! `LIBTMUX_SOCKET_PATH` to execute this unchanged source on a private daemon.

use libtmux::lifecycle::{Discovery, FindOrCreate, PaneIdentity};
use libtmux::{Error, ScopeError, Server, SplitDirection, SplitOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = Server::new()?;
    let name = format!("lifecycle-example-{}", std::process::id());
    let outcome = Box::pin(server
        .with_session(name.clone(), async |session| {
            let reused = server.find_or_create_session(name.clone()).await?;
            assert!(matches!(reused, FindOrCreate::Reused(_)));
            let window = session.new_window("adopt-existing").await?;
            let window_id = window.id().clone();
            let adopted = window.adopt().await?;
            adopted
                .scope(async |window| {
                    let identity = PaneIdentity::new("@example-role", "worker")?;
                    let options = SplitOptions::new(SplitDirection::Below).command("sleep 30");
                    let created = window
                        .find_or_create_pane(identity.clone(), options.clone())
                        .await?;
                    let reused = window.find_or_create_pane(identity, options).await?;
                    assert!(created.is_created());
                    assert!(matches!(reused, FindOrCreate::Reused(_)));
                    assert_eq!(created.resource().id(), reused.resource().id());
                    if let FindOrCreate::Created(owner) = created {
                        owner.close().await?;
                        owner.close().await?;
                    }
                    Ok::<(), Error>(())
                })
                .await?;
            assert!(server.window_by_id(&window_id).await?.is_none());
            let root = server.socket_path().parent().ok_or(Error::LifecycleInput {
                reason: "the selected socket has no parent directory",
            })?;
            let discovery = Discovery::new([root.to_owned()]).scan().await;
            assert!(discovery
                .servers
                .iter()
                .any(|found| found.server.socket_path() == server.socket_path()));
            println!(
                "created and reused a pane; adopted window killed; discovered {} daemon(s), {} diagnostics, truncation {:?}",
                discovery.servers.len(), discovery.diagnostics.len(), discovery.truncated
            );
            Ok::<(), Box<dyn std::error::Error>>(())
        }))
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
