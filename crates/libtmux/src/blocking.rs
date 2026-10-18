//! Drive client operations from ordinary synchronous code.
//!
//! [`crate::WaitExt::wait`] runs one operation. [`Runtime`] retains the older
//! `run` spelling for callers that drive a sequence of futures. Supervisors
//! belong to the server's driver and continue running between calls.
//!
//! ```no_run
//! use libtmux::{Server, WaitExt as _};
//!
//! let server = Server::new()?;
//! for session in server.sessions().wait()? {
//!     println!("{}", session.id());
//! }
//! server.shutdown().wait()?;
//! # Ok::<(), libtmux::Error>(())
//! ```

use std::cell::Cell;
use std::future::Future;

use crate::Error;

thread_local! { static RUNNING: Cell<bool> = const { Cell::new(false) }; }

struct Entered;
impl Drop for Entered {
    fn drop(&mut self) {
        RUNNING.set(false);
    }
}

/// Drives futures on the calling thread without constructing a Tokio runtime.
///
/// ```
/// let runtime = libtmux::blocking::Runtime::new()?;
/// assert_eq!(runtime.run(async { 4 }), 4);
/// # Ok::<(), libtmux::Error>(())
/// ```
#[derive(Debug, Default)]
pub struct Runtime {
    _private: (),
}

impl Runtime {
    /// Construct an inert blocking driver.
    ///
    /// # Errors
    ///
    /// This constructor currently always succeeds. Driver startup errors are
    /// returned by the first operation that needs a background supervisor.
    pub fn new() -> Result<Self, Error> {
        Ok(Self::default())
    }

    /// Drive a future to completion on this thread.
    ///
    /// # Panics
    ///
    /// Panics if called recursively through this blocking driver.
    #[allow(
        clippy::panic,
        reason = "the infallible compatibility method documents nested-call panic"
    )]
    pub fn run<F: Future>(&self, future: F) -> F::Output {
        match self.try_run(future) {
            Ok(value) => value,
            Err(error) => panic!("{error}"),
        }
    }

    /// Drive a future or return an error for recursive blocking calls.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RuntimeNested`] if this thread is already inside `run`.
    pub fn try_run<F: Future>(&self, future: F) -> Result<F::Output, Error> {
        if RUNNING.replace(true) {
            return Err(Error::RuntimeNested);
        }
        let _entered = Entered;
        Ok(futures_lite::future::block_on(future))
    }
}
