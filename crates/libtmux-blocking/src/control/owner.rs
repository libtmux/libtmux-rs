use std::fs::File;
use std::io::{self, Read, Write};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Instant;

use rustix::event::{PollFd, PollFlags, Timespec, poll};

use super::pin::PinnedEndpoint;
use super::{
    Completion, ControlError, ControlFault, ControlProbe, DRAINING, FINISHED, INTERVAL, QUEUED,
    Request, Shared, WRITING, cancel, wire,
};
use crate::supervisor::{OwnedChild, Permit, nonblocking_file};
use crate::{RequestOptions, fresh_nonce};

struct Pipes {
    input: File,
    output: File,
    error: File,
}
struct Active {
    request: Option<Request>,
    bytes: Vec<u8>,
    written: usize,
    drain_deadline: Option<Instant>,
}

pub(super) fn run(
    mut command: Command,
    _endpoint: PinnedEndpoint,
    _permit: Permit,
    shared: &Shared,
    rx: &mpsc::Receiver<Request>,
    options: &RequestOptions,
) {
    let mut child = match command.spawn() {
        Ok(child) => OwnedChild::new(child),
        Err(error) => {
            publish(shared, Some(ControlError::native(error, false)), Ok(()));
            return;
        }
    };
    let mut active = None;
    let result = pipes(&mut child)
        .and_then(|mut pipes| drive(&mut child, &mut pipes, shared, rx, options, &mut active));
    if let Err(error) = &result {
        shared.lock().terminal = Some(error.clone().pending());
        shared.changed.notify_all();
    }
    let cleaned = child.finish().map(|_| ());
    let terminal = match (result, cleaned) {
        (Ok(()), Ok(())) => None,
        (Err(error), Ok(())) => Some(error),
        (Ok(()), Err(errors)) => {
            Some(ControlError::protocol(ControlFault::Closed, false).cleanup(errors))
        }
        (Err(error), Err(errors)) => Some(error.cleanup(errors)),
    };
    let cleanup = terminal.as_ref().map_or(Ok(()), |error| Err(error.clone()));
    publish(shared, terminal.clone(), cleanup);
    if let Some(active) = active {
        if let Some(request) = active.request {
            fail_request(
                request,
                terminal
                    .clone()
                    .unwrap_or_else(|| ControlError::protocol(ControlFault::Closed, true)),
            );
        }
    }
    while let Ok(request) = rx.try_recv() {
        fail_request(request, ControlError::protocol(ControlFault::Closed, false));
    }
    // The persistent reservation and pinned paths outlive any delayed reap.
    if child.child.is_some() {
        child.reap();
    }
}

fn pipes(child: &mut OwnedChild) -> Result<Pipes, ControlError> {
    let missing = || ControlError::native(io::Error::other("control pipe is unavailable"), false);
    let child = child.child.as_mut().ok_or_else(missing)?;
    Ok(Pipes {
        input: nonblocking_file(child.stdin.take().ok_or_else(missing)?)
            .map_err(|error| ControlError::native(error, false))?,
        output: nonblocking_file(child.stdout.take().ok_or_else(missing)?)
            .map_err(|error| ControlError::native(error, false))?,
        error: nonblocking_file(child.stderr.take().ok_or_else(missing)?)
            .map_err(|error| ControlError::native(error, false))?,
    })
}

fn publish(shared: &Shared, terminal: Option<ControlError>, cleanup: Result<(), ControlError>) {
    let mut state = shared.lock();
    state.tracker.close(
        terminal
            .as_ref()
            .and_then(ControlError::fault)
            .unwrap_or(ControlFault::Closed),
    );
    state.terminal = terminal;
    state.cleanup = Some(cleanup);
    shared.stop.store(true, Ordering::Release);
    shared.changed.notify_all();
}

