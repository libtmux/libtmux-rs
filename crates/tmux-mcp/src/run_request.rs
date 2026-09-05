//! One request-owned pane command and its bounded output collector.

use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use libtmux::Pane;
use tokio_util::sync::CancellationToken;

use crate::exec::{self, RunOutcome, RunView};
use crate::retained::RetainedBytes;
use crate::text::{TextFilter, readable_from};

/// Why a request-owned pane command could not establish a result.
#[derive(Debug)]
pub(crate) enum RunError {
    /// The watcher or confirmed dispatch failed.
    Tmux(libtmux::Error),
    /// Pane input may have reached tmux, but delivery was not acknowledged.
    DispatchUnknown(Box<libtmux::Error>),
}

impl From<libtmux::Error> for RunError {
    fn from(error: libtmux::Error) -> Self {
        Self::Tmux(error)
    }
}

#[derive(Debug)]
struct Progress {
    stream: RetainedBytes,
    body: Option<Range<usize>>,
    dropped: u64,
    checkpoint: TextFilter,
    bytes: usize,
    truncated: bool,
}

impl Progress {
    fn new() -> Self {
        Self {
            stream: RetainedBytes::new(),
            body: None,
            dropped: 0,
            checkpoint: TextFilter::new(),
            bytes: 0,
            truncated: false,
        }
    }

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

    fn interrupted(&self, pane: String, outcome: RunOutcome) -> RunView {
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
        }
    }
}

fn hold<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Run one pane command while this request owns the watcher and collector.
pub(crate) async fn run(
    pane: &Pane,
    command: &str,
    timeout: Duration,
    suppress_history: bool,
    cancelled: &CancellationToken,
) -> Result<RunView, RunError> {
    let prepared = exec::prepare_run(pane, command, suppress_history).await?;
    let run = match prepared.dispatch().await {
        exec::RunDispatch::Confirmed(run) => run,
        exec::RunDispatch::NotDispatched(error) => return Err(RunError::Tmux(error)),
        exec::RunDispatch::Unknown { run, error } => {
            drop(run);
            return Err(RunError::DispatchUnknown(Box::new(error)));
        }
    };

    let pane_id = pane.id().to_string();
    let progress = Arc::new(Mutex::new(Progress::new()));
    let update = Arc::clone(&progress);
    let result = {
        let collected = run.collect(move |delta| hold(&update).apply(delta));
        tokio::pin!(collected);
        tokio::select! {
            biased;
            view = &mut collected => Ok(view),
            () = cancelled.cancelled() => Err(RunOutcome::Cancelled),
            () = tokio::time::sleep(timeout) => Err(RunOutcome::Deadline),
        }
    };

    match result {
        Ok(view) => Ok(view),
        Err(outcome) => Ok(hold(&progress).interrupted(pane_id, outcome)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deadline_without_a_shell_acknowledgement_is_no_shell() {
        let progress = Progress::new();
        let view = progress.interrupted("%1".to_owned(), RunOutcome::Deadline);

        assert_eq!(view.outcome, RunOutcome::NoShell);
        assert_eq!(view.pane, "%1");
    }
}
