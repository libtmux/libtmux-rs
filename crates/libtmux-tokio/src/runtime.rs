use std::io;
use std::time::Instant;

use futures_util::future::BoxFuture;
use libtmux::driver::{AbortHandle, Process, Reader, Runtime, TaskError, TaskHandle, Writer};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

/// Native Tokio processes, timers, and background tasks for a client.
///
/// Constructing this adapter does not require a running runtime. Operations
/// must be driven from a Tokio runtime with I/O and time enabled.
///
/// ```
/// use std::sync::Arc;
/// let server = libtmux::Server::builder()
///     .runtime(Arc::new(libtmux_tokio::TokioRuntime))
///     .build()?;
/// # let _ = server;
/// # Ok::<(), libtmux::Error>(())
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioRuntime;

impl Runtime for TokioRuntime {
    fn sleep_until(&self, deadline: Instant) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            tokio::time::sleep_until(deadline.into()).await;
        })
    }

    fn spawn(&self, future: BoxFuture<'static, ()>) -> TaskHandle {
        let runtime = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime,
            Err(error) => {
                return TaskHandle::new(
                    Box::pin(async move { Err(TaskError::Spawn(io::Error::other(error))) }),
                    AbortHandle::new(|| {}),
                );
            }
        };
        let task = runtime.spawn(future);
        let abort = task.abort_handle();
        TaskHandle::new(
            Box::pin(async move {
                task.await.map_err(|error| {
                    if error.is_panic() {
                        TaskError::Panicked(error.into_panic())
                    } else {
                        TaskError::Cancelled
                    }
                })
            }),
            AbortHandle::new(move || abort.abort()),
        )
    }

    fn spawn_process(&self, command: std::process::Command) -> io::Result<Box<dyn Process>> {
        tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        Ok(Box::new(TokioProcess::spawn(command)?))
    }
}

struct TokioProcess {
    child: tokio::process::Child,
    retired: bool,
    ownership_lost: bool,
}

impl TokioProcess {
    fn spawn(command: std::process::Command) -> io::Result<Self> {
        // This wrapper owns the kill decision; Tokio must not keep a second
        // numeric-PID drop guard armed after an ECHILD error retires it.
        let child = tokio::process::Command::from(command)
            .kill_on_drop(false)
            .spawn()?;
        Ok(Self {
            child,
            retired: false,
            ownership_lost: false,
        })
    }

    fn observe<T>(&mut self, result: &io::Result<T>, reaped: bool) {
        self.ownership_lost |= result.as_ref().is_err_and(|error| {
            error.raw_os_error() == Some(rustix::io::Errno::CHILD.raw_os_error())
        });
        self.retired |= reaped || self.ownership_lost;
    }
}

impl Drop for TokioProcess {
    fn drop(&mut self) {
        if !self.retired {
            let _ = self.child.start_kill();
            let _ = self.child.try_wait();
        }
    }
}

impl Process for TokioProcess {
    fn id(&self) -> Option<u32> {
        if self.retired { None } else { self.child.id() }
    }
    fn take_stdin(&mut self) -> Option<Writer> {
        self.child
            .stdin
            .take()
            .map(|stream| Box::new(stream.compat_write()) as Writer)
    }
    fn take_stdout(&mut self) -> Option<Reader> {
        self.child
            .stdout
            .take()
            .map(|stream| Box::new(stream.compat()) as Reader)
    }
    fn take_stderr(&mut self) -> Option<Reader> {
        self.child
            .stderr
            .take()
            .map(|stream| Box::new(stream.compat()) as Reader)
    }
    fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        if self.ownership_lost {
            return Err(rustix::io::Errno::CHILD.into());
        }
        let result = self.child.try_wait();
        self.observe(&result, result.as_ref().is_ok_and(Option::is_some));
        result
    }
    fn start_kill(&mut self) -> io::Result<()> {
        if self.ownership_lost {
            return Err(rustix::io::Errno::CHILD.into());
        }
        if self.retired {
            return Ok(());
        }
        self.child.start_kill()
    }
    fn wait(&mut self) -> BoxFuture<'_, io::Result<std::process::ExitStatus>> {
        Box::pin(async move {
            if self.ownership_lost {
                return Err(rustix::io::Errno::CHILD.into());
            }
            let result = self.child.wait().await;
            self.observe(&result, result.is_ok());
            result
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::TokioProcess;
    use libtmux::driver::Process as _;
    use rustix::process::{Pid, Signal, WaitIdOptions, WaitOptions, kill_process, waitid, waitpid};
    use std::os::unix::process::CommandExt as _;

    fn sleeper() -> TokioProcess {
        let mut command = std::process::Command::new("/bin/sleep");
        command.arg("30").process_group(0);
        TokioProcess::spawn(command).expect("owned sleeper")
    }

    fn pid(process: &TokioProcess) -> Pid {
        Pid::from_raw(i32::try_from(process.child.id().unwrap()).unwrap()).unwrap()
    }

    fn remains_running(pid: Pid) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
        loop {
            match waitid(
                rustix::process::WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            ) {
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Ok(None) => return true,
                Ok(Some(_)) | Err(_) => return false,
            }
        }
    }

    #[tokio::test]
    async fn echild_retires_native_process_identity() {
        let mut process = sleeper();
        let original_pid = pid(&process);
        kill_process(original_pid, Signal::KILL).expect("harness kills child");
        waitpid(Some(original_pid), WaitOptions::empty()).expect("deliberate external reap");
        let error = process.try_wait().expect_err("driver observes ECHILD");
        assert_eq!(
            error.raw_os_error(),
            Some(rustix::io::Errno::CHILD.raw_os_error())
        );
        assert!(process.id().is_none());
        assert_eq!(
            process.wait().await.unwrap_err().raw_os_error(),
            error.raw_os_error()
        );
        let mut replacement = sleeper();
        let replacement_pid = pid(&replacement);
        std::mem::swap(&mut process.child, &mut replacement.child);
        let inert_kill = process.start_kill().is_err();
        drop(process);
        let alive = remains_running(replacement_pid);
        let _ = kill_process(replacement_pid, Signal::KILL);
        let _ = waitpid(Some(replacement_pid), WaitOptions::empty());
        replacement.retired = true;
        assert!(
            inert_kill && alive,
            "retired identity must not signal a replacement"
        );
    }

    #[tokio::test]
    async fn normal_native_wait_retires_drop_guard() {
        let mut process = sleeper();
        process.start_kill().expect("direct signal");
        process.wait().await.expect("owned child reaped");
        assert!(process.id().is_none());
        let mut replacement = sleeper();
        let replacement_pid = pid(&replacement);
        std::mem::swap(&mut process.child, &mut replacement.child);
        process.start_kill().expect("retired kill is inert");
        drop(process);
        let alive = remains_running(replacement_pid);
        let _ = kill_process(replacement_pid, Signal::KILL);
        let _ = waitpid(Some(replacement_pid), WaitOptions::empty());
        replacement.retired = true;
        assert!(alive, "reaped child's drop guard must be inert");
    }
}
