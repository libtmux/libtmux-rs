use std::fmt;

use super::Error;

/// A scoped resource's creation, operation, or cleanup failure.
///
/// Returned by [`crate::Server::with_session`],
/// [`crate::Session::with_window`], and [`crate::Window::with_pane`]. The
/// operation error keeps its original type and value, with no `From<Error>`
/// requirement. Cleanup failures retain [`Error::AfterEffect`] because
/// creation succeeded; an operation error alone makes no replay guarantee.
/// [`Self::Cleanup`] retains the operation's own result too: it already
/// computed `T` when cleanup failed, and matching the variant is the only way
/// to reach it, since the outer `Result` is `Err` either way.
///
/// `Debug`, `Display`, and [`std::error::Error`] are implemented for every
/// `T` and `E`, but each only shows a value when its type supports it:
/// `Debug` needs `T: Debug` and `E: Debug`, `Display` needs `E: Display`, and
/// `Error` needs both, since it requires them as supertraits. A caller whose
/// types have neither still gets a working scope: both values remain
/// reachable by matching the variant, and creation and cleanup failures
/// format and chain regardless.
///
/// [`std::error::Error::source`] exposes the cleanup error in
/// [`Self::Cleanup`] and [`Self::OperationAndCleanup`], and the creation
/// error in [`Self::Creation`]. It never exposes the operation error: `E`
/// need not implement [`std::error::Error`] at all, so there is no
/// `&(dyn Error + 'static)` to hand back even when `Display` can show it.
/// Match the variant to reach it directly.
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
pub enum ScopeError<T, E> {
    /// The resource could not be created; the operation did not run.
    Creation(Error),
    /// The operation failed and cleanup succeeded.
    Operation(E),
    /// The operation succeeded, but cleanup failed after creation.
    Cleanup {
        /// The operation's own result, since the caller cannot reach it any
        /// other way once cleanup fails.
        value: T,
        /// The cleanup error, marked as [`Error::AfterEffect`].
        cleanup: Error,
    },
    /// The operation and cleanup both failed.
    OperationAndCleanup {
        /// The caller's original operation error.
        operation: E,
        /// The cleanup error, marked as [`Error::AfterEffect`].
        cleanup: Error,
    },
}

impl<T: fmt::Debug, E: fmt::Debug> fmt::Debug for ScopeError<T, E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Creation(error) => formatter.debug_tuple("Creation").field(error).finish(),
            Self::Operation(error) => formatter.debug_tuple("Operation").field(error).finish(),
            Self::Cleanup { value, cleanup } => formatter
                .debug_struct("Cleanup")
                .field("value", value)
                .field("cleanup", cleanup)
                .finish(),
            Self::OperationAndCleanup { operation, cleanup } => formatter
                .debug_struct("OperationAndCleanup")
                .field("operation", operation)
                .field("cleanup", cleanup)
                .finish(),
        }
    }
}

impl<T, E: fmt::Display> fmt::Display for ScopeError<T, E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Creation(error) => write!(formatter, "scoped resource creation failed: {error}"),
            Self::Operation(error) => write!(formatter, "scoped operation failed: {error}"),
            Self::Cleanup { cleanup, .. } => {
                write!(formatter, "scoped resource cleanup failed: {cleanup}")
            }
            Self::OperationAndCleanup { operation, cleanup } => write!(
                formatter,
                "scoped operation failed: {operation}; cleanup also failed: {cleanup}"
            ),
        }
    }
}

impl<T: fmt::Debug, E: fmt::Debug + fmt::Display> std::error::Error for ScopeError<T, E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Creation(error) => Some(error),
            Self::Cleanup { cleanup, .. } | Self::OperationAndCleanup { cleanup, .. } => {
                Some(cleanup)
            }
            // `E` is not required to implement `Error`, so there is no
            // `&(dyn Error + 'static)` to return here even though `Debug`
            // and `Display` can show it.
            Self::Operation(_) => None,
        }
    }
}
