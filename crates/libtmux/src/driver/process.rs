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
/// Implementations must retire the child's identity after reaping or `ECHILD`;
/// subsequent kill and drop operations must not signal that numeric ID.
///
/// The library requires exclusive child-reaping ownership. External reapers
/// and automatic `SIGCHLD` reaping are unsupported.
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
    ownership_lost: bool,
    #[cfg(test)]
    deny_drop_signals: bool,
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
            ownership_lost: false,
            #[cfg(test)]
            deny_drop_signals: false,
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
        (!self.reaped && !self.ownership_lost)
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
        if self.ownership_lost {
            return Err(rustix::io::Errno::CHILD.into());
        }
        let result = self.child.as_mut().ok_or_else(missing_child)?.try_wait();
        match &result {
            Ok(Some(_)) => self.reaped = true,
            Err(error) if error.raw_os_error() == Some(rustix::io::Errno::CHILD.raw_os_error()) => {
                self.ownership_lost = true;
            }
            _ => {}
        }
        result
    }

    fn start_kill(&mut self) -> io::Result<()> {
        if self.ownership_lost {
            return Err(rustix::io::Errno::CHILD.into());
        }
        if self.reaped {
            return Ok(());
        }
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
        if self.reaped || self.ownership_lost {
            return;
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        #[cfg(test)]
        let deny_signals = self.deny_drop_signals;
        #[cfg(not(test))]
        let deny_signals = false;
        if !deny_signals && let Some(pid) = i32::try_from(child.id()).ok().and_then(Pid::from_raw) {
            let _ = kill_process_group(pid, Signal::KILL);
        }
        let killed = !deny_signals && child.kill().is_ok();
        match child.try_wait() {
            Ok(None) if killed => {}
            Ok(_) | Err(_) => return,
        }
        let readiness = self.readiness.take();
        drop(AsyncIoRuntime.spawn(Box::pin(async move {
            if let Some(readiness) = readiness {
                let _ = readiness.readable().await;
                let _ = child.try_wait();
            } else {
                // Registration can fail after spawn. Reap without blocking Drop
                // or the worker thread if the killed process delays its exit.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) | Err(_) => break,
                        Ok(None) if std::time::Instant::now() >= deadline => break,
                        Ok(None) => {
                            async_io::Timer::after(std::time::Duration::from_millis(1)).await;
                        }
                    }
                }
            }
        })));
    }
}

fn missing_child() -> io::Error {
    io::Error::other("child process is unavailable")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::{Process, StdProcess};
    use rustix::process::{
        PidfdFlags, Signal, WaitOptions, kill_process, pidfd_open, pidfd_send_signal, waitpid,
    };
    use std::os::unix::process::CommandExt as _;
    use std::time::{Duration, Instant};

    fn unregistered() -> StdProcess {
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("owned sleeper");
        StdProcess {
            child: Some(child),
            readiness: None,
            stdin: None,
            stdout: None,
            stderr: None,
            reaped: false,
            ownership_lost: false,
            deny_drop_signals: false,
        }
    }

    #[test]
    fn failed_registration_drop_does_not_wait_after_failed_signals() {
        let mut process = unregistered();
        process.deny_drop_signals = true;
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(process.id().unwrap()).unwrap()).unwrap();
        let pidfd = pidfd_open(pid, PidfdFlags::empty()).expect("harness reference");
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            drop(process);
            send.send(()).expect("harness receiver");
        });
        let returned = receive.recv_timeout(Duration::from_millis(250)).is_ok();
        pidfd_send_signal(&pidfd, Signal::KILL).expect("harness kills its owned child");
        let _ = waitpid(Some(pid), WaitOptions::empty());
        worker.join().expect("drop thread remains healthy");
        assert!(
            returned,
            "Drop must return when registration and both signals fail"
        );
    }

    #[test]
    fn unregistered_drop_reaps_after_a_successful_signal() {
        let process = unregistered();
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(process.id().unwrap()).unwrap()).unwrap();
        drop(process);
        let deadline = Instant::now() + Duration::from_secs(5);
        while rustix::process::test_kill_process(pid).is_ok() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
    }

    #[test]
    fn echild_retires_std_process_identity() {
        let mut process = unregistered();
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(process.id().unwrap()).unwrap()).unwrap();
        kill_process(pid, Signal::KILL).expect("harness kills child");
        waitpid(Some(pid), WaitOptions::empty())
            .expect("harness deliberately reaps outside the driver");
        assert_eq!(
            process.try_wait().unwrap_err().raw_os_error(),
            Some(rustix::io::Errno::CHILD.raw_os_error())
        );
        assert!(process.id().is_none());
        let mut replacement = unregistered();
        std::mem::swap(&mut process.child, &mut replacement.child);
        let inert_kill = process.start_kill().is_err();
        let alive_after_kill = process
            .child
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none();
        let replacement_pid = rustix::process::Pid::from_raw(
            i32::try_from(process.child.as_ref().unwrap().id()).unwrap(),
        )
        .unwrap();
        drop(process);
        let deadline = Instant::now() + Duration::from_millis(50);
        let alive_after_drop = loop {
            use rustix::process::{WaitIdOptions, waitid};
            match waitid(
                rustix::process::WaitId::Pid(replacement_pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            ) {
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(None) => break true,
                Ok(Some(_)) | Err(_) => break false,
            }
        };
        let _ = kill_process(replacement_pid, Signal::KILL);
        let _ = waitpid(Some(replacement_pid), WaitOptions::empty());
        // The replacement struct holds the original already-reaped process.
        replacement.ownership_lost = true;
        assert!(
            inert_kill && alive_after_kill && alive_after_drop,
            "retired identity must be inert"
        );
    }
}
