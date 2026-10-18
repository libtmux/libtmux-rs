//! Task and timer contracts used by the client supervisors.

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Instant;

use futures_channel::oneshot;
use futures_util::future::BoxFuture;

/// Executes client supervisors and wakes bounded waits.
///
/// Spawned tasks must continue to run without their handles being polled.
/// Dropping a task handle detaches the task; only [`TaskHandle::abort`] asks
/// the runtime to cancel it. Task panics must retain their original payload.
pub trait Runtime: Send + Sync + 'static {
    /// Wake when `deadline` is reached.
    fn sleep_until(&self, deadline: Instant) -> BoxFuture<'static, ()>;

    /// Start a supervisor independently of the returned handle.
    fn spawn(&self, task: BoxFuture<'static, ()>) -> TaskHandle;

    /// Start a child with the configured arguments, environment, and streams.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error if the child or its streams
    /// cannot be created.
    fn spawn_process(
        &self,
        command: std::process::Command,
    ) -> std::io::Result<Box<dyn crate::driver::Process>>;
}

/// Typed task results over an object-safe [`Runtime`].
pub trait RuntimeExt: Runtime {
    /// Start a task whose result is returned through its handle.
    fn spawn_value<T: Send + 'static>(
        &self,
        task: impl Future<Output = T> + Send + 'static,
    ) -> TaskHandle<T> {
        let (send, receive) = oneshot::channel();
        let supervisor = self.spawn(Box::pin(async move {
            let _ = send.send(task.await);
        }));
        let abort = supervisor.abort_handle();
        TaskHandle::new(
            Box::pin(async move {
                supervisor.await?;
                receive.await.map_err(|_| TaskError::Cancelled)
            }),
            abort,
        )
    }
}

impl<R: Runtime + ?Sized> RuntimeExt for R {}

/// A request to cancel a task without waiting for it to stop.
#[derive(Clone)]
pub struct AbortHandle {
    abort: Arc<dyn Fn() + Send + Sync>,
}

impl AbortHandle {
    /// Wrap a runtime's cancellation request.
    ///
    /// The callback must be safe to call more than once, including after the
    /// task has completed. Dropping the callback must not cancel the task.
    #[must_use]
    pub fn new(abort: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            abort: Arc::new(abort),
        }
    }

    /// Ask the runtime to cancel the task.
    pub fn abort(&self) {
        (self.abort)();
    }
}

impl fmt::Debug for AbortHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AbortHandle")
            .finish_non_exhaustive()
    }
}

/// A spawned task's result; dropping the handle detaches the task.
#[must_use = "dropping the task handle detaches the task"]
pub struct TaskHandle<T = ()> {
    // Keep the handle Sync for public control handles without requiring a
    // Sync future. Polling has exclusive access and does not acquire a lock.
    completion: Mutex<BoxFuture<'static, Result<T, TaskError>>>,
    abort: AbortHandle,
}

impl<T> TaskHandle<T> {
    /// Wrap a runtime's completion future and cancellation request.
    ///
    /// Dropping `completion` must detach its task. A runtime whose native
    /// handle cancels on drop must arrange detachment before using this
    /// constructor.
    pub fn new(completion: BoxFuture<'static, Result<T, TaskError>>, abort: AbortHandle) -> Self {
        Self {
            completion: Mutex::new(completion),
            abort,
        }
    }

    /// Ask the runtime to cancel the task.
    ///
    /// Await the handle to observe whether cancellation or completion won.
    pub fn abort(&self) {
        self.abort.abort();
    }

    /// Obtain a cancellation request that can outlive this handle.
    #[must_use]
    pub fn abort_handle(&self) -> AbortHandle {
        self.abort.clone()
    }
}

impl<T> Future for TaskHandle<T> {
    type Output = Result<T, TaskError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut()
            .completion
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            .poll(context)
    }
}

impl<T> fmt::Debug for TaskHandle<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("TaskHandle").finish_non_exhaustive()
    }
}

/// A task could not start, was cancelled, or panicked.
pub enum TaskError {
    /// The runtime could not start the task.
    Spawn(std::io::Error),
    /// Cancellation stopped the task before it completed.
    Cancelled,
    /// The task unwound with this original panic payload.
    Panicked(Box<dyn Any + Send + 'static>),
}

impl TaskError {
    /// Whether cancellation stopped the task.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    /// Whether the task unwound.
    #[must_use]
    pub fn is_panic(&self) -> bool {
        matches!(self, Self::Panicked(_))
    }

    /// Recover the task's original panic payload.
    ///
    /// # Panics
    ///
    /// Unwinds with this error if the task did not panic.
    #[must_use]
    pub fn into_panic(self) -> Box<dyn Any + Send + 'static> {
        match self {
            Self::Panicked(payload) => payload,
            Self::Cancelled | Self::Spawn(_) => std::panic::resume_unwind(Box::new(self)),
        }
    }
}

impl fmt::Debug for TaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => formatter
                .debug_tuple("TaskError::Spawn")
                .field(error)
                .finish(),
            Self::Cancelled => formatter.write_str("TaskError::Cancelled"),
            Self::Panicked(_) => formatter
                .debug_tuple("TaskError::Panicked")
                .finish_non_exhaustive(),
        }
    }
}

impl fmt::Display for TaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(formatter, "task could not start: {error}"),
            Self::Cancelled => formatter.write_str("spawned task was cancelled"),
            Self::Panicked(_) => formatter.write_str("spawned task panicked"),
        }
    }
}

impl std::error::Error for TaskError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            Self::Cancelled | Self::Panicked(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use futures_channel::oneshot;

    use super::RuntimeExt as _;
    use crate::driver::AsyncIoRuntime;

    static_assertions::assert_impl_all!(super::TaskHandle: Send, Sync, Unpin);

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn typed_tasks_retain_results_and_panic_payloads() {
        let runtime = AsyncIoRuntime;
        assert_eq!(runtime.spawn_value(async { 42 }).await.unwrap(), 42);

        let error = runtime
            .spawn_value(async { std::panic::panic_any(47_u32) })
            .await
            .expect_err("the task panics");
        assert!(error.is_panic());
        assert_eq!(error.into_panic().downcast_ref::<u32>(), Some(&47));
    }

    #[tokio::test]
    async fn dropping_a_typed_handle_detaches_its_task() {
        let runtime = AsyncIoRuntime;
        let (release, released) = oneshot::channel();
        let (finished, finish) = oneshot::channel();
        let task = runtime.spawn_value(async move {
            released
                .await
                .expect("the caller releases the detached task");
            let _ = finished.send(());
            42
        });
        drop(task);
        release.send(()).expect("the task remains alive");
        tokio::time::timeout(TEST_TIMEOUT, finish)
            .await
            .expect("the detached task finishes")
            .expect("the detached task sends its result");
    }

    #[tokio::test]
    async fn aborting_a_typed_task_waits_for_its_future_to_drop() {
        struct Dropped(Arc<AtomicBool>);

        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let runtime = AsyncIoRuntime;
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = Dropped(Arc::clone(&dropped));
        let (started, start) = oneshot::channel();
        let task = runtime.spawn_value(async move {
            let _guard = guard;
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        tokio::time::timeout(TEST_TIMEOUT, start)
            .await
            .expect("the task starts")
            .expect("the task announces its start");
        task.abort();
        let error = tokio::time::timeout(TEST_TIMEOUT, task)
            .await
            .expect("the task is cancelled")
            .expect_err("the task does not complete");
        assert!(error.is_cancelled());
        assert!(dropped.load(Ordering::Acquire));
    }
}
