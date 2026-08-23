//! Running the async API without writing an async program.
//!
//! Most tmux code is a script, a test fixture, or a small tool. Requiring
//! `#[tokio::main]` and `.await` for that is a tax with nothing bought: the
//! transport is a subprocess, so there is no concurrency to gain at that
//! scale.
//!
//! This is a runtime, not a mirror of the API. Mirroring would mean a second
//! copy of every method to keep in step, and a caller who reaches past its
//! edge would have two servers and no way to combine them. A runtime covers
//! every method that exists now and every one added later.
//!
//! ```no_run
//! use libtmux::blocking::Runtime;
//!
//! let runtime = Runtime::new()?;
//! let server = libtmux::Server::new()?;
//!
//! let sessions = runtime.run(server.sessions())?;
//! for session in &sessions {
//!     println!("{}", session.id());
//! }
//!
//! runtime.run(server.shutdown())?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::future::Future;

use crate::Error;

/// A runtime for calling this crate's async methods from ordinary code.
///
/// One runtime serves any number of servers and handles. Keep it alive for as
/// long as you use them: the executor reaps tmux child processes on it, so
/// dropping it early turns deterministic cleanup into best-effort cleanup.
#[derive(Debug)]
pub struct Runtime {
    /// Emptied only by [`Drop`], which needs the runtime by value to shut it
    /// down without blocking. Every other method takes `&self`, and the borrow
    /// checker does not hand one out during a drop, so no caller can observe
    /// the gap.
    inner: Option<tokio::runtime::Runtime>,
}

impl Runtime {
    /// Build a single-threaded runtime.
    ///
    /// Single-threaded is the right default here. The work is waiting on
    /// subprocesses, so extra threads buy nothing and would make a script's
    /// behavior depend on how many cores it happened to run on.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating system refuses the resources a
    /// runtime needs.
    pub fn new() -> Result<Self, Error> {
        let inner = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(Error::runtime_unavailable)?;

        Ok(Self { inner: Some(inner) })
    }

    /// Run one future to completion.
    ///
    /// # Panics
    ///
    /// Panics when called from inside an async context, because a runtime
    /// cannot be driven from within another. Await the future directly there.
    pub fn run<F: Future>(&self, future: F) -> F::Output {
        self.runtime().block_on(future)
    }

    /// Run one future to completion, or say why it cannot be run here.
    ///
    /// Same as [`run`], except that being inside an async context is returned
    /// rather than raised. This exists because the callers this module is for
    /// are the ones most likely to hit it: a script that grows a `#[tokio::main]`,
    /// or a helper written for a script and later called from an async test.
    ///
    /// [`run`]: Self::run
    ///
    /// # Errors
    ///
    /// Returns [`Error::RuntimeNested`] when called from inside an async
    /// context. Await the future directly there.
    ///
    /// # Examples
    ///
    /// Outside an async context it runs the future and gives back its value:
    ///
    /// ```
    /// use libtmux::blocking::Runtime;
    ///
    /// let runtime = Runtime::new()?;
    /// let answer = runtime.try_run(async { 2 + 2 })?;
    /// assert_eq!(answer, 4);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// Inside one it says so, where [`run`] would panic:
    ///
    /// ```
    /// use libtmux::{Error, ErrorKind};
    /// use libtmux::blocking::Runtime;
    ///
    /// let runtime = Runtime::new()?;
    /// let nested = runtime.run(async { runtime.try_run(async { 2 + 2 }) });
    ///
    /// assert!(matches!(nested, Err(Error::RuntimeNested)));
    /// assert_eq!(nested.unwrap_err().kind(), ErrorKind::InvalidInput);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn try_run<F: Future>(&self, future: F) -> Result<F::Output, Error> {
        // Any runtime handle in scope means `block_on` would panic, including
        // one that is not this runtime.
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(Error::RuntimeNested);
        }
        Ok(self.runtime().block_on(future))
    }

    /// Borrow the runtime this value owns for its whole life.
    fn runtime(&self) -> &tokio::runtime::Runtime {
        let Some(runtime) = self.inner.as_ref() else {
            unreachable!("only Drop takes the runtime, and it holds &mut self")
        };
        runtime
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let Some(runtime) = self.inner.take() else {
            return;
        };

        // Dropping a tokio runtime blocks until its tasks stop, and blocking
        // is forbidden inside another runtime, so an ordinary drop there ends
        // the caller's process. That caller is the one `try_run` exists for:
        // it told them they were nested, they handled the error, and the value
        // that told them went out of scope on the next line.
        //
        // Shutting down in the background gives that case up on waiting for
        // the executor to reap its tmux children, which is the cost of not
        // aborting. A drop outside an async context still waits.
        if tokio::runtime::Handle::try_current().is_ok() {
            runtime.shutdown_background();
        }
    }
}
