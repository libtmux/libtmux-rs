//! Commands that outlive the call that started them.
//!
//! `run_command` blocks the caller's turn until the command finishes or the
//! deadline runs out, which is the right shape for something that takes a
//! second and the wrong one for a build. An agent that has to sit on a request
//! for ten minutes cannot do anything else in the meantime, and a client that
//! gives up first leaves the pane busy with no way to ask about it again.
//!
//! A job is the same sentinel-bracketed run, reading in a task of its own. The
//! call that starts it returns an id, and the answer is collected whether or
//! not anyone is waiting. Polling is cheap because a poll reports only what is
//! new, on the same cursor contract `capture_since` uses.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use libtmux::{Error, Pane};
use serde::Serialize;
use tokio::sync::Notify;

use crate::exec::{self, Outcome, RunView};

/// Take a lock, treating a poisoned one as held rather than as fatal.
fn hold<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How many jobs this server remembers, running and finished together.
///
/// A finished job is kept so its answer can still be collected; the least
/// recently touched is dropped to make room. Only running jobs hold a
/// control-mode connection.
const MAX_JOBS: usize = 32;

/// Numbers job ids, so one is never reused within a process.
static JOB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Where a job has got to.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// The command is still running.
    Running,
    /// The command finished and reported a status.
    Finished,
    /// The pane closed before the command finished.
    PaneClosed,
    /// The pane never acknowledged the command.
    ///
    /// The same evidence `run_command` reports as `no_shell`: the opening
    /// sentinel never came back, so the text went into whatever the pane was
    /// running rather than to a prompt.
    NoShell,
}

/// One job, as the protocol sees it.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct JobView {
    /// The id to poll with.
    pub job: String,
    /// The pane the command runs in.
    pub pane: String,
    /// The command as it was given.
    pub command: String,
    /// Where the job has got to.
    pub state: JobState,
    /// The command's exit status, once it has finished.
    ///
    /// Absent while running, and when a signal killed the command rather than
    /// it exiting.
    pub exit_status: Option<i32>,
    /// How many seconds ago the job was started.
    pub age_seconds: u64,
}

/// What a job has written since a cursor.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct JobProgress {
    /// The job that was read.
    pub job: String,
    /// The pane the command runs in.
    pub pane: String,
    /// Where the job has got to.
    pub state: JobState,
    /// The command's exit status, once it has finished.
    pub exit_status: Option<i32>,
    /// What the command wrote since the cursor, escape sequences removed.
    pub output: String,
    /// The cursor to pass back next time.
    ///
    /// Counts bytes of this job's output, so passing back the last one returns
    /// only what is new. Omit it to read from the beginning.
    pub cursor: u64,
    /// Whether output before this text was dropped to bound memory.
    pub truncated: bool,
    /// Whether the job is over, so polling again will report nothing new.
    pub complete: bool,
}

/// What one job's reader has collected.
#[derive(Debug)]
struct Progress {
    state: JobState,
    exit_status: Option<i32>,
    /// The command's output so far, as bytes the reader has kept.
    output: Vec<u8>,
    /// How many bytes were dropped off the front of `output`.
    dropped: u64,
}

impl Progress {
    /// Take the command's output as the reader now knows it.
    ///
    /// The scanner owns the bytes and may trim its own front, so this copies
    /// rather than sharing. `dropped` is how many of the command's bytes went
    /// with that trimming, which is what keeps a cursor meaningful.
    fn replace(&mut self, body: &[u8], dropped: u64) {
        self.dropped = dropped;
        self.output.clear();
        self.output.extend_from_slice(body);
    }

    /// Read from `cursor`, saying whether anything before it was lost.
    ///
    /// The cursor counts bytes of the command's whole output, so it stays
    /// meaningful after trimming: what moves is where those bytes live, not
    /// what they are called.
    fn read_from(&self, cursor: u64) -> (&[u8], u64, bool) {
        let end = self.dropped + self.output.len() as u64;
        if cursor < self.dropped {
            return (&self.output, end, true);
        }
        let from = usize::try_from(cursor - self.dropped).unwrap_or(self.output.len());
        (self.output.get(from..).unwrap_or_default(), end, false)
    }
}

