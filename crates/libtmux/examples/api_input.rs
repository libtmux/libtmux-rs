//! Send literal text and a named Enter key to a ready shell.

use std::error::Error;
use std::time::Duration;

use libtmux::{NewSessionOptions, PaneWait, Server};

type ExampleError = Box<dyn Error>;

async fn demonstrate(server: &Server) -> Result<(), ExampleError> {
    let shell = "env ENV=/dev/null PS1='api-ready> ' sh";
    let options = NewSessionOptions::new("input").command(shell);
    let session = server.new_session(options).await?;
    let panes = session.panes().await?;
    let pane = panes.first().ok_or("session has no pane")?;
    if pane
        .wait_for_text("api-ready>", Duration::from_secs(5))
        .await?
        != PaneWait::Arrived
    {
        return Err("shell prompt did not become ready".into());
    }
    pane.send_keys("printf '\\nC-c is text\\n'").await?;
    pane.send_key_names(["Enter"]).await?;
    loop {
        let lines = pane.capture().await?;
        if lines.iter().any(|line| line.as_bytes() == b"C-c is text") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    println!("input: C-c is text");
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), ExampleError> {
    let root = std::path::Path::new("/tmp/libtmux-rs-dev");
    std::fs::create_dir_all(root)?;
    let directory = tempfile::Builder::new()
        .prefix("api-input-")
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
