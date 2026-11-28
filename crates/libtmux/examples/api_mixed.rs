//! Create a session synchronously and capture its first pane with Tokio.

use std::error::Error;
use std::io::{self, Write};
use std::time::Duration;

use libtmux::tokio::{PaneWait, test::TestServer};
use libtmux::{NewSession, blocking};
use tokio::runtime::{Builder, Runtime};

fn capture(guard: &TestServer, runtime: &Runtime) -> Result<Vec<u8>, Box<dyn Error>> {
    let server = guard.server();
    let blocking =
        blocking::Server::at(guard.socket_path())?.with_executable(server.tmux_executable());
    let session = blocking.new_session(
        NewSession::new("work")
            .size(40, 1)
            .command("printf 'mixed runtime'; exec cat"),
    )?;

    runtime.block_on(async {
        let key = session.pane();
        let pane = server
            .pane_by_id(&key.id())
            .await?
            .ok_or("created pane disappeared")?;
        if pane
            .wait_for_text("mixed runtime", Duration::from_secs(5))
            .await?
            != PaneWait::Arrived
        {
            return Err("the pane did not print its greeting".into());
        }
        Ok(server.capture(key).await?)
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let runtime = Builder::new_current_thread().enable_all().build()?;
    let guard = runtime.block_on(TestServer::new())?;
    let bytes = capture(&guard, &runtime);
    runtime.block_on(guard.shutdown())?;
    io::stdout().lock().write_all(&bytes?)?;
    Ok(())
}
