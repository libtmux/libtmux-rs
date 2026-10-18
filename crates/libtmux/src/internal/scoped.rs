use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use crate::{CancellationToken, Error, ScopeError};

pub(crate) fn run<R, T, E, Create, Cleanup, Operation>(
    cancellation: &CancellationToken,
    operation_name: &'static str,
    create: Create,
    cleanup: Cleanup,
    operation: Operation,
) -> Result<T, ScopeError<T, E>>
where
    R: Clone,
    Create: FnOnce() -> Result<R, Error>,
    Cleanup: FnOnce(R) -> Result<(), Error>,
    Operation: FnOnce(&R) -> Result<T, E>,
{
    if cancellation.is_cancelled() {
        return Err(ScopeError::Cancelled { cleanup: None });
    }
    let created = create().map_err(|error| {
        if matches!(error, Error::Cancelled { .. }) {
            ScopeError::Cancelled { cleanup: None }
        } else {
            ScopeError::Creation(error)
        }
    })?;
    let cleanup = ScopeCleanup {
        resource: Some(created.clone()),
        cleanup: Some(cleanup),
    };
    if cancellation.is_cancelled() {
        return Err(ScopeError::Cancelled {
            cleanup: cleanup
                .finish()
                .err()
                .map(|error| error.after_effect(operation_name)),
        });
    }
    let outcome = operation(&created);

    match (outcome, cleanup.finish()) {
        (outcome, Ok(())) => outcome.map_err(ScopeError::Operation),
        (Ok(value), Err(error)) => Err(ScopeError::Cleanup {
            value,
            cleanup: error.after_effect(operation_name),
        }),
        (Err(operation), Err(cleanup)) => Err(ScopeError::OperationAndCleanup {
            operation,
            cleanup: cleanup.after_effect(operation_name),
        }),
    }
}

struct ScopeCleanup<R, Cleanup: FnOnce(R) -> Result<(), Error>> {
    resource: Option<R>,
    cleanup: Option<Cleanup>,
}

impl<R, Cleanup: FnOnce(R) -> Result<(), Error>> ScopeCleanup<R, Cleanup> {
    fn finish(mut self) -> Result<(), Error> {
        match (self.resource.take(), self.cleanup.take()) {
            (Some(resource), Some(cleanup)) => cleanup(resource),
            _ => Ok(()),
        }
    }
}

impl<R, Cleanup: FnOnce(R) -> Result<(), Error>> Drop for ScopeCleanup<R, Cleanup> {
    fn drop(&mut self) {
        let (Some(resource), Some(cleanup)) = (self.resource.take(), self.cleanup.take()) else {
            return;
        };
        let unwinding = std::thread::panicking();
        match catch_unwind(AssertUnwindSafe(|| cleanup(resource))) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => trace_cleanup_failure(&error),
            Err(payload) if !unwinding => resume_unwind(payload),
            // A cleanup panic must not replace the operation's panic or abort
            // the process by unwinding twice on the same thread.
            Err(_) => {
                #[cfg(feature = "tracing")]
                tracing::debug!("scoped operation cleanup panicked during unwinding");
            }
        }
    }
}

