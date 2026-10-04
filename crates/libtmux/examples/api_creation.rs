//! Create a session and window, then split a window and a pane.

use std::error::Error;
use std::time::Duration;

use libtmux::{
    NewSessionOptions, NewWindowOptions, Server, SplitDirection, SplitOptions,
};

type ExampleError = Box<dyn Error>;

fn check(condition: bool, message: &str) -> Result<(), ExampleError> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

async fn demonstrate(server: &Server) -> Result<(), ExampleError> {
    let session = server
        .new_session(
            NewSessionOptions::new("work")
                .window_name("editor")
                .command("cat"),
        )
        .await?;
    let window = session
        .new_window(NewWindowOptions::new("logs").command("cat"))
        .await?;
    let pane = window
        .split(SplitOptions::new(SplitDirection::Below).command("cat"))
        .await?;
    pane.split(SplitOptions::new(SplitDirection::Right).command("cat"))
        .await?;
    check(
        session.windows().await?.len() == 2,
        "expected two session windows",
    )?;
    check(
        window.panes().await?.len() == 3,
        "expected three panes in split window",
    )?;
    check(
        server.panes().await?.len() == 4,
        "expected four panes on server",
    )?;
    println!("creation: 2 windows, 4 panes");
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), ExampleError> {
    let root = std::path::Path::new("/tmp/libtmux-rs-dev");
    std::fs::create_dir_all(root)?;
    let directory = tempfile::Builder::new()
        .prefix("api-creation-")
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
    let limit = Duration::from_secs(10);
    let outcome = tokio::time::timeout(limit, demonstrate(&server)).await;

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
        let path = retained.display();
        failures.push(format!("inspect retained directory {path}"));
    } else if let Err(error) = directory.close() {
        failures.push(format!("directory cleanup: {error}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; ").into())
    }
}
