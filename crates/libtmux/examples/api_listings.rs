//! List sessions, windows and panes, then read their hierarchy.

use std::error::Error;
use std::time::Duration;

use libtmux::{NewSessionOptions, NewWindowOptions, Server};

use libtmux::{
    WaitExt as _,
    driver::{AsyncIoRuntime, Runtime as _},
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
    session
        .new_window(NewWindowOptions::new("logs").command("cat"))
        .await?;
    let sessions = server.sessions().await?;
    let windows = server.windows().await?;
    let panes = server.panes().await?;
    check(sessions.len() == 1, "expected one session")?;
    check(windows.len() == 2, "expected two windows")?;
    check(panes.len() == 2, "expected two panes")?;
    check(
        session.windows().await?.len() == 2,
        "expected two session windows",
    )?;
    check(
        session.panes().await?.len() == 2,
        "expected two session panes",
    )?;
    for window in &windows {
        check(
            window.session_id() == session.id(),
            "window belongs to another session",
        )?;
        check(
            window.panes().await?.len() == 1,
            "expected one pane per window",
        )?;
    }
    let hierarchy = server.hierarchy().await?;
    let branch = hierarchy.first().ok_or("hierarchy has no session")?;
    check(
        branch.session.id() == session.id(),
        "hierarchy session differs",
    )?;
    check(branch.windows.len() == 2, "expected two hierarchy windows")?;
    check(
        branch.windows.iter().all(|window| window.panes.len() == 1),
        "hierarchy panes differ",
    )?;
    println!("listings: 1 session, 2 windows, 2 panes");
    Ok(())
}

fn main() -> Result<(), ExampleError> {
    run().wait()
}

async fn run() -> Result<(), ExampleError> {
    let root = std::path::Path::new("/tmp/libtmux-rs-dev");
    std::fs::create_dir_all(root)?;
    let directory = tempfile::Builder::new()
        .prefix("api-listings-")
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
