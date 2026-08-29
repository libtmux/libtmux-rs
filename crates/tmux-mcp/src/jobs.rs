//! Commands that outlive the call that started them.
//!
//! `run_command` waits on the same owned reader a background job uses. A
//! deadline or withdrawn request stops that wait but leaves the job available
//! to inspect with `job_status`. `forget_job` only discards retained output;
//! use `send_keys` with `keys: ["C-c"]` to interrupt the whole pane, which
//! can discard unrelated queued input.
//!
//! A job is the same sentinel-bracketed run, reading in a task of its own. The
//! call that starts it returns an id, and the answer is collected whether or
//! not anyone is waiting. Polling is cheap because a poll reports only what is
//! new, on the same cursor contract `capture_since` uses.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use libtmux::{Error, Pane};
use serde::Serialize;
use tokio::sync::{Notify, oneshot};

use crate::exec::{self, RetainedBytes, RunOutcome, RunView};
use crate::identity::{InstanceId, InstanceIdentity};
use crate::text::{TextFilter, readable_from};

/// Take a lock, treating a poisoned one as held rather than as fatal.
fn hold<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How many jobs this server remembers, running and finished together.
///
/// A finished job is kept so its answer can still be collected; the least
/// recently touched is dropped to make room. Only an active job can hold a
/// control-mode connection.
const MAX_JOBS: usize = 32;

/// Why a background command could not be started.
#[derive(Debug)]
pub(crate) enum StartError {
    /// Every remembered slot belongs to a command still starting or running.
    AtCapacity { limit: usize },
    /// A server-instance identity could not be generated.
    IdentityUnavailable,
    /// This server instance issued every possible counter value.
    IdSpaceExhausted,
    /// The pane could not be watched, or the line dispatch never began.
    Tmux(Error),
    /// A send may have reached tmux, and the named job retains ownership.
    DispatchUnknown { job: String, cause: DispatchFailure },
    /// The published worker stopped before it reported a retained outcome.
    WorkerStopped,
}

/// Why a published job could not establish whether tmux accepted its input.
#[derive(Debug)]
pub(crate) enum DispatchFailure {
    /// A pane-input dispatch failed without delivery certainty.
    Tmux(Box<Error>),
    /// The owned worker ended before reporting its dispatch result.
    WorkerStopped,
}

/// What the owned worker learned from sending the command.
enum DispatchReport {
    Confirmed,
    NotDispatched(Error),
    Unknown(Error),
}

impl From<Error> for StartError {
    fn from(error: Error) -> Self {
        Self::Tmux(error)
    }
}

/// Where a job has got to.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// The watcher is attached and the command is being sent.
    Starting,
    /// The command is still running.
    Running,
    /// tmux did not confirm a send, so the command may be running.
    ///
    /// The watcher stays attached. Inspect it with `job_status` and inspect
    /// the pane; retrying automatically is unsafe. `forget_job` only discards
    /// retained output. To interrupt the whole pane, use `send_keys` with
    /// `keys: ["C-c"]`, which can discard unrelated queued input.
    DispatchUnknown,
    /// The line dispatch was rejected before tmux could receive pane input.
    ///
    /// Normally the failed start is removed before its caller returns. This
    /// remains observable only when that caller was cancelled first.
    NotStarted,
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

impl JobState {
    const fn is_active(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::DispatchUnknown)
    }
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
    /// Absent while active, and when a signal killed the command rather than
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
    /// The retained pane stream, including the command's echoed input.
    stream: RetainedBytes,
    /// The command's own bytes within `stream`, once its shell answered.
    body: Option<Range<usize>>,
    /// How many bytes were dropped off the front of the command's output.
    dropped: u64,
    /// Filter state immediately before the retained command output.
    checkpoint: TextFilter,
    /// How many pane-stream bytes arrived before trimming.
    bytes: usize,
    /// Whether the pane stream was trimmed from the front.
    truncated: bool,
    /// The exact terminal view returned by the collector.
    terminal: Option<RunView>,
}

