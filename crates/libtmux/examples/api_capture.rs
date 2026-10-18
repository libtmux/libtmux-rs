//! Capture complete output lines after submitting a shell command.

use std::error::Error;
use std::time::Duration;

use libtmux::{NewSessionOptions, PaneWait, Server};

use libtmux::{
    WaitExt as _,
    driver::{AsyncIoRuntime, Runtime as _},
};

type ExampleError = Box<dyn Error>;

async fn demonstrate(server: &Server) -> Result<(), ExampleError> {
    let session = server
        .new_session(
            NewSessionOptions::new("capture").command("env ENV=/dev/null PS1='api-ready> ' sh"),
        )
        .await?;
    let panes = session.panes().await?;
    let pane = panes.first().ok_or("session has no pane")?;
    if pane
        .wait_for_text("api-ready>", Duration::from_secs(5))
        .await?
        != PaneWait::Arrived
    {
        return Err("shell prompt did not become ready".into());
    }
    pane.send_line("printf '\\nfirst line\\nsecond line\\n'")
        .await?;
    loop {
        let lines = pane.capture().await?;
        if lines.iter().any(|line| line.as_bytes() == b"first line")
            && lines.iter().any(|line| line.as_bytes() == b"second line")
        {
            break;
        }
        AsyncIoRuntime
            .sleep_until(std::time::Instant::now() + Duration::from_millis(20))
            .await;
    }
    println!("capture: first line, second line");
    Ok(())
}

fn main() -> Result<(), ExampleError> {
    run().wait()
}

async fn run() -> Result<(), ExampleError> {
    let root = std::path::Path::new("/tmp/libtmux-rs-dev");
    std::fs::create_dir_all(root)?;
    let directory = tempfile::Builder::new()
        .prefix("api-capture-")
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
    let outcome = within(Duration::from_secs(10), demonstrate(&server)).await;

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

async fn within<T>(budget: Duration, work: impl Future<Output = T>) -> Result<T, std::io::Error> {
    let mut work = std::pin::pin!(work);
    let mut timer = AsyncIoRuntime.sleep_until(std::time::Instant::now() + budget);
    std::future::poll_fn(|context| {
        if let std::task::Poll::Ready(result) = work.as_mut().poll(context) {
            return std::task::Poll::Ready(Ok(result));
        }
        timer.as_mut().poll(context).map(|()| {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "example deadline elapsed",
            ))
        })
    })
    .await
}
