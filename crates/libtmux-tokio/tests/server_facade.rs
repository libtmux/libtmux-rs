//! Native server forwarding and scoped callback contracts.

#![cfg(feature = "test-support")]

use libtmux_tokio::{Error, Server, test::TestServer};

#[tokio::test]
async fn associated_calls_and_scoped_callbacks_keep_the_native_server_type()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestServer::new().await?;
    let server = fixture.server();
    Server::new_session(server, "facade").await?;
    assert_eq!(Server::sessions(server).await?.len(), 1);

    let returned: Server = Server::with_channel_lock(server, "facade", async |held| {
        Ok::<Server, Error>(held.clone())
    })
    .await?;
    assert_eq!(returned.identity(), server.identity());
    returned.lock_channel("facade").await?;
    returned.unlock_channel("facade").await?;
    fixture.shutdown().await?;
    Ok(())
}
