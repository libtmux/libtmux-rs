//! Create a session synchronously and capture its first pane with Tokio.

use std::io::{self, Write};
use std::time::Duration;

use libtmux::tokio::{Error, PaneWait, Server};
use libtmux::{CreatedSession, NewSession, blocking};
use tokio::runtime::Builder;

enum CaptureOutcome {
    PaneAbsent,
    NotReady(PaneWait),
    Captured(Vec<u8>),
}

async fn capture(server: &Server, receipt: &CreatedSession) -> Result<CaptureOutcome, Error> {
    let key = receipt.pane();
    let session = server.inspect_session(receipt).await?;
    let Some(pane) = session
        .panes()
        .await?
        .into_iter()
        .find(|pane| pane.key() == key)
    else {
        return Ok(CaptureOutcome::PaneAbsent);
    };
    let readiness = pane
        .wait_for_text("mixed runtime", Duration::from_secs(5))
        .await?;
    if readiness != PaneWait::Arrived {
        return Ok(CaptureOutcome::NotReady(readiness));
    }
    Ok(CaptureOutcome::Captured(server.capture(key).await?))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let socket = arguments.next().ok_or("pass an absolute socket path")?;
    if arguments.next().is_some() {
        return Err("pass exactly one absolute socket path".into());
    }
    let blocking = blocking::Server::at(&socket)?;
    let server = Server::builder().socket_path(&socket).build()?;
    let runtime = Builder::new_current_thread().enable_all().build()?;
    let creation = blocking.new_session(
        NewSession::new("work")
            .size(40, 1)
            .command("printf 'mixed runtime'; exec cat"),
    );
    let captured = match creation {
        Ok(receipt) => {
            let captured = runtime
                .block_on(capture(&server, &receipt))
                .map_err(|error| error.after_creation(receipt.clone()));
            let cleanup = blocking
                .kill_created_session(&receipt)
                .map_err(|error| Error::from(error).after_creation(receipt.clone()));
            Error::after_cleanup(captured, cleanup)
        }
        Err(error) => {
            let cleanup = error.creation_receipt().map_or(Ok(()), |receipt| {
                blocking
                    .kill_created_session(receipt)
                    .map_err(|cleanup| Error::from(cleanup).after_creation(receipt.clone()))
            });
            Error::after_cleanup(Err(error.into()), cleanup)
        }
    };
    let captured = Error::after_cleanup(captured, runtime.block_on(server.shutdown()));
    match captured? {
        CaptureOutcome::Captured(bytes) => {
            io::stdout().lock().write_all(&bytes)?;
            Ok(())
        }
        CaptureOutcome::PaneAbsent => Err("the created pane was absent from its session".into()),
        CaptureOutcome::NotReady(readiness) => {
            Err(format!("pane readiness returned {readiness:?}").into())
        }
    }
}
