//! Byte and time budgets shared by subprocess drivers.
use crate::Error;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub deadline: Instant,
    pub bytes_per_stream: usize,
}

impl Budget {
    #[must_use]
    pub fn starting_at(now: Instant) -> Self {
        Self {
            deadline: now + Duration::from_secs(5),
            bytes_per_stream: 1024 * 1024,
        }
    }
}

/// Maximum outstanding subprocesses shared by clones of one server.
pub const MAX_IN_FLIGHT: usize = 16;

/// Time allowed for a signalled child to exit after a failed request.
pub const CLEANUP_ALLOWANCE: Duration = Duration::from_secs(1);

impl Budget {
    #[must_use]
    pub fn is_late(&self, observed: Instant) -> bool {
        observed >= self.deadline
    }

    pub fn append(
        &self,
        stream: &'static str,
        bytes: &mut Vec<u8>,
        chunk: &[u8],
    ) -> Result<(), Error> {
        if chunk.len() > self.bytes_per_stream.saturating_sub(bytes.len()) {
            return Err(Error::OutputLimit {
                stream,
                limit: self.bytes_per_stream,
            });
        }
        bytes.extend_from_slice(chunk);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("kill error {kill:?}, reap error {reap:?}")]
pub struct CleanupErrors {
    pub kill: Option<std::io::Error>,
    pub reap: Option<std::io::Error>,
}

impl CleanupErrors {
    pub fn after(kill: std::io::Result<()>, reap: std::io::Result<()>) -> Result<(), Self> {
        match (kill, reap) {
            (Ok(()), Ok(())) => Ok(()),
            (kill, reap) => Err(Self {
                kill: kill.err(),
                reap: reap.err(),
            }),
        }
    }
}

#[derive(Debug)]
pub enum Event<T> {
    Done(Result<T, Error>),
    Failed(Error),
    Cleaned(Result<(), CleanupErrors>),
}

/// A caller may lose its receiver while a supervisor is killing or reaping.
pub fn after_cleanup<T>(primary: Error, event: Option<Event<T>>) -> Error {
    match event {
        Some(Event::Cleaned(Ok(()))) => primary,
        Some(Event::Cleaned(Err(errors))) => Error::Cleanup {
            primary: Box::new(primary),
            errors,
        },
        _ => Error::CleanupPending(Box::new(primary)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_reply_never_implies_rejection_before_submission() {
        let start = Instant::now();
        let budget = Budget::starting_at(start);
        assert!(!budget.is_late(start));
        assert!(budget.is_late(budget.deadline));
        let pending = after_cleanup::<()>(Error::Deadline, None);
        assert!(
            matches!(pending, Error::CleanupPending(cause) if matches!(*cause, Error::Deadline))
        );
    }

    #[test]
    fn cleanup_preserves_the_primary_and_each_failed_system_call() {
        let cleanup = CleanupErrors::after(
            Err(std::io::Error::other("signal")),
            Err(std::io::Error::other("wait")),
        );
        let err = after_cleanup::<()>(
            Error::OutputLimit {
                stream: "stderr",
                limit: 4,
            },
            Some(Event::Cleaned(cleanup)),
        );
        let Error::Cleanup { primary, errors } = err else {
            panic!("{err}");
        };
        assert!(matches!(
            *primary,
            Error::OutputLimit {
                stream: "stderr",
                limit: 4
            }
        ));
        assert_eq!(errors.kill.unwrap().to_string(), "signal");
        assert_eq!(errors.reap.unwrap().to_string(), "wait");
        assert!(matches!(
            after_cleanup::<()>(Error::Deadline, None),
            Error::CleanupPending(_)
        ));
    }
}