fn drive(
    child: &mut OwnedChild,
    pipes: &mut Pipes,
    shared: &Shared,
    rx: &mpsc::Receiver<Request>,
    options: &RequestOptions,
    active: &mut Option<Active>,
) -> Result<(), ControlError> {
    let mut stderr_bytes = 0usize;
    let mut exited_at = None;
    let mut stderr_eof = false;
    loop {
        if shared.stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let ready = shared.lock().ready;
        if !ready
            && (options.cancellation.is_cancelled() || Instant::now() >= options.budget.deadline)
        {
            let fault = if options.cancellation.is_cancelled() {
                ControlFault::Cancelled
            } else {
                ControlFault::Deadline
            };
            return Err(ControlError::protocol(fault, false));
        }
        if active.is_none() && ready {
            while let Ok(request) = rx.try_recv() {
                if let Some(next) = start_request(shared, request)? {
                    *active = Some(next);
                    break;
                }
            }
        }
        progress(active, shared, pipes)?;
        let read = read_output(pipes, shared)?;
        read_error(
            pipes,
            options.budget.bytes_per_stream,
            &mut stderr_bytes,
            &mut stderr_eof,
        )?;
        if !shared.watching.load(Ordering::Acquire) {
            shared.lock().tracker.discard_notices();
        }
        completions(shared, options, active, ready)?;
        if ready && shared.lock().tracker.is_closed() {
            // feed recognised %exit. Pending commands have separate completions.
            return Ok(());
        }
        if read == Some(0) {
            shared
                .lock()
                .tracker
                .eof()
                .map_err(|fault| ControlError::protocol(fault, ready))?;
            return Err(ControlError::protocol(ControlFault::Disconnected, ready));
        }
        if child
            .has_exited()
            .map_err(|error| ControlError::native(error, ready))?
        {
            // A child may exit with more than one read left in the pipe.
            // A descendant inheriting stdout cannot extend this drain forever.
            let since = exited_at.get_or_insert_with(Instant::now);
            if read.is_some() && since.elapsed() < libtmux_core::supervision::CLEANUP_ALLOWANCE {
                continue;
            }
            return Err(ControlError::protocol(ControlFault::Disconnected, ready));
        }
        let writing = active
            .as_ref()
            .is_some_and(|current| current.written < current.bytes.len());
        let mut fds = vec![PollFd::new(&pipes.output, PollFlags::IN)];
        if !stderr_eof {
            fds.push(PollFd::new(&pipes.error, PollFlags::IN));
        }
        if writing {
            fds.push(PollFd::new(&pipes.input, PollFlags::OUT));
        }
        let timeout = Timespec::try_from(INTERVAL)
            .map_err(|error| ControlError::native(io::Error::other(error), ready))?;
        match poll(&mut fds, Some(&timeout)) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(error) => return Err(ControlError::native(error.into(), ready)),
        }
    }
}

fn progress(
    active: &mut Option<Active>,
    shared: &Shared,
    pipes: &mut Pipes,
) -> Result<(), ControlError> {
    if let Some(current) = active {
        if let Some(request) = &current.request {
            if request.options.cancellation.is_cancelled()
                || Instant::now() >= request.options.budget.deadline
            {
                cancel(&request.phase);
            }
            if request.phase.load(Ordering::Acquire) == DRAINING {
                shared.lock().tracker.cancel(request.token);
                let deadline = current.drain_deadline.get_or_insert_with(|| {
                    Instant::now()
                        .checked_add(shared.limits.drain_timeout)
                        .unwrap_or_else(Instant::now)
                });
                if Instant::now() >= *deadline {
                    return Err(ControlError::protocol(ControlFault::Deadline, true));
                }
            }
        }
        if current.written < current.bytes.len() {
            match pipes.input.write(&current.bytes[current.written..]) {
                Ok(0) => {
                    return Err(ControlError::native(
                        io::Error::new(
                            io::ErrorKind::WriteZero,
                            "control input stopped accepting bytes",
                        ),
                        true,
                    ));
                }
                Ok(count) => current.written += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(ControlError::native(error, true)),
            }
        }
    }
    Ok(())
}