/// One background command.
#[derive(Debug)]
struct Job {
    pane: String,
    command: String,
    started: Instant,
    progress: Arc<Mutex<Progress>>,
    /// Fires when the reader reaches a terminal state.
    finished: Arc<Notify>,
    reader: tokio::task::JoinHandle<()>,
    last_read: Instant,
}

impl Drop for Job {
    fn drop(&mut self) {
        // Dropping the reader drops its `PaneOutput`, which is how libtmux is
        // told the control-mode connection is no longer wanted.
        self.reader.abort();
    }
}

/// The jobs this server is holding.
#[derive(Debug, Default)]
pub(crate) struct Jobs {
    inner: Mutex<HashMap<String, Job>>,
}

impl Jobs {
    /// Hold no jobs yet.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Start a command in a pane and return once it is under way.
    ///
    /// # Errors
    ///
    /// Returns an error when the pane cannot be watched or the keys cannot be
    /// sent.
    pub(crate) async fn start(
        &self,
        pane: &Pane,
        command: &str,
        suppress_history: bool,
    ) -> Result<JobView, Error> {
        let id = format!("job-{}", JOB_COUNTER.fetch_add(1, Ordering::Relaxed));
        let progress = Arc::new(Mutex::new(Progress {
            state: JobState::Running,
            exit_status: None,
            output: Vec::new(),
            dropped: 0,
        }));
        let finished = Arc::new(Notify::new());

        // Attaching happens here rather than in the reader so a pane that
        // cannot be watched fails this call, where the caller can see it.
        let run = exec::start_run(pane, command, suppress_history).await?;

        let reader = {
            let progress = Arc::clone(&progress);
            let finished = Arc::clone(&finished);
            tokio::spawn(async move {
                let view = run
                    .collect(|body, dropped| {
                        let mut held = hold(&progress);
                        held.replace(body, dropped);
                    })
                    .await;

                {
                    let mut held = hold(&progress);
                    let (state, exit_status) = ended(&view);
                    held.state = state;
                    held.exit_status = exit_status;
                }
                finished.notify_waiters();
            })
        };

        let started = Instant::now();
        let view = JobView {
            job: id.clone(),
            pane: pane.id().to_string(),
            command: command.to_owned(),
            state: JobState::Running,
            exit_status: None,
            age_seconds: 0,
        };

        let mut jobs = hold(&self.inner);
        if jobs.len() >= MAX_JOBS
            && let Some(stale) = jobs
                .iter()
                .filter(|(_, job)| !matches!(hold(&job.progress).state, JobState::Running))
                .min_by_key(|(_, job)| job.last_read)
                .or_else(|| jobs.iter().min_by_key(|(_, job)| job.last_read))
                .map(|(id, _)| id.clone())
        {
            jobs.remove(&stale);
        }
        jobs.insert(
            id,
            Job {
                pane: pane.id().to_string(),
                command: command.to_owned(),
                started,
                progress,
                finished,
                reader,
                last_read: started,
            },
        );

        Ok(view)
    }

    /// Report what a job has written since `cursor`.
    ///
    /// Returns `None` when no such job is held.
    pub(crate) fn read(&self, id: &str, cursor: Option<u64>) -> Option<JobProgress> {
        let mut jobs = hold(&self.inner);
        let job = jobs.get_mut(id)?;
        job.last_read = Instant::now();
        let pane = job.pane.clone();
        let progress = hold(&job.progress);
        let (bytes, end, truncated) = progress.read_from(cursor.unwrap_or(0));

        Some(JobProgress {
            job: id.to_owned(),
            pane,
            state: progress.state,
            exit_status: progress.exit_status,
            output: exec::readable(bytes),
            cursor: end,
            truncated,
            complete: progress.state != JobState::Running,
        })
    }

