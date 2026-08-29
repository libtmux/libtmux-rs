//! Retained job progress and the owned command reader.

use std::ops::Range;
use std::sync::{Arc, Mutex};

use tokio::sync::{Notify, oneshot};

use crate::exec::{self, RunOutcome, RunView};
use crate::retained::RetainedBytes;
use crate::text::{TextFilter, readable_from};

use super::{DispatchReport, JobState, hold};

/// What one job's reader has collected.
#[derive(Debug)]
pub(super) struct Progress {
    pub(super) state: JobState,
    pub(super) exit_status: Option<i32>,
    /// The retained pane stream, including the command's echoed input.
    pub(super) stream: RetainedBytes,
    /// The command's own bytes within `stream`, once its shell answered.
    pub(super) body: Option<Range<usize>>,
    /// How many bytes were dropped off the front of the command's output.
    pub(super) dropped: u64,
    /// Filter state immediately before the retained command output.
    pub(super) checkpoint: TextFilter,
    /// How many pane-stream bytes arrived before trimming.
    pub(super) bytes: usize,
    /// Whether the pane stream was trimmed from the front.
    pub(super) truncated: bool,
    /// The exact terminal view returned by the collector.
    pub(super) terminal: Option<RunView>,
}

impl Progress {
    pub(super) fn new() -> Self {
        Self {
            state: JobState::Starting,
            exit_status: None,
            stream: RetainedBytes::new(),
            body: None,
            dropped: 0,
            checkpoint: TextFilter::new(),
            bytes: 0,
            truncated: false,
            terminal: None,
        }
    }

    /// Apply one scanner delta to the retained command output.
    pub(super) fn apply(&mut self, progress: exec::RunProgress<'_>) {
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
    pub(super) fn read_from(&self, cursor: u64) -> (&[u8], u64, bool) {
        let output = self.body();
        let end = self.dropped + output.len() as u64;
        if cursor < self.dropped {
            return (output, end, true);
        }
        let from = usize::try_from(cursor - self.dropped).unwrap_or(output.len());
        (output.get(from..).unwrap_or_default(), end, false)
    }

    pub(super) fn text_from(&self, cursor: u64) -> (String, u64, bool) {
        let (bytes, end, truncated) = self.read_from(cursor);
        let output = self.body();
        let from = output.len() - bytes.len();
        (
            readable_from(&self.checkpoint, output, from),
            end,
            truncated,
        )
    }

    pub(super) fn unfinished(&self, pane: String, outcome: RunOutcome) -> RunView {
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
    pub(super) fn foreground_view(
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

/// Dispatch and read one job after its table entry is visible.
pub(super) async fn drive(
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
const fn ended(view: &RunView) -> (JobState, Option<i32>) {
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