fn completions(
    shared: &Shared,
    options: &RequestOptions,
    active: &mut Option<Active>,
    ready: bool,
) -> Result<(), ControlError> {
    loop {
        let completion = shared.lock().tracker.next_completion();
        match completion {
            Some(Completion::Opened) => {
                let probe =
                    ControlProbe::new(shared.identity.session().clone(), fresh_nonce(options)?);
                let nonce = fresh_nonce(options)?;
                let bytes = wire(&probe.command(), nonce, shared.limits.max_line_bytes)?;
                shared
                    .lock()
                    .tracker
                    .register_challenge(0, nonce, probe, shared.identity.clone())
                    .map_err(|fault| ControlError::protocol(fault, false))?;
                *active = Some(Active {
                    request: None,
                    bytes,
                    written: 0,
                    drain_deadline: None,
                });
            }
            Some(Completion::Reply { token: 0, .. }) => {
                if !shared.lock().tracker.is_qualified() {
                    return Err(ControlError::protocol(
                        ControlFault::InvalidChallenge,
                        false,
                    ));
                }
                *active = None;
                shared.lock().ready = true;
                shared.changed.notify_all();
            }
            Some(Completion::Reply { token, reply }) => {
                let request = take_request(active, token)?;
                request.phase.store(FINISHED, Ordering::Release);
                let _ = request.tx.try_send(Ok(reply));
            }
            Some(Completion::Drained { token }) => {
                let request = take_request(active, token)?;
                let fault = if request.options.cancellation.is_cancelled() {
                    ControlFault::Cancelled
                } else {
                    ControlFault::Deadline
                };
                fail_request(request, ControlError::protocol(fault, true));
            }
            Some(Completion::Lost { fault, .. }) => {
                return Err(ControlError::protocol(fault, ready));
            }
            None => break,
        }
    }
    Ok(())
}

fn start_request(shared: &Shared, mut request: Request) -> Result<Option<Active>, ControlError> {
    if request.options.cancellation.is_cancelled()
        || Instant::now() >= request.options.budget.deadline
    {
        cancel(&request.phase);
    }
    if request
        .phase
        .compare_exchange(QUEUED, WRITING, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        fail_request(
            request,
            ControlError::protocol(ControlFault::Cancelled, false),
        );
        return Ok(None);
    }
    shared
        .lock()
        .tracker
        .register_with_reply_limit(
            request.token,
            request.nonce,
            request.kind.clone(),
            request.options.budget.bytes_per_stream,
        )
        .map_err(|fault| ControlError::protocol(fault, false))?;
    Ok(Some(Active {
        bytes: std::mem::take(&mut request.bytes),
        request: Some(request),
        written: 0,
        drain_deadline: None,
    }))
}

fn take_request(active: &mut Option<Active>, token: u64) -> Result<Request, ControlError> {
    active
        .take()
        .and_then(|active| active.request)
        .filter(|request| request.token == token)
        .ok_or_else(|| ControlError::protocol(ControlFault::UnownedReply, true))
}

fn fail_request(request: Request, error: ControlError) {
    let Request { phase, tx, .. } = request;
    phase.store(FINISHED, Ordering::Release);
    let _ = tx.try_send(Err(error));
}

fn read_output(pipes: &mut Pipes, shared: &Shared) -> Result<Option<usize>, ControlError> {
    let mut bytes = [0; 8192];
    let read = loop {
        match pipes.output.read(&mut bytes) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            result => break result,
        }
    };
    match read {
        Ok(0) => Ok(Some(0)),
        Ok(count) => {
            shared
                .lock()
                .tracker
                .feed(&bytes[..count])
                .map_err(|fault| ControlError::protocol(fault, true))?;
            shared.changed.notify_all();
            Ok(Some(count))
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(ControlError::native(error, true)),
    }
}

fn read_error(
    pipes: &mut Pipes,
    limit: usize,
    count: &mut usize,
    eof: &mut bool,
) -> Result<(), ControlError> {
    if *eof {
        return Ok(());
    }
    let mut bytes = [0; 8192];
    match pipes.error.read(&mut bytes) {
        Ok(0) => *eof = true,
        Ok(read) => {
            *count = count.saturating_add(read);
            if *count > limit {
                return Err(ControlError::from(libtmux_core::Error::OutputLimit {
                    stream: "stderr",
                    limit,
                }));
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) => {}
        Err(error) => return Err(ControlError::native(error, true)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt as _;

    #[test]
    fn a_pipe_setup_failure_reclaims_the_already_spawned_child() {
        let mut command = Command::new("python3");
        command
            .args(["-c", "import time; time.sleep(30)"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0);
        let spawned = command.spawn().unwrap();
        let pid = spawned.id();
        let mut child = OwnedChild::new(spawned);
        assert!(pipes(&mut child).is_err());
        child.finish().unwrap();
        assert!(child.child.is_none());
        #[cfg(target_os = "linux")]
        assert!(!std::path::Path::new("/proc").join(pid.to_string()).exists());
        #[cfg(not(target_os = "linux"))]
        let _ = pid;
    }
}