fn trace_cleanup_failure(error: &impl std::fmt::Display) {
    #[cfg(feature = "tracing")]
    tracing::debug!(error = %error, "scoped operation cleanup failed");
    #[cfg(not(feature = "tracing"))]
    let _ = error;
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::panic::{AssertUnwindSafe, catch_unwind, panic_any};
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::{Command, Error, ErrorKind, ObjectKind, ScopeError};

    #[cfg(feature = "tracing")]
    use tracing::subscriber::Subscriber;
    #[cfg(feature = "tracing")]
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
    #[cfg(feature = "tracing")]
    use tracing_subscriber::registry::LookupSpan;

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    #[test]
    fn cancellation_before_creation_does_not_create_a_resource() {
        let token = crate::CancellationToken::new();
        token.cancel();
        let created = Cell::new(false);
        let outcome = super::run(
            &token,
            "with-pane",
            || {
                created.set(true);
                Ok::<_, Error>(())
            },
            |()| panic!("no resource to clean"),
            |()| -> Result<(), Error> { panic!("the callback must not run") },
        );
        assert!(matches!(
            outcome,
            Err(ScopeError::Cancelled { cleanup: None })
        ));
        assert!(!created.get());
    }

    #[test]
    fn cancellation_during_creation_cleans_the_created_resource() {
        let token = crate::CancellationToken::new();
        let during_creation = token.clone();
        let cleaned = Cell::new(0);
        let outcome = super::run(
            &token,
            "with-pane",
            move || {
                during_creation.cancel();
                Ok::<_, Error>(())
            },
            |()| {
                cleaned.set(cleaned.get() + 1);
                Ok(())
            },
            |()| -> Result<(), Error> { panic!("the callback must not run") },
        );
        assert!(matches!(
            outcome,
            Err(ScopeError::Cancelled { cleanup: None })
        ));
        assert_eq!(cleaned.get(), 1);
    }

    #[test]
    fn cancellation_retains_a_cleanup_failure_after_creation() {
        let token = crate::CancellationToken::new();
        let during_creation = token.clone();
        let outcome = super::run(
            &token,
            "with-pane",
            move || {
                during_creation.cancel();
                Ok::<_, Error>(())
            },
            |()| {
                Err(Error::Overloaded {
                    request_id: 10,
                    command: Command::new("kill-pane").summary(),
                    in_flight: 1,
                })
            },
            |()| -> Result<(), Error> { panic!("the callback must not run") },
        );
        let Err(ScopeError::Cancelled {
            cleanup: Some(cleanup),
        }) = outcome
        else {
            panic!("cancellation retains cleanup failure");
        };
        assert!(matches!(cleanup,
            Error::AfterEffect { operation: "with-pane", source }
                if matches!(*source, Error::Overloaded { .. })
        ));
    }

    #[test]
    fn cleanup_failure_after_a_successful_operation_marks_the_effect() {
        let error = super::run(
            &crate::CancellationToken::new(),
            "with-pane",
            || Ok::<_, Error>(()),
            |()| {
                Err(Error::Overloaded {
                    request_id: 9,
                    command: Command::new("kill-pane").summary(),
                    in_flight: 1,
                })
            },
            |()| Ok::<_, Error>(()),
        )
        .expect_err("cleanup fails after the scoped operation succeeded");
        let ScopeError::Cleanup {
            value: (),
            cleanup: error,
        } = error
        else {
            panic!("cleanup failed after the operation succeeded");
        };
        assert_eq!(error.kind(), ErrorKind::PartialEffect);
        assert!(matches!(error,
            Error::AfterEffect { operation: "with-pane", source }
                if source.kind() == ErrorKind::Refused && source.is_transient()
        ));
    }

    #[test]
    fn cleanup_failure_owns_the_replay_boundary_when_the_operation_also_fails() {
        let error = super::run(
            &crate::CancellationToken::new(),
            "with-window",
            || Ok::<_, Error>(()),
            |()| {
                Err(Error::Overloaded {
                    request_id: 10,
                    command: Command::new("kill-window").summary(),
                    in_flight: 1,
                })
            },
            |()| {
                Err::<(), Error>(Error::ObjectGone {
                    kind: ObjectKind::Window,
                    id: String::from("@1"),
                })
            },
        )
        .expect_err("both operation and cleanup fail");
        let ScopeError::OperationAndCleanup { operation, cleanup } = error else {
            panic!("operation and cleanup both failed");
        };
        assert!(matches!(
            operation,
            Error::ObjectGone {
                kind: ObjectKind::Window,
                ..
            }
        ));
        assert!(matches!(cleanup,
            Error::AfterEffect { operation: "with-window", source }
                if matches!(*source, Error::Overloaded { .. })
        ));
    }

    #[test]
    fn a_borrowed_operation_can_keep_non_send_state_and_results() {
        let value = Rc::new(Cell::new(0));
        let result = super::run(
            &crate::CancellationToken::new(),
            "with-pane",
            || Ok::<_, Error>(()),
            |()| Ok(()),
            |()| {
                value.set(4);
                Ok::<_, Rc<str>>(Rc::clone(&value))
            },
        )
        .expect("a non-Send result");
        assert!(Rc::ptr_eq(&result, &value));
        assert_eq!(result.get(), 4);
    }

    #[test]
    fn an_operation_panic_retains_its_payload_and_cleans_exactly_once() {
        let calls = Cell::new(0);
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let _ = super::run(
                &crate::CancellationToken::new(),
                "with-pane",
                || Ok::<_, Error>(()),
                |()| {
                    calls.set(calls.get() + 1);
                    Ok(())
                },
                |()| -> Result<(), Error> { panic_any(812_u32) },
            );
        }));
        assert_eq!(
            *outcome
                .expect_err("the body panics")
                .downcast::<u32>()
                .expect("original payload"),
            812
        );
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn a_cleanup_panic_during_unwinding_does_not_replace_the_operation_panic() {
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let _ = super::run(
                &crate::CancellationToken::new(),
                "with-pane",
                || Ok::<_, Error>(()),
                |()| -> Result<(), Error> { panic_any("cleanup") },
                |()| -> Result<(), Error> { panic_any("operation") },
            );
        }));
        assert_eq!(
            *outcome
                .expect_err("the body panics")
                .downcast::<&str>()
                .expect("original payload"),
            "operation"
        );
    }

    #[test]
    fn detaching_a_calling_thread_during_creation_retains_cleanup() {
        let (started, creation_started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (cleaned, cleanup_done) = mpsc::channel();
        let operation_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&operation_calls);
        let thread = std::thread::spawn(move || {
            let _ = super::run(
                &crate::CancellationToken::new(),
                "with-session",
                move || {
                    started.send(()).expect("creation observed");
                    released
                        .recv_timeout(TEST_TIMEOUT)
                        .expect("creation released");
                    Ok::<_, Error>(())
                },
                move |()| {
                    cleaned.send(()).expect("cleanup observed");
                    Ok(())
                },
                move |()| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, Error>(())
                },
            );
        });
        creation_started
            .recv_timeout(TEST_TIMEOUT)
            .expect("creation starts");
        drop(thread);
        release.send(()).expect("creation continues");
        cleanup_done
            .recv_timeout(TEST_TIMEOUT)
            .expect("detached call cleans up");
        assert_eq!(
            operation_calls.load(Ordering::SeqCst),
            1,
            "detaching a native thread does not cancel a synchronous callback"
        );
    }

    #[cfg(feature = "tracing")]
    #[derive(Clone, Default)]
    struct CleanupFailures {
        count: Arc<AtomicUsize>,
    }

    #[cfg(feature = "tracing")]
    impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for CleanupFailures {
        fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
            struct Message(bool);
            impl tracing::field::Visit for Message {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message"
                        && format!("{value:?}").contains("scoped operation cleanup failed")
                    {
                        self.0 = true;
                    }
                }
            }
            let mut message = Message(false);
            event.record(&mut message);
            if message.0 {
                self.count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn an_observed_cleanup_failure_is_not_traced() {
        let failures = CleanupFailures::default();
        let subscriber = tracing_subscriber::registry().with(failures.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let outcome = super::run(
            &crate::CancellationToken::new(),
            "with-pane",
            || Ok::<_, Error>(()),
            |()| {
                Err(Error::ObjectGone {
                    kind: ObjectKind::Session,
                    id: String::from("$observed"),
                })
            },
            |()| Ok::<_, Error>(()),
        );
        assert!(outcome.is_err());
        assert_eq!(failures.count.load(Ordering::Relaxed), 0);
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn an_unobserved_cleanup_failure_during_unwinding_is_traced_once() {
        let failures = CleanupFailures::default();
        let subscriber = tracing_subscriber::registry().with(failures.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let _ = super::run(
                &crate::CancellationToken::new(),
                "with-pane",
                || Ok::<_, Error>(()),
                |()| {
                    Err(Error::ObjectGone {
                        kind: ObjectKind::Session,
                        id: String::from("$unobserved"),
                    })
                },
                |()| -> Result<(), Error> { panic_any("operation") },
            );
        }));
        assert!(outcome.is_err());
        assert_eq!(failures.count.load(Ordering::Relaxed), 1);
    }
}
