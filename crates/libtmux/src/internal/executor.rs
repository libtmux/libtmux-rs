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

/// The span one dispatch runs in, from admission to its outcome.
///
/// No field may carry an argument: a sensitive one would reach every
/// subscriber. Without the `tracing` feature this is empty and [`Self::run`]
/// adds nothing to the dispatch.
pub(crate) struct DispatchSpan {
    #[cfg(feature = "tracing")]
    span: tracing::Span,
}

impl DispatchSpan {
    /// Open the span before `request` is consumed.
    pub(crate) fn new(request: &CommandRequest, transport: &'static str) -> Self {
        #[cfg(not(feature = "tracing"))]
        let _ = (request, transport);
        Self {
            #[cfg(feature = "tracing")]
            span: tracing::debug_span!(
                "tmux_command",
                request_id = request.request_id().get(),
                subcommand = request.summary().subcommand(),
                transport,
                outcome = tracing::field::Empty,
                error_kind = tracing::field::Empty,
            ),
        }
    }

    /// Run `future` inside the span and record how it ended.
    #[cfg_attr(
        not(feature = "tracing"),
        allow(clippy::unused_self, reason = "the span is compiled out")
    )]
    pub(crate) fn run(
        self,
        future: impl Future<Output = Result<CommandResult, Error>> + Send + 'static,
    ) -> DispatchFuture {
        #[cfg(feature = "tracing")]
        {
            use tracing::Instrument as _;

            let mut outcome = OutcomeRecorder {
                span: self.span.clone(),
                recorded: false,
            };
            DispatchFuture::new(
                async move {
                    let result = future.await;
                    outcome.record(&result);
                    result
                }
                .instrument(self.span),
            )
        }
        #[cfg(not(feature = "tracing"))]
        DispatchFuture::new(future)
    }
}

/// Records a dispatch's outcome once, or `cancelled` if it is dropped first.
#[cfg(feature = "tracing")]
struct OutcomeRecorder {
    span: tracing::Span,
    recorded: bool,
}

#[cfg(feature = "tracing")]
impl OutcomeRecorder {
    fn record(&mut self, result: &Result<CommandResult, Error>) {
        match result {
            Ok(result) if result.success() => self.span.record("outcome", "success"),
            Ok(_) => self.span.record("outcome", "exit"),
            Err(error) => self
                .span
                .record("outcome", "error")
                .record("error_kind", tracing::field::debug(error.kind())),
        };
        self.recorded = true;
    }
}

#[cfg(feature = "tracing")]
impl Drop for OutcomeRecorder {
    fn drop(&mut self) {
        if !self.recorded {
            self.span.record("outcome", "cancelled");
        }
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
