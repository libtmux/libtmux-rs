use std::io;
use std::os::fd::OwnedFd;
use std::process::{Child, Command, ExitStatus};

use async_io::Async;
use futures_util::future::BoxFuture;
use rustix::process::{Pid, PidfdFlags, Signal, kill_process_group, pidfd_open};

use super::{AsyncIoRuntime, Runtime as _};

/// A process output stream supplied by a driver.
pub type Reader = Box<dyn futures_io::AsyncRead + Send + Unpin>;

/// A process input stream supplied by a driver.
pub type Writer = Box<dyn futures_io::AsyncWrite + Send + Unpin>;

/// An owned process whose streams and exit can be awaited independently.
///
/// `wait` must not reap the child until it is polled. The supervisor drains
/// output before waiting so an unreaped leader anchors its process group.
pub trait Process: Send {
    /// Return the unreaped child's process ID.
    fn id(&self) -> Option<u32>;
    /// Take the child's input stream once.
    fn take_stdin(&mut self) -> Option<Writer>;
    /// Take the child's output stream once.
    fn take_stdout(&mut self) -> Option<Reader>;
    /// Take the child's error stream once.
    fn take_stderr(&mut self) -> Option<Reader>;
    /// Reap a child that has exited, without blocking.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if the child status cannot be read.
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;
    /// Ask the operating system to kill the child.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if the child cannot be signalled.
    fn start_kill(&mut self) -> io::Result<()>;
    /// Wait for and reap the child.
    fn wait(&mut self) -> BoxFuture<'_, io::Result<ExitStatus>>;
}

pub(crate) struct StdProcess {
    child: Option<Child>,
    readiness: Option<Async<OwnedFd>>,
    stdin: Option<Writer>,
    stdout: Option<Reader>,
    stderr: Option<Reader>,
    reaped: bool,
}

impl StdProcess {
    pub(crate) fn spawn(mut command: Command) -> io::Result<Box<dyn Process>> {
        let child = command.spawn()?;
        let mut process = Self {
            child: Some(child),
            readiness: None,
            stdin: None,
            stdout: None,
            stderr: None,
            reaped: false,
        };
        let child = process.child.as_mut().ok_or_else(missing_child)?;
        let pid = Pid::from_raw(i32::try_from(child.id()).map_err(io::Error::other)?)
            .ok_or_else(missing_child)?;
        process.readiness = Some(Async::new_nonblocking(pidfd_open(
            pid,
            PidfdFlags::empty(),
        )?)?);
        process.stdin = child
            .stdin
            .take()
            .map(Async::new)
            .transpose()?
            .map(|stream| Box::new(stream) as Writer);
        process.stdout = child
            .stdout
            .take()
            .map(Async::new)
            .transpose()?
            .map(|stream| Box::new(stream) as Reader);
        process.stderr = child
            .stderr
            .take()
            .map(Async::new)
            .transpose()?
            .map(|stream| Box::new(stream) as Reader);
        Ok(Box::new(process))
    }
}

impl Process for StdProcess {
    fn id(&self) -> Option<u32> {
        (!self.reaped)
            .then(|| self.child.as_ref().map(Child::id))
            .flatten()
    }

    fn take_stdin(&mut self) -> Option<Writer> {
        self.stdin.take()
    }
    fn take_stdout(&mut self) -> Option<Reader> {
        self.stdout.take()
    }
    fn take_stderr(&mut self) -> Option<Reader> {
        self.stderr.take()
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.as_mut().ok_or_else(missing_child)?.try_wait()?;
        self.reaped |= status.is_some();
        Ok(status)
    }

    fn start_kill(&mut self) -> io::Result<()> {
        self.child.as_mut().ok_or_else(missing_child)?.kill()
    }

    fn wait(&mut self) -> BoxFuture<'_, io::Result<ExitStatus>> {
        Box::pin(async move {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            self.readiness
                .as_ref()
                .ok_or_else(missing_child)?
                .readable()
                .await?;
            self.try_wait()?
                .ok_or_else(|| io::Error::other("child exit readiness had no exit status"))
        })
    }
}

impl Drop for StdProcess {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        if let Some(pid) = i32::try_from(child.id()).ok().and_then(Pid::from_raw) {
            let _ = kill_process_group(pid, Signal::KILL);
        }
        let _ = child.kill();
        if let Some(readiness) = self.readiness.take() {
            drop(AsyncIoRuntime.spawn(Box::pin(async move {
                let _ = readiness.readable().await;
                let _ = child.wait();
            })));
        } else {
            // Registration failed before the process reached its supervisor.
            let _ = child.wait();
        }
    }
}

fn missing_child() -> io::Error {
    io::Error::other("child process is unavailable")
}
