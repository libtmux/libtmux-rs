//! Filter native snapshots with typed field expressions.

use std::error::Error;
use std::time::Duration;

use libtmux::query::{Filterable as _, QueryIteratorExt as _};
use libtmux::{NewSessionOptions, Server, Session};

type ExampleError = Box<dyn Error>;

fn check(condition: bool, message: &str) -> Result<(), ExampleError> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

async fn demonstrate(server: &Server) -> Result<(), ExampleError> {
    server
        .new_session(NewSessionOptions::new("prod-api").command("cat"))
        .await?;
    server
        .new_session(NewSessionOptions::new("dev-api").command("cat"))
        .await?;
    let sessions = server.sessions().await?;
    let fields = Session::filter_fields();
    let production = fields.session_name.starts_with("prod-");
    let selected = sessions.iter().matching(&production).exactly_one()?;
    check(
        selected.name().as_bytes() == b"prod-api",
        "wrong filtered session",
    )?;
    check(sessions.len() == 2, "filtering changed the source snapshot")?;
    println!("query: prod-api");
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), ExampleError> {
    let root = std::path::Path::new("/tmp/libtmux-rs-dev");
    std::fs::create_dir_all(root)?;
    let directory = tempfile::Builder::new()
        .prefix("api-query-")
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
    let outcome =
        tokio::time::timeout(Duration::from_secs(10), demonstrate(&server))
            .await;

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
        failures
            .push(format!("inspect retained directory {}", retained.display()));
    } else if let Err(error) = directory.close() {
        failures.push(format!("directory cleanup: {error}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; ").into())
    }
}
