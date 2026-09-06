//! One request-owned pane command and its bounded output collector.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::future::Future;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use libtmux::Pane;
use rmcp::model::ErrorData;
use tokio_util::sync::CancellationToken;

use crate::exec::{self, RunOutcome, RunView};
use crate::retained::RetainedBytes;
use crate::text::{TextFilter, readable_from};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RunKey {
    endpoint: PathBuf,
    pane: String,
}

#[derive(Default)]
struct ActiveRuns {
    next: u64,
    entries: HashMap<RunKey, u64>,
}

fn active_runs() -> &'static Mutex<ActiveRuns> {
    static ACTIVE: OnceLock<Mutex<ActiveRuns>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(ActiveRuns::default()))
}

#[derive(Debug)]
struct LeaseInner {
    key: RunKey,
    token: u64,
}

impl Drop for LeaseInner {
    fn drop(&mut self) {
        let mut active = hold(active_runs());
        if active.entries.get(&self.key) == Some(&self.token) {
            active.entries.remove(&self.key);
        }
    }
}

/// One process-wide reservation for a tmux endpoint and pane.
#[derive(Clone, Debug)]
pub(crate) struct RunLease(Arc<LeaseInner>);

impl RunLease {
    fn permits(&self, key: &RunKey, token: u64) -> bool {
        self.0.key == *key && self.0.token == token
    }
}

/// Reserve a pane until its command watcher proves completion or pane closure.
pub(crate) fn reserve(endpoint: &Path, pane: &str) -> Option<RunLease> {
    let key = RunKey {
        endpoint: endpoint.to_path_buf(),
        pane: pane.to_owned(),
    };
    let mut active = hold(active_runs());
    if active.entries.contains_key(&key) {
        return None;
    }
    active.next = active.next.wrapping_add(1).max(1);
    let token = active.next;
    active.entries.insert(key.clone(), token);
    Some(RunLease(Arc::new(LeaseInner { key, token })))
}

/// Whether input would cross an active run other than the permitted one.
pub(crate) fn is_reserved(endpoint: &Path, pane: &str, permitted: Option<&RunLease>) -> bool {
    let key = RunKey {
        endpoint: endpoint.to_path_buf(),
        pane: pane.to_owned(),
    };
    let active = hold(active_runs());
    active
        .entries
        .get(&key)
        .is_some_and(|token| !permitted.is_some_and(|lease| lease.permits(&key, *token)))
}

/// Prove that this reservation still owns the same endpoint and pane.
pub(crate) fn owns(lease: &RunLease, endpoint: &Path, pane: &str) -> bool {
    let key = RunKey {
        endpoint: endpoint.to_path_buf(),
        pane: pane.to_owned(),
    };
    let active = hold(active_runs());
    active
        .entries
        .get(&key)
        .is_some_and(|token| lease.permits(&key, *token))
}

fn retain_lease_until(proof: impl Future<Output = ()> + Send + 'static, lease: RunLease) {
    tokio::spawn(async move {
        proof.await;
        drop(lease);
    });
}

/// Why a request-owned pane command could not establish a result.
#[derive(Debug)]
pub(crate) enum RunError {
    /// The watcher or confirmed dispatch failed.
    Tmux(libtmux::Error),
    /// Pane input may have reached tmux, but delivery was not acknowledged.
    DispatchUnknown(Box<libtmux::Error>),
    /// Pane state changed after watcher setup and before dispatch.
    Guard(ErrorData),
    /// Completion framing failed before the pane watcher was attached.
    Frame,
}

impl From<libtmux::Error> for RunError {
    fn from(error: libtmux::Error) -> Self {
        Self::Tmux(error)
    }
}

impl From<exec::PrepareRunError> for RunError {
    fn from(error: exec::PrepareRunError) -> Self {
        match error {
            exec::PrepareRunError::Tmux(error) => Self::Tmux(error),
            exec::PrepareRunError::Frame => Self::Frame,
        }
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
    transport: (&OsStr, &Path, RunLease),
    final_check: impl Future<Output = Result<(), ErrorData>>,
) -> Result<RunView, RunError> {
    let (executable, endpoint, lease) = transport;
    let prepared = exec::prepare_run(pane, command, suppress_history, executable, endpoint).await?;
    if let Err(error) = final_check.await {
        let _ = prepared.shutdown().await;
        return Err(RunError::Guard(error));
    }
    let run = match prepared.dispatch().await {
        exec::RunDispatch::Confirmed(run) => run,
        exec::RunDispatch::NotDispatched(error) => return Err(RunError::Tmux(error)),
        exec::RunDispatch::Unknown { run, error } => {
            retain_lease_until(
                async move {
                    run.collect(|_| {}).await.finish_proof().await;
                },
                lease,
            );
            return Err(RunError::DispatchUnknown(Box::new(error)));
        }
    };

    let pane_id = pane.id().to_string();
    let progress = Arc::new(Mutex::new(Progress::new()));
    let update = Arc::clone(&progress);
    let result = {
        let mut collected = Box::pin(run.collect(move |delta| hold(&update).apply(delta)));
        tokio::select! {
            biased;
            view = &mut collected => Ok(view),
            () = cancelled.cancelled() => Err(RunOutcome::Cancelled),
            () = tokio::time::sleep(timeout) => Err(RunOutcome::Deadline),
        }
        .map_err(|outcome| (outcome, collected))
    };

    match result {
        Ok(collected) => {
            let (view, proof) = collected.into_parts();
            if let Some(proof) = proof {
                retain_lease_until(proof.wait(), lease);
            } else {
                drop(lease);
            }
            Ok(view)
        }
        Err((outcome, collected)) => {
            let view = hold(&progress).interrupted(pane_id, outcome);
            retain_lease_until(async move { collected.await.finish_proof().await }, lease);
            Ok(view)
        }
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
        let lease = reserve(guard.server().socket_path(), pane.id().as_ref())
            .expect("the fixture pane is unreserved");

        let result = run(
            &pane,
            "printf should-not-run",
            Duration::from_secs(2),
            false,
            &CancellationToken::new(),
            (executable.as_os_str(), guard.server().socket_path(), lease),
            async { Err(refusal) },
        )
        .await;

        assert!(matches!(result, Err(RunError::Guard(_))));
        assert_eq!(exec::prepared_shutdown_completions(), before + 1);
        assert!(
            reserve(guard.server().socket_path(), pane.id().as_ref()).is_some(),
            "final refusal releases its reservation after watcher shutdown"
        );
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
