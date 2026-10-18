use std::io::{self, Read};
use std::net::Shutdown;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::ExitStatus;
use std::sync::mpsc;
use std::time::Duration;

use super::{OutputReader, RequestContext};
use crate::internal::process::ProcessChild;
use crate::{CancellationToken, Error};

type Output = Result<(Vec<u8>, Vec<u8>), Error>;
enum Event {
    Read(usize, Result<Vec<u8>, Error>),
    Wake,
}

/// Park output readers in the kernel until output, EOF, or shutdown.
///
/// Each reader sends one bounded result. Cancellation sends only a wakeup,
/// so it never waits for a reader to make room in a channel.
pub(super) fn collect(
    child: &mut ProcessChild,
    readers: [OutputReader; 2],
    context: &RequestContext,
    token: &CancellationToken,
    shutdown: &CancellationToken,
    check: impl Fn() -> Result<(), Error>,
) -> (Output, io::Result<ExitStatus>) {
    let mut interrupts = Vec::with_capacity(2);
    for reader in &readers {
        match reader.stream.try_clone().and_then(|stream| {
            stream.set_nonblocking(false)?;
            Ok(stream)
        }) {
            Ok(stream) => interrupts.push(stream),
            Err(source) => {
                return (
                    Err(Error::wait_child(
                        context.request_id(),
                        context.command.clone(),
                        source,
                    )),
                    child.finish(),
                );
            }
        }
    }
    std::thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel();
        let cancel_sender = sender.clone();
        let _cancel = token.on_cancel(move || {
            let _ = cancel_sender.send(Event::Wake);
        });
        let shutdown_sender = sender.clone();
        let _shutdown = shutdown.on_cancel(move || {
            let _ = shutdown_sender.send(Event::Wake);
        });
        let mut handles = Vec::with_capacity(2);
        let output = (|| {
            for (index, reader) in readers.into_iter().enumerate() {
                let sender = sender.clone();
                let handle = std::thread::Builder::new()
                    .name(format!("libtmux-{}", reader.name))
                    .spawn_scoped(scope, move || {
                        let name = reader.name;
                        let result = catch_unwind(AssertUnwindSafe(|| read(reader, context)))
                            .unwrap_or_else(|_| {
                                Err(Error::read_output(
                                    context.request_id(),
                                    context.command.clone(),
                                    name,
                                    io::ErrorKind::Other,
                                ))
                            });
                        let _ = sender.send(Event::Read(index, result));
                    })
                    .map_err(|source| {
                        Error::wait_child(context.request_id(), context.command.clone(), source)
                    })?;
                handles.push(handle);
            }
            let mut output: [Option<Vec<u8>>; 2] = [None, None];
            loop {
                check()?;
                if output.iter().all(Option::is_some) {
                    // EOF alone does not establish that the leader has exited.
                    if child.exited().map_err(|source| {
                        Error::wait_child(context.request_id(), context.command.clone(), source)
                    })? {
                        let [stdout, stderr] = output;
                        return Ok((stdout.unwrap_or_default(), stderr.unwrap_or_default()));
                    }
                    // Only a process that closes both streams before exiting
                    // needs this fallback. An idle parked tmux client does not.
                    match receiver.recv_timeout(Duration::from_millis(1)) {
                        Ok(Event::Wake) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Ok(Event::Read(_, result)) => {
                            result?;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            return Err(disconnected(context));
                        }
                    }
                } else {
                    match receiver.recv().map_err(|_| disconnected(context))? {
                        Event::Read(index, result) => output[index] = Some(result?),
                        Event::Wake => {}
                    }
                }
            }
        })();
        // The leader pins its group ID until finish reaps it. Closing our
        // sockets also releases readers if the group-signal helper failed.
        let status = child.finish();
        for stream in interrupts {
            let _ = stream.shutdown(Shutdown::Both);
        }
        for handle in handles {
            let _ = handle.join();
        }
        (output, status)
    })
}

fn read(mut reader: OutputReader, context: &RequestContext) -> Result<Vec<u8>, Error> {
    let mut buffer = [0; 8192];
    loop {
        let remaining = reader
            .limit
            .saturating_sub(reader.bytes.len())
            .saturating_add(1)
            .min(buffer.len());
        match reader.stream.read(&mut buffer[..remaining]) {
            Ok(0) => return Ok(reader.bytes),
            Ok(count) if reader.bytes.len().saturating_add(count) > reader.limit => {
                return Err(Error::OutputLimitExceeded {
                    request_id: context.request_id(),
                    command: context.command.clone(),
                    stream: reader.name,
                    limit: reader.limit,
                });
            }
            Ok(count) => reader.bytes.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(Error::read_output(
                    context.request_id(),
                    context.command.clone(),
                    reader.name,
                    error.kind(),
                ));
            }
        }
    }
}

fn disconnected(context: &RequestContext) -> Error {
    Error::wait_child(
        context.request_id(),
        context.command.clone(),
        io::Error::other("parked process output readers stopped"),
    )
}
