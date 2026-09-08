//! One request-owned pane command and its bounded output collector.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::future::Future;
use std::ops::Range;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use libtmux::{Pane, ServerGeneration};
use rmcp::model::ErrorData;
use tokio_util::sync::CancellationToken;

use crate::exec::{self, RunOutcome, RunView};
use crate::retained::RetainedBytes;
use crate::text::{TextFilter, readable_from};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RunKey {
    generation: ServerGeneration,
    endpoint: EndpointIdentity,
    pane: String,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct EndpointIdentity {
    device: u64,
    inode: u64,
}

fn endpoint_identity(path: &Path) -> std::io::Result<EndpointIdentity> {
    let metadata = std::fs::metadata(path)?;
    Ok(EndpointIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

pub(crate) fn same_endpoint(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (endpoint_identity(left), endpoint_identity(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => match (left.canonicalize(), right.canonicalize()) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        },
    }
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
    keys: Vec<RunKey>,
    token: u64,
}

impl Drop for LeaseInner {
    fn drop(&mut self) {
        let mut active = hold(active_runs());
        for key in &self.keys {
            if active.entries.get(key) == Some(&self.token) {
                active.entries.remove(key);
            }
        }
    }
}

/// One process-wide reservation for a configured pane-input cohort.
#[derive(Clone, Debug)]
pub(crate) struct PaneReservation(Arc<LeaseInner>);

impl PaneReservation {
    fn permits(&self, key: &RunKey, token: u64) -> bool {
        self.0.keys.contains(key) && self.0.token == token
    }
}

fn reservation_keys(
    generation: ServerGeneration,
    endpoint: &Path,
    panes: &[String],
) -> Option<Vec<RunKey>> {
    let endpoint = endpoint_identity(endpoint).ok()?;
    let mut panes = panes.to_vec();
    panes.sort_unstable();
    panes.dedup();
    Some(
        panes
            .into_iter()
            .map(|pane| RunKey {
                generation,
                endpoint,
                pane,
            })
            .collect(),
    )
}

/// Reserve every pane until the input completes or its watcher proves release.
pub(crate) fn reserve(
    generation: ServerGeneration,
    endpoint: &Path,
    panes: &[String],
) -> Option<PaneReservation> {
    let keys = reservation_keys(generation, endpoint, panes)?;
    if keys.is_empty() {
        return None;
    }
    let mut active = hold(active_runs());
    if keys.iter().any(|key| active.entries.contains_key(key)) {
        return None;
    }
    active.next = active.next.wrapping_add(1).max(1);
    let token = active.next;
    for key in &keys {
        active.entries.insert(key.clone(), token);
    }
    Some(PaneReservation(Arc::new(LeaseInner { keys, token })))
}

/// Whether input would cross an active run other than the permitted one.
pub(crate) fn is_reserved(
    generation: ServerGeneration,
    endpoint: &Path,
    pane: &str,
    permitted: Option<&PaneReservation>,
) -> bool {
    let Some(endpoint) = endpoint_identity(endpoint).ok() else {
        return true;
    };
    let key = RunKey {
        generation,
        endpoint,
        pane: pane.to_owned(),
    };
    let active = hold(active_runs());
    active
        .entries
        .get(&key)
        .is_some_and(|token| !permitted.is_some_and(|lease| lease.permits(&key, *token)))
}

/// Prove that this reservation still owns exactly the same cohort and route.
pub(crate) fn owns(
    lease: &PaneReservation,
    generation: ServerGeneration,
    endpoint: &Path,
    panes: &[String],
) -> bool {
    let Some(keys) = reservation_keys(generation, endpoint, panes) else {
        return false;
    };
    if keys != lease.0.keys {
        return false;
    }
    let active = hold(active_runs());
    keys.iter().all(|key| {
        active
            .entries
            .get(key)
            .is_some_and(|token| lease.permits(key, *token))
    })
}

fn retain_lease_until(proof: impl Future<Output = ()> + Send + 'static, lease: PaneReservation) {
    tokio::spawn(async move {
        proof.await;
        drop(lease);
    });
}

pub(crate) struct RunTransport<'a> {
    pub(crate) server: &'a libtmux::Server,
    pub(crate) generation: ServerGeneration,
    pub(crate) executable: &'a OsStr,
    pub(crate) endpoint: &'a Path,
    pub(crate) shell: &'a [u8],
    pub(crate) lease: PaneReservation,
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
    transport: RunTransport<'_>,
    final_check: impl Future<Output = Result<(), ErrorData>>,
) -> Result<RunView, RunError> {
    let RunTransport {
        server,
        generation,
        executable,
        endpoint,
        shell,
        lease,
    } = transport;
    let prepared =
        exec::prepare_run(pane, command, suppress_history, executable, endpoint, shell).await?;
    if let Err(error) = final_check.await {
        let _ = prepared.shutdown().await;
        return Err(RunError::Guard(error));
    }
    let run = match prepared.dispatch().await {
        exec::RunDispatch::Confirmed(run) => run,
        exec::RunDispatch::NotDispatched(error) => return Err(RunError::Tmux(error)),
        exec::RunDispatch::Unknown { run, error } => {
            let proof = run.proof(server.clone(), generation);
            let server = server.clone();
            retain_lease_until(
                async move {
                    let collection = run.collect(server, generation, |_| {});
                    tokio::pin!(collection);
                    tokio::select! {
                        biased;
                        collected = &mut collection => collected.finish_proof().await,
                        () = proof.wait() => {}
                    }
                },
                lease,
            );
            return Err(RunError::DispatchUnknown(Box::new(error)));
        }
    };

    let pane_id = pane.id().to_string();
    let progress = Arc::new(Mutex::new(Progress::new()));
    let update = Arc::clone(&progress);
    let proof = run.proof(server.clone(), generation);
    let result = {
        let mut collected = Box::pin(run.collect(server.clone(), generation, move |delta| {
            hold(&update).apply(delta);
        }));
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
            retain_lease_until(
                async move {
                    tokio::select! {
                        biased;
                        collection = collected => collection.finish_proof().await,
                        () = proof.wait() => {}
                    }
                },
                lease,
            );
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
        reason = "the guard serializes the process-wide reservation registry"
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
        let refusal = ErrorData::invalid_params("refused".to_owned(), None);
        let generation = guard
            .server()
            .generation()
            .await
            .expect("fixture generation reads");
        let panes = vec![pane.id().to_string()];
        let lease = reserve(generation, guard.server().socket_path(), &panes)
            .expect("the fixture pane is unreserved");

        // The count is read after `run` returns, so a shutdown that had not
        // completed first would report zero.
        let (result, shutdowns) = exec::observing_prepared_shutdowns(run(
            &pane,
            "printf should-not-run",
            Duration::from_secs(2),
            false,
            &CancellationToken::new(),
            RunTransport {
                server: guard.server(),
                generation,
                executable: executable.as_os_str(),
                endpoint: guard.server().socket_path(),
                shell: b"sh",
                lease,
            },
            async { Err(refusal) },
        ))
        .await;

        assert!(matches!(result, Err(RunError::Guard(_))));
        assert_eq!(shutdowns, 1, "the refused run shuts its watcher down once");
        assert!(
            reserve(generation, guard.server().socket_path(), &panes).is_some(),
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

    #[tokio::test]
    #[allow(
        clippy::await_holding_invalid_type,
        reason = "the guard serializes the process-wide reservation registry"
    )]
    async fn a_replacement_daemon_can_reserve_its_reused_pane_id() {
        let _serial = CLEANUP_TEST.lock().await;
        let guard = libtmux::test::TestServer::builder()
            .start()
            .await
            .expect("tmux starts");
        let server = guard.server();
        let first_session = server
            .new_session("first-generation")
            .await
            .expect("first session starts");
        let first_pane = first_session
            .panes()
            .await
            .expect("first panes list")
            .remove(0)
            .id()
            .to_string();
        let first_generation = server.generation().await.expect("first generation");
        let first_panes = vec![first_pane.clone()];
        let first_lease = reserve(first_generation, server.socket_path(), &first_panes)
            .expect("the first generation reserves its pane");
        let executable = server
            .resolved_tmux_executable()
            .expect("fixture tmux resolves");
        let socket = server.socket_path().to_path_buf();

        server.kill().await.expect("first daemon stops");
        libtmux::test::retry_until(Duration::from_secs(5), async || !server.is_alive().await)
            .await
            .expect("the first daemon goes away");
        let replacement = libtmux::Server::builder()
            .tmux_executable(executable)
            .socket_path(&socket)
            .build()
            .expect("replacement route builds");
        let second_session = replacement
            .new_session("second-generation")
            .await
            .expect("replacement session starts");
        let second_pane = second_session
            .panes()
            .await
            .expect("replacement panes list")
            .remove(0)
            .id()
            .to_string();
        let second_generation = replacement.generation().await.expect("second generation");
        let second_panes = vec![second_pane.clone()];
        let replacement_lease =
            reserve(second_generation, replacement.socket_path(), &second_panes)
                .expect("the replacement generation reserves its pane");
        let replacement_still_owned = owns(
            &replacement_lease,
            second_generation,
            replacement.socket_path(),
            &second_panes,
        );
        drop(first_lease);
        let replacement_survived_first_release = owns(
            &replacement_lease,
            second_generation,
            replacement.socket_path(),
            &second_panes,
        );
        drop(replacement_lease);
        replacement.kill().await.expect("replacement daemon stops");
        guard.shutdown().await.expect("tmux fixture shuts down");

        assert_eq!(first_pane, second_pane, "tmux reuses the first pane ID");
        assert_ne!(
            first_generation, second_generation,
            "the replacement is a distinct server generation"
        );
        assert!(
            replacement_still_owned && replacement_survived_first_release,
            "releasing the old daemon's lease cannot release the replacement's reservation"
        );
    }

    #[tokio::test]
    #[allow(
        clippy::await_holding_invalid_type,
        reason = "the guard serializes the process-wide reservation registry"
    )]
    async fn endpoint_aliases_share_one_pane_reservation() {
        let _serial = CLEANUP_TEST.lock().await;
        let guard = libtmux::test::TestServer::builder()
            .start()
            .await
            .expect("tmux starts");
        let server = guard.server();
        let pane = server
            .new_session("physical-reservation")
            .await
            .expect("session starts")
            .panes()
            .await
            .expect("panes list")
            .remove(0)
            .id()
            .to_string();
        let generation = server.generation().await.expect("server generation");
        let panes = vec![pane.clone()];
        let socket = server.socket_path();
        let hard_link = socket.with_file_name("reservation-hard-link.sock");
        let symbolic_link = socket.with_file_name("reservation-symbolic-link.sock");
        std::fs::hard_link(socket, &hard_link).expect("socket hard link is created");
        std::os::unix::fs::symlink(socket, &symbolic_link)
            .expect("socket symbolic link is created");

        let lease = reserve(generation, socket, &panes).expect("source route reserves the pane");

        for alias in [&hard_link, &symbolic_link] {
            assert!(
                reserve(generation, alias, &panes).is_none(),
                "a physical endpoint alias cannot reserve the same pane"
            );
            assert!(
                is_reserved(generation, alias, &pane, None),
                "the active reservation is visible through every endpoint alias"
            );
            assert!(
                owns(&lease, generation, alias, &panes),
                "the lease owns its cohort through every endpoint alias"
            );
        }

        drop(lease);
        std::fs::remove_file(&hard_link).expect("socket hard link is removed");
        std::fs::remove_file(&symbolic_link).expect("socket symbolic link is removed");
        guard.shutdown().await.expect("tmux fixture shuts down");
    }
}
