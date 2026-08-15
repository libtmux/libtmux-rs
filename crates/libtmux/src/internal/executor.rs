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

pub(crate) trait Executor: Send + Sync + 'static {
    fn execute(&self, request: CommandRequest) -> DispatchFuture;

    fn shutdown(&self) -> ShutdownFuture;
}
