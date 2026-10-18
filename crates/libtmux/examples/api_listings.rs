//! List sessions, windows and panes, then read their hierarchy.

use std::error::Error;
use std::time::Duration;

use libtmux::{NewSessionOptions, NewWindowOptions, Server};

type ExampleError = Box<dyn Error>;

fn check(condition: bool, message: &str) -> Result<(), ExampleError> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn demonstrate(server: &Server) -> Result<(), ExampleError> {
    let session = server.new_session(
        NewSessionOptions::new("work")
            .window_name("editor")
            .command("cat"),
    )?;
    session.new_window(NewWindowOptions::new("logs").command("cat"))?;
    let sessions = server.sessions()?;
    let windows = server.windows()?;
    let panes = server.panes()?;
    check(sessions.len() == 1, "expected one session")?;
    check(windows.len() == 2, "expected two windows")?;
    check(panes.len() == 2, "expected two panes")?;
    check(
        session.windows()?.len() == 2,
        "expected two session windows",
    )?;
    check(session.panes()?.len() == 2, "expected two session panes")?;
    for window in &windows {
        check(
            window.session_id() == session.id(),
            "window belongs to another session",
        )?;
        check(window.panes()?.len() == 1, "expected one pane per window")?;
    }
    let hierarchy = server.hierarchy()?;
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
    let root = std::path::Path::new("/tmp/libtmux-rs-dev");
    std::fs::create_dir_all(root)?;
    let directory = tempfile::Builder::new()
        .prefix("api-listings-")
        .tempdir_in(root)?;
    std::fs::write(
        directory.path().join("owner"),
        std::process::id().to_string(),
    )?;
    let cancellation = libtmux::CancellationToken::new();
    let server = Server::builder()
        .socket_path(directory.path().join("tmux.sock"))
        .config_file("/dev/null")
        .default_timeout(Duration::from_secs(5))
        .cancellation_token(cancellation.clone())
        .build()?;
    let cleanup = Server::builder()
        .socket_path(directory.path().join("tmux.sock"))
        .config_file("/dev/null")
        .default_timeout(Duration::from_secs(5))
        .build()?;
    let outcome = with_deadline(cancellation, Duration::from_secs(10), || {
        demonstrate(&server)
    });

    // Stop the owned daemon before closing the client executor.
    let killed = cleanup.kill();
    let closed = server.shutdown().and_then(|()| cleanup.shutdown());
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

fn with_deadline<T>(
    token: libtmux::CancellationToken,
    within: Duration,
    operation: impl FnOnce() -> T,
) -> Result<T, &'static str> {
    std::thread::scope(|threads| {
        let (finished, completion) = std::sync::mpsc::channel::<()>();
        let timer = threads.spawn(move || {
            if matches!(
                completion.recv_timeout(within),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                token.cancel();
                true
            } else {
                false
            }
        });
        let result = operation();
        drop(finished);
        let elapsed = match timer.join() {
            Ok(elapsed) => elapsed,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        if elapsed {
            Err("example deadline elapsed")
        } else {
            Ok(result)
        }
    })
}
