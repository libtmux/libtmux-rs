//! One request-owned pane command and its bounded output collector.

use std::ffi::OsStr;
use std::future::Future;
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use libtmux::Pane;
use rmcp::model::ErrorData;
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
    /// Pane state changed after watcher setup and before dispatch.
    Guard(ErrorData),
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
    transport: (&OsStr, &Path),
    final_check: impl Future<Output = Result<(), ErrorData>>,
) -> Result<RunView, RunError> {
    let prepared =
        exec::prepare_run(pane, command, suppress_history, transport.0, transport.1).await?;
    if let Err(error) = final_check.await {
        let _ = prepared.shutdown().await;
        return Err(RunError::Guard(error));
    }
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

    use rmcp::model::ErrorData;

    static CLEANUP_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn a_deadline_without_a_shell_acknowledgement_is_no_shell() {
        let progress = Progress::new();
        let view = progress.interrupted("%1".to_owned(), RunOutcome::Deadline);

        assert_eq!(view.outcome, RunOutcome::NoShell);
        assert_eq!(view.pane, "%1");
    }

    #[tokio::test]
    #[allow(
        clippy::await_holding_invalid_type,
        reason = "the guard serializes the test-only global counter"
    )]
    async fn final_guard_awaits_prepared_shutdown_before_return() {
        let _serial = CLEANUP_TEST.lock().await;
        let guard = libtmux::test::TestServer::builder()
            .start()
            .await
            .expect("tmux starts");
        let session = guard
            .server()
            .new_session("prepared-cleanup")
            .await
            .expect("session starts");
        let pane = session.panes().await.expect("panes list").remove(0);
        let executable = guard
            .server()
            .resolved_tmux_executable()
            .expect("fixture tmux resolves");
        let before = exec::prepared_shutdown_completions();
        let refusal = ErrorData::invalid_params("refused".to_owned(), None);

        let result = run(
            &pane,
            "printf should-not-run",
            Duration::from_secs(2),
            false,
            &CancellationToken::new(),
            (executable.as_os_str(), guard.server().socket_path()),
            async { Err(refusal) },
        )
        .await;

        assert!(matches!(result, Err(RunError::Guard(_))));
        assert_eq!(exec::prepared_shutdown_completions(), before + 1);
        let screen = pane.capture().await.expect("pane capture");
        assert!(
            !screen.iter().any(|line| line
                .as_bytes()
                .windows(b"should-not-run".len())
                .any(|part| part == b"should-not-run")),
            "the refused prepared payload never reaches the pane"
        );
        guard.shutdown().await.expect("tmux fixture shuts down");
    }
}