impl Progress {
    /// Apply one scanner delta to the retained command output.
    fn apply(&mut self, progress: exec::RunProgress<'_>) {
        self.stream.discard(progress.discarded);
        self.stream.append(progress.appended);
        self.stream.settle();
        self.body = progress.body;
        self.dropped = progress.body_dropped;
        self.checkpoint = progress.body_checkpoint.clone();
        self.bytes = progress.bytes;
        self.truncated = progress.truncated;
    }

    fn body(&self) -> &[u8] {
        self.body
            .as_ref()
            .and_then(|range| self.stream.as_slice().get(range.clone()))
            .unwrap_or_default()
    }

    /// Read from `cursor`, saying whether anything before it was lost.
    ///
    /// The cursor counts bytes of the command's whole output, so it stays
    /// meaningful after trimming: what moves is where those bytes live, not
    /// what they are called.
    fn read_from(&self, cursor: u64) -> (&[u8], u64, bool) {
        let output = self.body();
        let end = self.dropped + output.len() as u64;
        if cursor < self.dropped {
            return (output, end, true);
        }
        let from = usize::try_from(cursor - self.dropped).unwrap_or(output.len());
        (output.get(from..).unwrap_or_default(), end, false)
    }

    fn text_from(&self, cursor: u64) -> (String, u64, bool) {
        let (bytes, end, truncated) = self.read_from(cursor);
        let output = self.body();
        let from = output.len() - bytes.len();
        (
            readable_from(&self.checkpoint, output, from),
            end,
            truncated,
        )
    }

    fn unfinished(&self, pane: String, outcome: RunOutcome) -> RunView {
        let outcome = if outcome == RunOutcome::Deadline && self.body.is_none() {
            RunOutcome::NoShell
        } else {
            outcome
        };
        let output = if self.body.is_some() {
            readable_from(&self.checkpoint, self.body(), 0)
        } else {
            exec::readable(self.stream.as_slice())
        };
        RunView {
            pane,
            outcome,
            exit_status: None,
            output,
            bytes: self.bytes,
            truncated: self.truncated,
            job: None,
        }
    }

    /// Resolve a foreground observer after its wait ends.
    fn foreground_view(
        &self,
        pane: String,
        stopped: Option<RunOutcome>,
    ) -> Option<(RunView, bool)> {
        if let Some(terminal) = &self.terminal {
            return Some((terminal.clone(), false));
        }
        stopped.map(|outcome| (self.unfinished(pane, outcome), true))
    }
}

/// One background command.
#[derive(Debug)]
struct Job {
    pane: String,
    command: String,
    started: Instant,
    progress: Arc<Mutex<Progress>>,
    /// Fires when the reader reaches a terminal state or loses its owner.
    finished: Arc<Notify>,
    reader: tokio::task::JoinHandle<()>,
    last_read: Instant,
}

impl Drop for Job {
    fn drop(&mut self) {
        // A terminal state is published before its notification. Let that
        // reader finish the handoff rather than stranding a waiter.
        if hold(&self.progress).state.is_active() {
            self.reader.abort();
            self.finished.notify_waiters();
        }
    }
}

/// One slot in the bounded job table.
#[derive(Debug)]
enum JobSlot {
    /// Reserved while the watcher attaches, before any pane input is sent.
    Pending,
    /// A command visible to callers and owned by the table.
    Ready(Job),
}

/// Running jobs, completed jobs, and starts that are between those states.
#[derive(Debug)]
struct JobTable {
    slots: HashMap<String, JobSlot>,
    limit: usize,
    next_id: Option<u64>,
}

impl JobTable {
    fn new(limit: usize) -> Self {
        Self {
            slots: HashMap::new(),
            limit,
            next_id: Some(0),
        }
    }

