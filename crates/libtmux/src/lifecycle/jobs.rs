use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::Notify;

use crate::Error;

#[derive(Clone, Default)]
pub(crate) struct Journal(Arc<Mutex<Vec<Arc<Job>>>>);

pub(crate) struct Job {
    result: Mutex<Option<Result<(), Error>>>,
    done: AtomicBool,
    changed: Notify,
}

impl Journal {
    pub(crate) fn begin(&self) -> Arc<Job> {
        let job = Arc::new(Job {
            result: Mutex::new(None),
            done: AtomicBool::new(false),
            changed: Notify::new(),
        });
        let mut jobs = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        jobs.retain(|job| !job.consumed());
        jobs.push(Arc::clone(&job));
        job
    }

    pub(crate) async fn drain(&self) -> Vec<Error> {
        loop {
            let pending = {
                let mut jobs = self
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                jobs.retain(|job| !job.consumed());
                jobs.clone()
            };
            for job in pending {
                job.wait().await;
            }
            let mut jobs = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Acquisitions register their handoff cleanup before completing.
            // Wait for those jobs too, without consuming errors before an await.
            if jobs.iter().any(|job| !job.done.load(Ordering::Acquire)) {
                continue;
            }
            let failures = jobs
                .iter()
                .filter_map(|job| job.take().and_then(Result::err))
                .collect();
            jobs.clear();
            return failures;
        }
    }
}

impl Job {
    pub(crate) fn complete(&self, result: Result<(), Error>) {
        *self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
        self.done.store(true, Ordering::Release);
        self.changed.notify_waiters();
    }

    pub(crate) async fn wait(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.done.load(Ordering::Acquire) {
                return;
            }
            changed.await;
        }
    }

    pub(crate) fn take(&self) -> Option<Result<(), Error>> {
        self.result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    pub(crate) fn discard_success(&self) {
        let mut result = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if result.as_ref().is_some_and(Result::is_ok) {
            result.take();
        }
    }

    fn consumed(&self) -> bool {
        self.done.load(Ordering::Acquire)
            && self
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none()
    }

    #[cfg(feature = "tracing")]
    pub(crate) fn trace_failure(&self) {
        if let Some(Err(error)) = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            tracing::debug!(error = %error, "scoped operation cleanup failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Journal;
    use crate::Error;

    #[tokio::test]
    async fn cancelling_a_drain_does_not_consume_earlier_failures() {
        let journal = Journal::default();
        let failed = journal.begin();
        failed.complete(Err(Error::LifecycleInput {
            reason: "retained failure",
        }));
        let pending = journal.begin();
        let draining = journal.clone();
        let task = tokio::spawn(async move { draining.drain().await });
        tokio::task::yield_now().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        pending.complete(Ok(()));
        assert!(matches!(
            journal.drain().await.as_slice(),
            [Error::LifecycleInput {
                reason: "retained failure"
            }]
        ));
        assert!(journal.drain().await.is_empty());
    }
}
