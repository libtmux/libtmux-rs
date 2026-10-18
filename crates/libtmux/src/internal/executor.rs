use crate::Error;
use crate::command::{CommandRequest, CommandResult};

/// A deferred blocking dispatch, started only by `run`.
#[must_use = "call run to dispatch the command"]
pub(crate) struct DispatchFuture(Box<dyn FnOnce() -> Result<CommandResult, Error> + Send>);
impl DispatchFuture {
    pub(crate) fn new(run: impl FnOnce() -> Result<CommandResult, Error> + Send + 'static) -> Self {
        Self(Box::new(run))
    }
    pub(crate) fn run(self) -> Result<CommandResult, Error> {
        (self.0)()
    }
}

pub(crate) struct DispatchSpan {
    #[cfg(feature = "tracing")]
    span: tracing::Span,
}
impl DispatchSpan {
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
                error_kind = tracing::field::Empty
            ),
        }
    }
    pub(crate) fn run(
        self,
        run: impl FnOnce() -> Result<CommandResult, Error> + Send + 'static,
    ) -> DispatchFuture {
        #[cfg(not(feature = "tracing"))]
        let _ = self;
        DispatchFuture::new(move || {
            #[cfg(feature = "tracing")]
            let _entered = self.span.enter();
            #[cfg(feature = "tracing")]
            let mut outcome = OutcomeRecorder {
                span: self.span.clone(),
                recorded: false,
            };
            let result = run();
            #[cfg(feature = "tracing")]
            outcome.record(&result);
            result
        })
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

#[must_use = "call run to shut down the executor"]
pub(crate) struct ShutdownFuture(Box<dyn FnOnce() -> Result<(), Error> + Send>);
impl ShutdownFuture {
    pub(crate) fn new(run: impl FnOnce() -> Result<(), Error> + Send + 'static) -> Self {
        Self(Box::new(run))
    }
    pub(crate) fn run(self) -> Result<(), Error> {
        (self.0)()
    }
}

pub(crate) trait Executor: Send + Sync + 'static {
    fn execute(&self, request: CommandRequest) -> DispatchFuture;
    fn shutdown(&self) -> ShutdownFuture;
    #[cfg(feature = "control-mode")]
    fn renders_control_line(&self) -> bool {
        false
    }
}