    /// Resolve a job to what it needs to be awaited without holding the lock.
    fn awaitable(&self, id: &str) -> Option<(Arc<Notify>, Arc<Mutex<Progress>>)> {
        let jobs = hold(&self.inner);
        let job = jobs.get(id)?;
        Some((Arc::clone(&job.finished), Arc::clone(&job.progress)))
    }

    /// Wait for a job to finish, up to `timeout`.
    ///
    /// Returns as soon as the job is already over, so a caller that polls too
    /// late still gets an answer rather than waiting out the deadline.
    pub(crate) async fn wait(&self, id: &str, timeout: std::time::Duration) -> bool {
        let Some((finished, progress)) = self.awaitable(id) else {
            return false;
        };

        // Registered before the state is checked. Subscribing afterwards would
        // miss a job that finished in between and then wait for a
        // notification that has already been sent.
        let notified = finished.notified();
        if hold(&progress).state != JobState::Running {
            return true;
        }

        tokio::time::timeout(timeout, notified).await.is_ok()
    }

    /// Describe every job this server holds, newest first.
    pub(crate) fn list(&self) -> Vec<JobView> {
        let jobs = hold(&self.inner);
        let mut views: Vec<_> = jobs
            .iter()
            .map(|(id, job)| {
                let progress = hold(&job.progress);
                JobView {
                    job: id.clone(),
                    pane: job.pane.clone(),
                    command: job.command.clone(),
                    state: progress.state,
                    exit_status: progress.exit_status,
                    age_seconds: job.started.elapsed().as_secs(),
                }
            })
            .collect();
        views.sort_by_key(|view| view.age_seconds);
        views
    }

    /// Report whether a job is still running, and which pane it is in.
    pub(crate) fn running_in(&self, id: &str) -> Option<(String, bool)> {
        let jobs = hold(&self.inner);
        let job = jobs.get(id)?;
        let running = hold(&job.progress).state == JobState::Running;
        Some((job.pane.clone(), running))
    }

    /// Forget a job, ending its reader.
    pub(crate) fn forget(&self, id: &str) -> bool {
        hold(&self.inner).remove(id).is_some()
    }
}

/// Translate a finished run into the two fields a job records.
pub(crate) const fn ended(view: &RunView) -> (JobState, Option<i32>) {
    match view.outcome {
        Outcome::Completed => (JobState::Finished, view.exit_status),
        Outcome::PaneClosed => (JobState::PaneClosed, None),
        _ => (JobState::NoShell, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(output: &[u8], dropped: u64) -> Progress {
        Progress {
            state: JobState::Running,
            exit_status: None,
            output: output.to_vec(),
            dropped,
        }
    }

    #[test]
    fn a_cursor_reads_only_what_is_new() {
        let held = progress(b"hello world", 0);

        assert_eq!(held.read_from(0), (&b"hello world"[..], 11, false));
        assert_eq!(held.read_from(6), (&b"world"[..], 11, false));
        assert_eq!(held.read_from(11), (&b""[..], 11, false));
    }

    #[test]
    fn a_cursor_behind_what_was_trimmed_says_so() {
        // The first 100 bytes were dropped, so `output` starts at offset 100.
        let held = progress(b"tail", 100);

        let (bytes, end, truncated) = held.read_from(0);
        assert!(truncated, "the bytes at offset 0 are gone");
        assert_eq!(bytes, b"tail");
        assert_eq!(end, 104);

        let (bytes, _, truncated) = held.read_from(100);
        assert!(!truncated);
        assert_eq!(bytes, b"tail");
    }

    #[test]
    fn a_cursor_past_the_end_yields_nothing_rather_than_panicking() {
        let held = progress(b"hello", 0);

        assert_eq!(held.read_from(99), (&b""[..], 5, false));
    }
}
