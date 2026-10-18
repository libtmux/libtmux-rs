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
        let child = tokio::process::Command::from(command)
            .kill_on_drop(true)
            .spawn()?;
        Ok(Box::new(TokioProcess(child)))
    }
}

struct TokioProcess(tokio::process::Child);

impl Process for TokioProcess {
    fn id(&self) -> Option<u32> {
        self.0.id()
    }
    fn take_stdin(&mut self) -> Option<Writer> {
        self.0
            .stdin
            .take()
            .map(|stream| Box::new(stream.compat_write()) as Writer)
    }
    fn take_stdout(&mut self) -> Option<Reader> {
        self.0
            .stdout
            .take()
            .map(|stream| Box::new(stream.compat()) as Reader)
    }
    fn take_stderr(&mut self) -> Option<Reader> {
        self.0
            .stderr
            .take()
            .map(|stream| Box::new(stream.compat()) as Reader)
    }
    fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.0.try_wait()
    }
    fn start_kill(&mut self) -> io::Result<()> {
        self.0.start_kill()
    }
    fn wait(&mut self) -> BoxFuture<'_, io::Result<std::process::ExitStatus>> {
        Box::pin(self.0.wait())
    }
}
