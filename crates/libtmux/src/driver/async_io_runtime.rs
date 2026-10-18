use std::io;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use async_executor::Executor;
use futures_channel::oneshot;
use futures_util::FutureExt as _;
use futures_util::future::{AbortHandle as FutureAbort, Abortable, BoxFuture};

use super::{AbortHandle, Process, Runtime, StdProcess, TaskError, TaskHandle};

/// A non-Tokio driver backed by standard processes and asynchronous OS readiness.
///
/// Construction is inert. The first background task starts one shared worker;
/// `async-io` provides readiness and timers. Dropping a waiter leaves its owned
/// supervisor running until cleanup completes.
#[derive(Clone, Copy, Debug, Default)]
pub struct AsyncIoRuntime;

type SharedExecutor = Arc<Executor<'static>>;
static EXECUTOR: OnceLock<SharedExecutor> = OnceLock::new();
static STARTING: Mutex<()> = Mutex::new(());

fn executor() -> io::Result<&'static SharedExecutor> {
    if let Some(executor) = EXECUTOR.get() {
        return Ok(executor);
    }
    let _starting = STARTING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(executor) = EXECUTOR.get() {
        return Ok(executor);
    }
    let executor = Arc::new(Executor::new());
    let worker = Arc::clone(&executor);
    std::thread::Builder::new()
        .name("libtmux-supervisors".to_owned())
        .spawn(move || async_io::block_on(worker.run(std::future::pending::<()>())))?;
    Ok(EXECUTOR.get_or_init(|| executor))
}

impl Runtime for AsyncIoRuntime {
    fn sleep_until(&self, deadline: Instant) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            async_io::Timer::at(deadline).await;
        })
    }

    fn spawn(&self, task: BoxFuture<'static, ()>) -> TaskHandle {
        let (abort, registration) = FutureAbort::new_pair();
        let cancellation = AbortHandle::new(move || abort.abort());
        let executor = match executor() {
            Ok(executor) => executor,
            Err(error) => {
                return TaskHandle::new(
                    Box::pin(async { Err(TaskError::Spawn(error)) }),
                    cancellation,
                );
            }
        };
        let (send, receive) = oneshot::channel();
        executor
            .spawn(async move {
                let result = match AssertUnwindSafe(Abortable::new(task, registration))
                    .catch_unwind()
                    .await
                {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(_)) => Err(TaskError::Cancelled),
                    Err(payload) => Err(TaskError::Panicked(payload)),
                };
                let _ = send.send(result);
            })
            .detach();
        TaskHandle::new(
            Box::pin(async move { receive.await.unwrap_or(Err(TaskError::Cancelled)) }),
            cancellation,
        )
    }

    fn spawn_process(&self, command: std::process::Command) -> io::Result<Box<dyn Process>> {
        executor()?;
        StdProcess::spawn(command)
    }
}
