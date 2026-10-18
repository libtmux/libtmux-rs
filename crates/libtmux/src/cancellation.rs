use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

type Callback = Arc<dyn Fn() + Send + Sync + 'static>;

#[derive(Default)]
struct State {
    cancelled: AtomicBool,
    callbacks: Mutex<Vec<Callback>>,
}

/// Cooperative cancellation shared by a server and its callers.
///
/// Pass a clone to [`crate::ServerBuilder::cancellation_token`]. Cancelling
/// closes admission for that server and asks active client work to stop. A
/// scoped callback already running checks [`Self::is_cancelled`] at its own
/// stopping points; cancellation does not interrupt a native thread.
#[derive(Clone, Default)]
pub struct CancellationToken {
    state: Arc<State>,
}

impl CancellationToken {
    /// Create a token that has not been cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation for every clone of this token.
    ///
    /// Repeated calls have no further effect. This signals client work; use
    /// [`crate::Server::shutdown`] to wait for its cleanup.
    pub fn cancel(&self) {
        let callbacks = {
            let mut callbacks = self
                .state
                .callbacks
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if self.state.cancelled.swap(true, Ordering::AcqRel) {
                return;
            }
            std::mem::take(&mut *callbacks)
        };
        let mut panic = None;
        for callback in callbacks {
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| callback()))
                && panic.is_none()
            {
                panic = Some(payload);
            }
        }
        if let Some(payload) = panic {
            resume_unwind(payload);
        }
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    /// Register a runtime wakeup, including when cancellation already happened.
    ///
    /// The callback runs outside the registration lock. Removing a registration
    /// does not wait for a callback a cancelling thread has already taken.
    #[doc(hidden)]
    #[must_use]
    pub fn on_cancel(
        &self,
        callback: impl Fn() + Send + Sync + 'static,
    ) -> CancellationRegistration {
        let callback: Callback = Arc::new(callback);
        let cancelled = {
            let mut callbacks = self
                .state
                .callbacks
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if self.is_cancelled() {
                true
            } else {
                callbacks.push(Arc::clone(&callback));
                false
            }
        };
        if cancelled {
            callback();
        }
        CancellationRegistration {
            state: Arc::downgrade(&self.state),
            callback,
        }
    }
}

impl fmt::Debug for CancellationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// Removes a cancellation wakeup when dropped.
#[doc(hidden)]
pub struct CancellationRegistration {
    state: Weak<State>,
    callback: Callback,
}

impl fmt::Debug for CancellationRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancellationRegistration")
            .finish_non_exhaustive()
    }
}

impl Drop for CancellationRegistration {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            state
                .callbacks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|callback| !Arc::ptr_eq(callback, &self.callback));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    use super::CancellationToken;

    #[test]
    fn cancellation_and_registration_cannot_lose_each_other() {
        for _ in 0..64 {
            let token = CancellationToken::new();
            let registering = token.clone();
            let ready = Arc::new(Barrier::new(2));
            let thread_ready = Arc::clone(&ready);
            let called = Arc::new(AtomicUsize::new(0));
            let thread_called = Arc::clone(&called);
            let thread = std::thread::spawn(move || {
                thread_ready.wait();
                registering.on_cancel(move || {
                    thread_called.fetch_add(1, Ordering::SeqCst);
                })
            });
            ready.wait();
            token.cancel();
            let registration = thread.join().expect("registration completes");
            assert_eq!(called.load(Ordering::SeqCst), 1);
            token.cancel();
            assert_eq!(called.load(Ordering::SeqCst), 1);
            drop(registration);
        }
    }

    #[test]
    fn dropping_a_registration_before_cancellation_removes_it() {
        let token = CancellationToken::new();
        let called = Arc::new(AtomicUsize::new(0));
        let callback_called = Arc::clone(&called);
        let registration = token.on_cancel(move || {
            callback_called.fetch_add(1, Ordering::SeqCst);
        });
        drop(registration);
        token.cancel();
        assert_eq!(called.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cancellation_notifies_other_callbacks_before_resuming_a_panic() {
        let token = CancellationToken::new();
        let _panic = token.on_cancel(|| std::panic::panic_any(42_u32));
        let called = Arc::new(AtomicUsize::new(0));
        let callback_called = Arc::clone(&called);
        let _other = token.on_cancel(move || {
            callback_called.fetch_add(1, Ordering::SeqCst);
        });
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| token.cancel()));
        assert_eq!(
            *outcome
                .expect_err("callback panics")
                .downcast::<u32>()
                .expect("original payload"),
            42
        );
        assert_eq!(called.load(Ordering::SeqCst), 1);
        assert!(token.is_cancelled());
    }
}
