use std::future::Future;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::Error;

#[cfg(feature = "tracing")]
use tracing::instrument::WithSubscriber as _;

pub(crate) async fn run<R, T, E, Create, Cleanup, CleanupFuture, Operation>(
    create: Create,
    cleanup: Cleanup,
    operation: Operation,
) -> Result<T, E>
where
    R: Clone + Send + 'static,
    Create: Future<Output = Result<R, Error>> + Send + 'static,
    Cleanup: FnOnce(R) -> CleanupFuture + Send + 'static,
    CleanupFuture: Future<Output = Result<(), Error>> + Send + 'static,
    Operation: AsyncFnOnce(&R) -> Result<T, E>,
    E: From<Error>,
{
    let (created, cleanup) = acquire(create, cleanup).await.map_err(E::from)?;
    let outcome = operation(&created).await;

    match (outcome, cleanup.finish().await) {
        (outcome, Ok(())) => outcome,
        (Ok(_), Err(error)) => Err(error.into()),
        (Err(outcome), Err(cleanup)) => {
            trace_cleanup_failure(&cleanup);
            Err(outcome)
        }
    }
}

async fn acquire<R, Create, Cleanup, CleanupFuture>(
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
    let supervisor = async move {
        let outcome = create.await.map(|created| {
            let cleanup = ScopeCleanup::new(cleanup(created.clone()));
            (created, cleanup)
        });
        let _ = handoff.send(outcome);
    };
    #[cfg(feature = "tracing")]
    let supervisor = tokio::spawn(supervisor.with_current_subscriber());
    #[cfg(not(feature = "tracing"))]
    let supervisor = tokio::spawn(supervisor);

    match receive.await {
        Ok(outcome) => outcome,
        Err(_) => match supervisor.await {
            Err(error) => std::panic::resume_unwind(error.into_panic()),
            Ok(()) => unreachable!("the creation supervisor ended without a handoff"),
        },
    }
}

struct ScopeCleanup {
    release: oneshot::Sender<()>,
    outcome: oneshot::Receiver<Result<(), Error>>,
    #[cfg(feature = "tracing")]
    observed: oneshot::Sender<()>,
    supervisor: JoinHandle<()>,
}

impl ScopeCleanup {
    fn new(cleanup: impl Future<Output = Result<(), Error>> + Send + 'static) -> Self {
        let (release, released) = oneshot::channel();
        let (outcome_sender, outcome) = oneshot::channel();
        #[cfg(feature = "tracing")]
        let (observed, observation) = oneshot::channel();
        let supervisor = async move {
            let _ = released.await;
            let outcome = cleanup.await;

            #[cfg(feature = "tracing")]
            let failure = outcome.as_ref().err().map(ToString::to_string);
            #[cfg(feature = "tracing")]
            let delivered = outcome_sender.send(outcome).is_ok();
            #[cfg(not(feature = "tracing"))]
            let _ = outcome_sender.send(outcome);

            #[cfg(feature = "tracing")]
            if let Some(error) = failure {
                let acknowledged = delivered && observation.await.is_ok();
                if !acknowledged {
                    trace_cleanup_failure(&error);
                }
            }
        };
        #[cfg(feature = "tracing")]
        let supervisor = tokio::spawn(supervisor.with_current_subscriber());
        #[cfg(not(feature = "tracing"))]
        let supervisor = tokio::spawn(supervisor);

        Self {
            release,
            outcome,
            #[cfg(feature = "tracing")]
            observed,
            supervisor,
        }
    }

    async fn finish(self) -> Result<(), Error> {
        let _ = self.release.send(());
        match self.outcome.await {
            Ok(outcome) => {
                #[cfg(feature = "tracing")]
                let _ = self.observed.send(());
                outcome
            }
            Err(_) => match self.supervisor.await {
                Err(error) => std::panic::resume_unwind(error.into_panic()),
                Ok(()) => unreachable!("the cleanup supervisor ended without an outcome"),
            },
        }
    }
}

#[cfg(feature = "tracing")]
fn trace_cleanup_failure(error: &impl std::fmt::Display) {
    tracing::debug!(error = %error, "scoped operation cleanup failed");
}

#[cfg(not(feature = "tracing"))]
fn trace_cleanup_failure(_error: &impl std::fmt::Display) {}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use tokio::sync::Notify;

    use crate::Error;

    #[cfg(feature = "tracing")]
    use crate::ObjectKind;

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