    /// Reserve a slot, evicting only a finished job when the table is full.
    fn reserve(&mut self, owner: InstanceId) -> Result<String, StartError> {
        let next_id = self.next_id.ok_or(StartError::IdSpaceExhausted)?;
        if self.slots.len() >= self.limit
            && let Some(stale) = self
                .slots
                .iter()
                .filter_map(|(id, slot)| match slot {
                    JobSlot::Ready(job) if !hold(&job.progress).state.is_active() => {
                        Some((id, job.last_read))
                    }
                    JobSlot::Pending | JobSlot::Ready(_) => None,
                })
                .min_by_key(|(_, last_read)| *last_read)
                .map(|(id, _)| id.clone())
        {
            self.slots.remove(&stale);
        }

        if self.slots.len() >= self.limit {
            return Err(StartError::AtCapacity { limit: self.limit });
        }

        let id = format!("job-{owner}-{next_id}");
        self.next_id = next_id.checked_add(1);
        self.slots.insert(id.clone(), JobSlot::Pending);
        Ok(id)
    }
}

/// A slot that is released unless a started job takes ownership of it.
struct Reservation<'a> {
    table: &'a Mutex<JobTable>,
    id: String,
    committed: bool,
}

impl Reservation<'_> {
    fn id(&self) -> &str {
        &self.id
    }

    fn commit(mut self, job: Job) {
        let previous = hold(self.table)
            .slots
            .insert(self.id.clone(), JobSlot::Ready(job));
        debug_assert!(matches!(previous, Some(JobSlot::Pending)));
        self.committed = true;
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            hold(self.table).slots.remove(&self.id);
        }
    }
}

/// The jobs this server is holding.
#[derive(Debug)]
pub(crate) struct Jobs {
    identity: Arc<InstanceIdentity>,
    inner: Mutex<JobTable>,
}

impl Jobs {
    /// Hold no jobs yet.
    #[must_use]
    pub(crate) fn new(identity: Arc<InstanceIdentity>) -> Self {
        Self {
            identity,
            inner: Mutex::new(JobTable::new(MAX_JOBS)),
        }
    }

    #[cfg(test)]
    fn with_limit(limit: usize) -> Self {
        Self {
            identity: Arc::new(InstanceIdentity::new()),
            inner: Mutex::new(JobTable::new(limit)),
        }
    }

