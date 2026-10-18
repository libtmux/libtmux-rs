//! Send literal text and a named Enter key to a ready shell.

use std::error::Error;
use std::time::Duration;

use libtmux::{NewSessionOptions, PaneWait, Server};

type ExampleError = Box<dyn Error>;

fn demonstrate(server: &Server) -> Result<(), ExampleError> {
    let session = server.new_session(
        NewSessionOptions::new("input").command("env ENV=/dev/null PS1='api-ready> ' sh"),
    )?;
    let panes = session.panes()?;
    let pane = panes.first().ok_or("session has no pane")?;
    if pane.wait_for_text("api-ready>", Duration::from_secs(5))? != PaneWait::Arrived {
        return Err("shell prompt did not become ready".into());
    }
    pane.send_keys("printf '\\nC-c is text\\n'")?;
    pane.send_key_names(["Enter"])?;
    loop {
        let lines = pane.capture()?;
        if lines.iter().any(|line| line.as_bytes() == b"C-c is text") {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("input: C-c is text");
    Ok(())
}

fn main() -> Result<(), ExampleError> {
    let root = std::path::Path::new("/tmp/libtmux-rs-dev");
    std::fs::create_dir_all(root)?;
    let directory = tempfile::Builder::new()
        .prefix("api-input-")
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
