use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::Error;
use crate::command::{CommandRequest, CommandResult};

type BoxedDispatch = Pin<Box<dyn Future<Output = Result<CommandResult, Error>> + Send + 'static>>;
type BoxedShutdown = Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'static>>;

#[must_use = "dispatch futures do nothing unless awaited"]
pub(crate) struct DispatchFuture(BoxedDispatch);

impl DispatchFuture {
    pub(crate) fn new(
        future: impl Future<Output = Result<CommandResult, Error>> + Send + 'static,
    ) -> Self {
        Self(Box::pin(future))
    }
}

impl Future for DispatchFuture {
    type Output = Result<CommandResult, Error>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.as_mut().poll(context)
    }
}

#[must_use = "shutdown futures do nothing unless awaited"]
pub(crate) struct ShutdownFuture(BoxedShutdown);

impl ShutdownFuture {
    pub(crate) fn new(future: impl Future<Output = Result<(), Error>> + Send + 'static) -> Self {
        Self(Box::pin(future))
    }
}

impl Future for ShutdownFuture {
    type Output = Result<(), Error>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.as_mut().poll(context)
    }
}

/// How a built command reaches tmux.
///
/// Not every path to tmux comes through here, and the exception matters when
/// reasoning about what installing an executor changes:
/// [`crate::internal::core::Core::spawn_control`] builds a `CommandRequest`
/// and then hands it to [`crate::internal::process::PersistentChild::spawn`]
/// directly, using the launch context rather than this trait. A control-mode
/// connection is a long-lived child with its own protocol on its pipes, not a
/// request with one answer, so it has nothing to return through
/// `DispatchFuture`. The consequence: opening a connection always forks tmux,
/// including the connection that a `ControlModeExecutor` then dispatches over.
pub(crate) trait Executor: Send + Sync + 'static {
    fn execute(&self, request: CommandRequest) -> DispatchFuture;

    fn shutdown(&self) -> ShutdownFuture;

    /// Whether this executor dispatches a text line rather than an argv.
    ///
    /// Asked before a request is built, so the line is rendered only for the
    /// one transport that reads it. A subprocess executor pays nothing.
    #[cfg(feature = "control-mode")]
    fn renders_control_line(&self) -> bool {
        false
    }
}