    fn reserve(&self) -> Result<Reservation<'_>, StartError> {
        let owner = self
            .identity
            .get()
            .map_err(|_| StartError::IdentityUnavailable)?;
        let id = hold(&self.inner).reserve(owner)?;
        Ok(Reservation {
            table: &self.inner,
            id,
            committed: false,
        })
    }

    /// Start a command in a pane and return once it is under way.
    ///
    /// # Errors
    ///
    /// Returns an error when the server identity cannot be generated, every
    /// job slot is active, or the pane cannot be watched. A send that tmux
    /// does not confirm returns the retained job id.
    pub(crate) async fn start(
        &self,
        pane: &Pane,
        command: &str,
        suppress_history: bool,
    ) -> Result<JobView, StartError> {
        let reservation = self.reserve()?;
        let id = reservation.id().to_owned();
        let pane_id = pane.id().to_string();
        let command = command.to_owned();
        let progress = Arc::new(Mutex::new(Progress {
            state: JobState::Starting,
            exit_status: None,
            stream: RetainedBytes::new(),
            body: None,
            dropped: 0,
            checkpoint: TextFilter::new(),
            bytes: 0,
            truncated: false,
            terminal: None,
        }));
        let finished = Arc::new(Notify::new());

        // Setup is still request-owned because it has not touched the pane.
        // Publication gates the worker immediately before its line dispatch.
        let prepared = exec::prepare_run(pane, &command, suppress_history).await?;
        let (publish, published) = oneshot::channel();
        let (report, reported) = oneshot::channel();
        let worker_progress = Arc::clone(&progress);
        let worker_finished = Arc::clone(&finished);
        let reader = tokio::spawn(async move {
            if published.await.is_ok() {
                drive(prepared, worker_progress, worker_finished, report).await;
            }
        });

        let started = Instant::now();
        reservation.commit(Job {
            pane: pane_id.clone(),
            command: command.clone(),
            started,
            progress: Arc::clone(&progress),
            finished,
            reader,
            last_read: started,
        });

        if publish.send(()).is_err() {
            self.forget(&id);
            return Err(StartError::WorkerStopped);
        }

        match reported.await {
            Ok(DispatchReport::Confirmed) => Ok(JobView {
                job: id,
                pane: pane_id,
                command,
                state: JobState::Running,
                exit_status: None,
                age_seconds: 0,
            }),
            Ok(DispatchReport::NotDispatched(error)) => {
                self.forget(&id);
                Err(StartError::Tmux(error))
            }
            Ok(DispatchReport::Unknown(error)) => {
                if !self.holds(&id) {
                    return Err(StartError::WorkerStopped);
                }
                Err(StartError::DispatchUnknown {
                    job: id,
                    cause: DispatchFailure::Tmux(Box::new(error)),
                })
            }
            Err(_) => {
                if !self.holds(&id) {
                    return Err(StartError::WorkerStopped);
                }
                hold(&progress).state = JobState::DispatchUnknown;
                Err(StartError::DispatchUnknown {
                    job: id,
                    cause: DispatchFailure::WorkerStopped,
                })
            }
        }
    }

    /// Run a command while keeping ownership if this caller stops waiting.
    ///
    /// # Errors
    ///
    /// Returns the same startup errors as [`Self::start`].
    pub(crate) async fn run(
        &self,
        pane: &Pane,
        command: &str,
        timeout: std::time::Duration,
        suppress_history: bool,
        cancelled: &tokio_util::sync::CancellationToken,
    ) -> Result<RunView, StartError> {
        let started = self.start(pane, command, suppress_history).await?;
        let id = started.job;
        let pane = started.pane;
        let Some((finished, progress)) = self.awaitable(&id) else {
            return Err(StartError::WorkerStopped);
        };

        let notified = finished.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.holds(&id) {
            return Err(StartError::WorkerStopped);
        }

        let stopped = if hold(&progress).state.is_active() {
            tokio::select! {
                biased;
                () = cancelled.cancelled() => Some(RunOutcome::Cancelled),
                () = tokio::time::sleep(timeout) => Some(RunOutcome::Deadline),
                () = notified.as_mut() => None,
            }
        } else {
            None
        };

        let (mut view, retain) = hold(&progress)
            .foreground_view(pane, stopped)
            .ok_or(StartError::WorkerStopped)?;
        if retain {
            view.job = Some(id);
        } else {
            self.forget(&id);
        }
        Ok(view)
    }

    /// Report what a job has written since `cursor`.
    ///
    /// Returns `None` when no such job is held.
    pub(crate) fn read(&self, id: &str, cursor: Option<u64>) -> Option<JobProgress> {
        let mut table = hold(&self.inner);
        let JobSlot::Ready(job) = table.slots.get_mut(id)? else {
            return None;
        };
        job.last_read = Instant::now();
        let pane = job.pane.clone();
        let progress = hold(&job.progress);
        let (output, end, truncated) = progress.text_from(cursor.unwrap_or(0));

        Some(JobProgress {
            job: id.to_owned(),
            pane,
            state: progress.state,
            exit_status: progress.exit_status,
            output,
            cursor: end,
            truncated,
            complete: !progress.state.is_active(),
        })
    }

    /// Resolve a job to what it needs to be awaited without holding the lock.
    fn awaitable(&self, id: &str) -> Option<(Arc<Notify>, Arc<Mutex<Progress>>)> {
        let table = hold(&self.inner);
        let JobSlot::Ready(job) = table.slots.get(id)? else {
            return None;
        };
        Some((Arc::clone(&job.finished), Arc::clone(&job.progress)))
    }

    /// Wait for a job to finish, up to `timeout`.
    ///
    /// Returns as soon as the job is already over, so a caller that polls too
    /// late still gets an answer rather than waiting out the deadline.
    pub(crate) async fn wait(&self, id: &str, timeout: std::time::Duration) -> bool {
        self.wait_with(id, timeout, || {}).await
    }

    async fn wait_with(
        &self,
        id: &str,
        timeout: std::time::Duration,
        ready: impl FnOnce(),
    ) -> bool {
        let Some((finished, progress)) = self.awaitable(id) else {
            return false;
        };

        let notified = finished.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.holds(id) {
            return false;
        }

        if !hold(&progress).state.is_active() {
            return true;
        }

        ready();
        tokio::time::timeout(timeout, notified.as_mut())
            .await
            .is_ok()
    }

    /// Describe every job this server holds, newest first.
    pub(crate) fn list(&self) -> Vec<JobView> {
        let table = hold(&self.inner);
        let mut views: Vec<_> = table
            .slots
            .iter()
            .filter_map(|(id, slot)| {
                let JobSlot::Ready(job) = slot else {
                    return None;
                };
                let progress = hold(&job.progress);
                Some(JobView {
                    job: id.clone(),
                    pane: job.pane.clone(),
                    command: job.command.clone(),
                    state: progress.state,
                    exit_status: progress.exit_status,
                    age_seconds: job.started.elapsed().as_secs(),
                })
            })
            .collect();
        views.sort_by_key(|view| view.age_seconds);
        views
    }

    /// Report whether this table still owns a published job.
    fn holds(&self, id: &str) -> bool {
        matches!(hold(&self.inner).slots.get(id), Some(JobSlot::Ready(_)))
    }

    /// Forget a published job and return its pane.
    pub(crate) fn forget(&self, id: &str) -> Option<String> {
        let job = {
            let mut table = hold(&self.inner);
            let JobSlot::Ready(_) = table.slots.get(id)? else {
                return None;
            };
            let Some(JobSlot::Ready(job)) = table.slots.remove(id) else {
                return None;
            };
            job
        };
        Some(job.pane.clone())
    }
}

