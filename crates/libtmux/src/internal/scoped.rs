use std::future::Future;
use std::sync::Arc;

use tokio::sync::oneshot;

use crate::lifecycle::jobs::{Job, Journal};
use crate::{Error, ScopeError};

pub(crate) async fn run<R, T, E, Create, Cleanup, CleanupFuture, Operation>(
    operation_name: &'static str,
    create: Create,
    cleanup: Cleanup,
    operation: Operation,
) -> Result<T, ScopeError<T, E>>
where
    R: Clone + Send + 'static,
    Create: Future<Output = Result<R, Error>> + Send + 'static,
    Cleanup: FnOnce(R) -> CleanupFuture + Send + 'static,
    CleanupFuture: Future<Output = Result<(), Error>> + Send + 'static,
    Operation: AsyncFnOnce(&R) -> Result<T, E>,
{
    run_tracked(
        Journal::default(),
        operation_name,
        create,
        cleanup,
        operation,
    )
    .await
}

pub(crate) async fn run_tracked<R, T, E, Create, Cleanup, CleanupFuture, Operation>(
    journal: Journal,
    operation_name: &'static str,
    create: Create,
    cleanup: Cleanup,
    operation: Operation,
) -> Result<T, ScopeError<T, E>>
where
    R: Clone + Send + 'static,
    Create: Future<Output = Result<R, Error>> + Send + 'static,
    Cleanup: FnOnce(R) -> CleanupFuture + Send + 'static,
    CleanupFuture: Future<Output = Result<(), Error>> + Send + 'static,
    Operation: AsyncFnOnce(&R) -> Result<T, E>,
{
    let (created, cleanup) = acquire(journal, create, cleanup)
        .await
        .map_err(ScopeError::Creation)?;
    let outcome = operation(&created).await;
    match (outcome, cleanup.finish().await) {
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

pub(crate) async fn handoff<R, Create, Cleanup, CleanupFuture>(
    journal: Journal,
    create: Create,
    cleanup: Cleanup,
) -> Result<R, Error>
where
    R: Clone + Send + 'static,
    Create: Future<Output = Result<R, Error>> + Send + 'static,
    Cleanup: FnOnce(R) -> CleanupFuture + Send + 'static,
    CleanupFuture: Future<Output = Result<(), Error>> + Send + 'static,
{
    let (resource, cleanup) = acquire(journal, create, cleanup).await?;
    cleanup.disarm();
    Ok(resource)
}

pub(crate) async fn acquire_owned<T: Clone + Send + Sync + 'static>(
    journal: Journal,
    create: impl Future<Output = Result<crate::lifecycle::Owned<T>, Error>> + Send + 'static,
) -> Result<crate::lifecycle::Owned<T>, Error> {
    let (owner, cleanup) =
        acquire(journal, create, |owner| async move { owner.close().await }).await?;
    cleanup.disarm();
    Ok(owner)
}

async fn acquire<R, Create, Cleanup, CleanupFuture>(
    journal: Journal,
    create: Create,
    cleanup: Cleanup,
) -> Result<(R, ScopeCleanup), Error>
where
    R: Clone + Send + 'static,
    Create: Future<Output = Result<R, Error>> + Send + 'static,
    Cleanup: FnOnce(R) -> CleanupFuture + Send + 'static,
    CleanupFuture: Future<Output = Result<(), Error>> + Send + 'static,
{
    let (handoff, receive) = oneshot::channel();
    let acquisition = journal.begin();
    tokio::spawn(async move {
        let outcome = match tokio::spawn(create).await {
            Ok(outcome) => outcome,
            Err(error) => Err(Error::LifecycleTaskLost {
                detail: error.to_string(),
            }),
        }
        .map(|created| {
            let cleanup = ScopeCleanup::new(&journal, cleanup(created.clone()));
            (created, cleanup)
        });
        let unobserved = match handoff.send(outcome) {
            Err(Err(error)) => Err(error),
            _ => Ok(()),
        };
        acquisition.complete(unobserved);
        acquisition.discard_success();
    });
    receive.await.map_err(|error| Error::LifecycleTaskLost {
        detail: error.to_string(),
    })?
}

struct ScopeCleanup {
    release: oneshot::Sender<bool>,
    outcome: Arc<Job>,
    observed: oneshot::Sender<()>,
}

impl ScopeCleanup {
    fn new(
        journal: &Journal,
        cleanup: impl Future<Output = Result<(), Error>> + Send + 'static,
    ) -> Self {
        let (release, released) = oneshot::channel();
        let (observed, observation) = oneshot::channel();
        let outcome = journal.begin();
        let result = Arc::clone(&outcome);
        let supervisor = async move {
            let disarmed = released.await == Ok(false);
            let outcome = if disarmed {
                Ok(())
            } else {
                match tokio::spawn(cleanup).await {
                    Ok(outcome) => outcome,
                    Err(error) => Err(Error::LifecycleTaskLost {
                        detail: error.to_string(),
                    }),
                }
            };
            result.complete(outcome);
            let acknowledged = observation.await;
            if disarmed || acknowledged.is_err() {
                #[cfg(feature = "tracing")]
                result.trace_failure();
                result.discard_success();
            }
        };
        #[cfg(feature = "tracing")]
        {
            use tracing::instrument::WithSubscriber as _;
            tokio::spawn(supervisor.with_current_subscriber());
        }
        #[cfg(not(feature = "tracing"))]
        tokio::spawn(supervisor);
        Self {
            release,
            outcome,
            observed,
        }
    }

    fn disarm(self) {
        let _ = self.release.send(false);
        let _ = self.observed.send(());
    }

    async fn finish(self) -> Result<(), Error> {
        let _ = self.release.send(true);
        self.outcome.wait().await;
        let outcome = self
            .outcome
            .take()
            .ok_or_else(|| Error::LifecycleTaskLost {
                detail: "cleanup was drained while its scope was still running".to_owned(),
            })?;
        let _ = self.observed.send(());
        outcome
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use tokio::sync::Notify;

    use crate::{Command, Error, ErrorKind, ObjectKind, ScopeError};

    #[cfg(feature = "tracing")]
    use tracing::subscriber::Subscriber;
    #[cfg(feature = "tracing")]
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
    #[cfg(feature = "tracing")]
    use tracing_subscriber::registry::LookupSpan;

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    #[derive(Clone)]
    struct Resource {
        cleaned: Arc<Notify>,
    }

    #[tokio::test]
    async fn cleanup_failure_after_a_successful_operation_marks_the_effect() {
        let error = super::run(
            "with-pane",
            async {
                Ok::<_, Error>(Resource {
                    cleaned: Arc::new(Notify::new()),
                })
            },
            |_resource: Resource| async {
                Err(Error::Overloaded {
                    request_id: 9,
                    command: Command::new("kill-pane").summary(),
                    in_flight: 1,
                })
            },
            async |_resource: &Resource| Ok::<_, Error>(()),
        )
        .await
        .expect_err("cleanup fails after the scoped operation succeeded");

        let ScopeError::Cleanup {
            value: (),
            cleanup: error,
        } = error
        else {
            panic!("cleanup failed after the operation succeeded");
        };
        assert_eq!(error.kind(), ErrorKind::PartialEffect);
        assert!(
            matches!(
                error,
                Error::AfterEffect { operation: "with-pane", source }
                    if source.kind() == ErrorKind::Refused && source.is_transient()
            ),
            "cleanup keeps its diagnostic source",
        );
    }

    #[tokio::test]
    async fn cleanup_failure_owns_the_replay_boundary_when_the_operation_also_fails() {
        let error = super::run(
            "with-window",
            async {
                Ok::<_, Error>(Resource {
                    cleaned: Arc::new(Notify::new()),
                })
            },
            |_resource: Resource| async {
                Err(Error::Overloaded {
                    request_id: 10,
                    command: Command::new("kill-window").summary(),
                    in_flight: 1,
                })
            },
            async |_resource: &Resource| {
                Err::<(), Error>(Error::ObjectGone {
                    kind: ObjectKind::Window,
                    id: String::from("@1"),
                })
            },
        )
        .await
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
        assert!(matches!(
            cleanup,
            Error::AfterEffect { operation: "with-window", source }
                if matches!(*source, Error::Overloaded { .. })
        ));
    }

    #[cfg(feature = "tracing")]
    #[derive(Clone, Default)]
    struct CleanupFailures {
        seen: Arc<Notify>,
        count: Arc<std::sync::atomic::AtomicUsize>,
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
                self.seen.notify_one();
            }
        }
    }

    #[tokio::test]
    async fn cancellation_during_creation_cleans_the_created_resource() {
        let creation_started = Arc::new(Notify::new());
        let creation_release = Arc::new(Notify::new());
        let cleaned = Arc::new(Notify::new());
        let operation_polled = Arc::new(AtomicBool::new(false));

        let scope = tokio::spawn(super::run(
            "with-session",
            {
                let creation_started = Arc::clone(&creation_started);
                let creation_release = Arc::clone(&creation_release);
                let cleaned = Arc::clone(&cleaned);
                async move {
                    creation_started.notify_one();
                    creation_release.notified().await;
                    Ok::<_, Error>(Resource { cleaned })
                }
            },
            |resource: Resource| async move {
                resource.cleaned.notify_one();
                Ok(())
            },
            {
                let operation_polled = Arc::clone(&operation_polled);
                async move |_resource: &Resource| {
                    operation_polled.store(true, Ordering::Relaxed);
                    Ok::<(), Error>(())
                }
            },
        ));

        tokio::time::timeout(TEST_TIMEOUT, creation_started.notified())
            .await
            .expect("creation starts");
        scope.abort();
        assert!(scope.await.expect_err("scope is aborted").is_cancelled());

        creation_release.notify_one();
        tokio::time::timeout(TEST_TIMEOUT, cleaned.notified())
            .await
            .expect("the owned creation supervisor cleans up");
        assert!(!operation_polled.load(Ordering::Relaxed));
    }

    #[cfg(feature = "tracing")]
    #[tokio::test]
    async fn cancellation_during_cleanup_records_a_cleanup_failure() {
        let cleanup_started = Arc::new(Notify::new());
        let cleanup_release = Arc::new(Notify::new());
        let failures = CleanupFailures::default();
        let subscriber = tracing_subscriber::registry().with(failures.clone());
        let _guard = tracing::subscriber::set_default(subscriber);

        let mut scope = Box::pin(super::run(
            "with-window",
            async {
                Ok::<_, Error>(Resource {
                    cleaned: Arc::new(Notify::new()),
                })
            },
            {
                let cleanup_started = Arc::clone(&cleanup_started);
                let cleanup_release = Arc::clone(&cleanup_release);
                move |_resource: Resource| async move {
                    cleanup_started.notify_one();
                    cleanup_release.notified().await;
                    Err(Error::ObjectGone {
                        kind: ObjectKind::Session,
                        id: String::from("$detached"),
                    })
                }
            },
            async |_resource: &Resource| Ok::<(), Error>(()),
        ));

        tokio::select! {
            outcome = &mut scope => {
                let _ = outcome;
                panic!("cleanup must remain pending")
            }
            () = cleanup_started.notified() => {}
        }
        drop(scope);
        cleanup_release.notify_one();

        tokio::time::timeout(TEST_TIMEOUT, failures.seen.notified())
            .await
            .expect("the detached cleanup failure is traced");
    }

    #[cfg(feature = "tracing")]
    #[tokio::test]
    async fn an_observed_cleanup_failure_is_not_traced() {
        let failures = CleanupFailures::default();
        let subscriber = tracing_subscriber::registry().with(failures.clone());
        let _guard = tracing::subscriber::set_default(subscriber);

        let outcome = super::run(
            "with-pane",
            async {
                Ok::<_, Error>(Resource {
                    cleaned: Arc::new(Notify::new()),
                })
            },
            |_resource: Resource| async {
                Err(Error::ObjectGone {
                    kind: ObjectKind::Session,
                    id: String::from("$observed"),
                })
            },
            async |_resource: &Resource| Ok::<(), Error>(()),
        )
        .await;

        assert!(outcome.is_err());
        assert_eq!(failures.count.load(Ordering::Relaxed), 0);
    }
}
