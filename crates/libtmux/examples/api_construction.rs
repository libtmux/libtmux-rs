//! Construct a server handle, then start its private daemon.

use std::error::Error;
use std::time::Duration;

use libtmux::{CapabilitySupport, NewSessionOptions, Server};

type ExampleError = Box<dyn Error>;

fn check(condition: bool, message: &str) -> Result<(), ExampleError> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

async fn demonstrate(server: &Server) -> Result<(), ExampleError> {
    check(!server.is_alive().await, "construction started a daemon")?;
    check(
        server.daemon_capabilities().await?.is_none(),
        "discovery reported an absent daemon as available",
    )?;
    check(
        server.config_file() == Some(std::path::Path::new("/dev/null")),
        "wrong config path",
    )?;
    let session = server
        .new_session(NewSessionOptions::new("work").command("cat"))
        .await?;
    check(
        server.is_alive().await,
        "new_session did not start a daemon",
    )?;
    check(session.name().as_bytes() == b"work", "wrong session name")?;
    let report = server
        .daemon_capabilities()
        .await?
        .ok_or("the owned daemon disappeared")?;
    let deletion = report
        .feature("buffer-delete-named")
        .ok_or("the feature is not in the registry")?;
    server
        .set_buffer(Some("capability-example"), "payload")
        .await?;
    if deletion.support == CapabilitySupport::Available {
        server.delete_buffer("capability-example").await?;
        check(
            server.buffer("capability-example").await?.is_none(),
            "named buffer deletion had no effect",
        )?;
    } else {
        check(
            server.buffer("capability-example").await? == Some(b"payload".to_vec()),
            "the unavailable operation changed the buffer",
        )?;
    }
    println!("construction: private daemon started by new_session");
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), ExampleError> {
    let root = std::path::Path::new("/tmp/libtmux-rs-dev");
    std::fs::create_dir_all(root)?;
    let directory = tempfile::Builder::new()
        .prefix("api-construction-")
        .tempdir_in(root)?;
    std::fs::write(
        directory.path().join("owner"),
        std::process::id().to_string(),
    )?;
    let server = Server::builder()
        .socket_path(directory.path().join("tmux.sock"))
        .config_file("/dev/null")
        .default_timeout(Duration::from_secs(5))
        .build()?;
    let outcome = tokio::time::timeout(Duration::from_secs(10), demonstrate(&server)).await;

    // Stop the owned daemon before closing the client executor.
    let killed = server.kill().await;
    let closed = server.shutdown().await;
    let cleanup_failed = killed.is_err() || closed.is_err();
    let mut failures = Vec::new();
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(error)) => failures.push(format!("example failed: {error}")),
        Err(error) => failures.push(format!("example deadline: {error}")),
    }
    if let Err(error) = killed {
        failures.push(format!("daemon cleanup: {error}"));
    }
    if let Err(error) = closed {
        failures.push(format!("executor cleanup: {error}"));
    }
    if cleanup_failed {
        let retained = directory.keep();
        failures.push(format!("inspect retained directory {}", retained.display()));
    } else if let Err(error) = directory.close() {
        failures.push(format!("directory cleanup: {error}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; ").into())
    }
}