/// Dispatch and read one job after its table entry is visible.
async fn drive(
    prepared: exec::PreparedRun,
    progress: Arc<Mutex<Progress>>,
    finished: Arc<Notify>,
    report: oneshot::Sender<DispatchReport>,
) {
    let run = match prepared.dispatch().await {
        exec::RunDispatch::Confirmed(run) => {
            hold(&progress).state = JobState::Running;
            let _ = report.send(DispatchReport::Confirmed);
            run
        }
        exec::RunDispatch::NotDispatched(error) => {
            hold(&progress).state = JobState::NotStarted;
            finished.notify_waiters();
            let _ = report.send(DispatchReport::NotDispatched(error));
            return;
        }
        exec::RunDispatch::Unknown { run, error } => {
            hold(&progress).state = JobState::DispatchUnknown;
            let _ = report.send(DispatchReport::Unknown(error));
            run
        }
    };

    let view = run.collect(|update| hold(&progress).apply(update)).await;

    {
        let mut held = hold(&progress);
        let (state, exit_status) = ended(&view);
        held.state = state;
        held.exit_status = exit_status;
        held.terminal = Some(view);
    }
    finished.notify_waiters();
}

/// Translate a finished run into the two fields a job records.
pub(crate) const fn ended(view: &RunView) -> (JobState, Option<i32>) {
    match view.outcome {
        RunOutcome::Completed => (JobState::Finished, view.exit_status),
        RunOutcome::PaneClosed => (JobState::PaneClosed, None),
        // A job reads until the command ends, so it has no deadline of its
        // own to reach and is never cancelled by a withdrawn request.
        RunOutcome::Deadline | RunOutcome::Cancelled | RunOutcome::NoShell => {
            (JobState::NoShell, None)
        }
    }
}

#[cfg(test)]
mod tests;
