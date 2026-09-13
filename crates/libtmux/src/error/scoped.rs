use std::fmt;

use super::Error;

/// A scoped resource's creation, operation, or cleanup failure.
///
/// Returned by [`crate::Server::with_session`],
/// [`crate::Session::with_window`], and [`crate::Window::with_pane`]. The
/// operation error keeps its original type and value, with no `From<Error>`
/// requirement. Cleanup failures retain [`Error::AfterEffect`] because
/// creation succeeded; an operation error alone makes no replay guarantee.
///
/// `Debug` and `Display` withhold the operation error's contents. Inspect its
/// variant to retrieve it. [`std::error::Error::source`] exposes creation or
/// cleanup errors. The operation value is available through its variant,
/// since its generic type need not implement [`std::error::Error`].
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::ScopeError;
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let outcome = guard.server().with_session("work", async |_session| {
///     Err::<(), _>("operation failed")
/// }).await;
/// assert!(matches!(outcome, Err(ScopeError::Operation("operation failed"))));
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
pub enum ScopeError<E> {
    /// The resource could not be created; the operation did not run.
    Creation(Error),
    /// The operation failed and cleanup succeeded.
    Operation(E),
    /// The operation succeeded, but cleanup failed after creation.
    Cleanup(Error),
    /// The operation and cleanup both failed.
    OperationAndCleanup {
        /// The caller's original operation error.
        operation: E,
        /// The cleanup error, marked as [`Error::AfterEffect`].
        cleanup: Error,
    },
}

impl<E> fmt::Debug for ScopeError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Creation(error) => formatter.debug_tuple("Creation").field(error).finish(),
            Self::Operation(_) => formatter.write_str("Operation(<redacted>)"),
            Self::Cleanup(error) => formatter.debug_tuple("Cleanup").field(error).finish(),
            Self::OperationAndCleanup { cleanup, .. } => formatter
                .debug_struct("OperationAndCleanup")
                .field("operation", &"<redacted>")
                .field("cleanup", cleanup)
                .finish(),
        }
    }
}

impl<E> fmt::Display for ScopeError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Creation(error) => write!(formatter, "scoped resource creation failed: {error}"),
            Self::Operation(_) => formatter.write_str("scoped operation failed"),
            Self::Cleanup(error) => write!(formatter, "scoped resource cleanup failed: {error}"),
            Self::OperationAndCleanup { cleanup, .. } => {
                write!(formatter, "scoped operation and cleanup failed: {cleanup}")
            }
        }
    }
}

impl<E> std::error::Error for ScopeError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Creation(error) | Self::Cleanup(error) => Some(error),
            Self::Operation(_) => None,
            Self::OperationAndCleanup { cleanup, .. } => Some(cleanup),
        }
    }
}
