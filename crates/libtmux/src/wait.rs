use std::future::Future;

/// Block the current thread until an operation completes.
///
/// This applies to every client operation, including borrowed results and
/// scoped callbacks. The server's non-Tokio driver owns timers, I/O, and
/// background cleanup independently of this waiter. In async code, use `.await`.
///
/// ```no_run
/// use libtmux::{Server, WaitExt as _};
/// let server = Server::new()?;
/// let sessions = server.sessions().wait()?;
/// assert!(sessions.iter().all(|session| session.id().as_ref().starts_with('$')));
/// server.shutdown().wait()?;
/// # Ok::<(), libtmux::Error>(())
/// ```
pub trait WaitExt: Future + Sized {
    /// Run this future to completion on the current thread.
    fn wait(self) -> Self::Output {
        futures_lite::future::block_on(self)
    }
}

impl<F: Future> WaitExt for F {}
