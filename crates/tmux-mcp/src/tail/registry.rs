//! Owned tail readers and the bounded least-recently-read registry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use libtmux::{Error, Pane, TmuxText};
use tokio::sync::{mpsc, oneshot};

use super::hold;
use super::ring::Ring;

/// How many baseline requests may wait behind the one being captured.
const SNAPSHOT_QUEUE: usize = 1;

#[derive(Debug)]
pub(super) struct SnapshotRequest {
    pub(super) result: oneshot::Sender<Result<TailSnapshot, Error>>,
}

#[derive(Debug)]
pub(super) struct TailSnapshot {
    pub(super) visible: Vec<TmuxText>,
    pub(super) offset: u64,
}

#[derive(Debug)]
pub(super) enum SnapshotError {
    Tmux(Error),
    Busy { limit: usize },
    Stopped,
}

pub(super) async fn next_live_snapshot(
    requests: &mut mpsc::Receiver<SnapshotRequest>,
) -> Option<SnapshotRequest> {
    while let Some(request) = requests.recv().await {
        if !request.result.is_closed() {
            return Some(request);
        }
    }
    None
}

/// The clonable state a caller may retain without owning the reader task.
#[derive(Clone, Debug)]
pub(super) struct RetainedTail {
    pub(super) epoch: u64,
    pub(super) ring: Arc<Mutex<Ring>>,
    pub(super) snapshots: mpsc::Sender<SnapshotRequest>,
}

impl RetainedTail {
    pub(super) async fn snapshot(&self) -> Result<TailSnapshot, SnapshotError> {
        let (result, answer) = oneshot::channel();
        self.snapshots
            .try_send(SnapshotRequest { result })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => SnapshotError::Busy {
                    limit: SNAPSHOT_QUEUE,
                },
                mpsc::error::TrySendError::Closed(_) => SnapshotError::Stopped,
            })?;
        answer
            .await
            .map_err(|_| SnapshotError::Stopped)?
            .map_err(SnapshotError::Tmux)
    }
}

/// One tailed pane.
#[derive(Debug)]
pub(super) struct Tail {
    pub(super) epoch: u64,
    pub(super) ring: Arc<Mutex<Ring>>,
    pub(super) snapshots: mpsc::Sender<SnapshotRequest>,
    pub(super) reader: tokio::task::JoinHandle<()>,
    pub(super) last_read: Instant,
}

impl Tail {
    /// Attach one owned pane reader before the registry can publish it.
    pub(super) async fn attach(pane: &Pane, epoch: u64) -> Result<Self, Error> {
        let mut output = pane.stream_output().await?;
        let ring = Arc::new(Mutex::new(Ring::new()));
        let (snapshots, mut requests) = mpsc::channel::<SnapshotRequest>(SNAPSHOT_QUEUE);
        let reader = {
            let ring = Arc::clone(&ring);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        Some(request) = next_live_snapshot(&mut requests) => {
                            let result = match output
                                .snapshot(|preceding| hold(&ring).push(preceding))
                                .await
                            {
                                Ok(visible) => Ok(TailSnapshot {
                                    visible,
                                    offset: hold(&ring).end(),
                                }),
                                Err(error) => Err(error),
                            };
                            let _ = request.result.send(result);
                        }
                        chunk = output.next_chunk() => {
                            let Some(chunk) = chunk else {
                                break;
                            };
                            hold(&ring).push(&chunk);
                        }
                    }
                }
                hold(&ring).closed = true;
            })
        };

        Ok(Self {
            epoch,
            ring,
            snapshots,
            reader,
            last_read: Instant::now(),
        })
    }

    pub(super) fn retained(&self) -> RetainedTail {
        RetainedTail {
            epoch: self.epoch,
            ring: Arc::clone(&self.ring),
            snapshots: self.snapshots.clone(),
        }
    }
}

impl Drop for Tail {
    fn drop(&mut self) {
        // Dropping the reader drops its `PaneOutput`, which is how libtmux is
        // told the connection is no longer wanted.
        self.reader.abort();
    }
}

/// Established tails retained by one server.
#[derive(Debug)]
pub(super) struct TailTable {
    tails: HashMap<String, Tail>,
    limit: usize,
}

impl TailTable {
    pub(super) fn new(limit: usize) -> Self {
        assert!(limit > 0, "a tail table needs replacement capacity");
        Self {
            tails: HashMap::new(),
            limit,
        }
    }

    /// Mark a tail as recently read, and hand back its retained ring.
    pub(super) fn touch(&mut self, id: &str) -> Option<RetainedTail> {
        let tail = self.tails.get_mut(id)?;
        tail.last_read = Instant::now();
        Some(tail.retained())
    }

    /// Publish an attached reader, evicting the least recently read tail.
    pub(super) fn insert(&mut self, id: String, tail: Tail) {
        if self.tails.len() >= self.limit
            && let Some(stale) = self
                .tails
                .iter()
                .min_by_key(|(_, tail)| tail.last_read)
                .map(|(id, _)| id.clone())
        {
            self.tails.remove(&stale);
        }
        self.tails.insert(id, tail);
    }

    /// Remove a stopped reader without evicting a newer replacement.
    pub(super) fn remove_if_epoch(&mut self, id: &str, epoch: u64) {
        if self.tails.get(id).is_some_and(|tail| tail.epoch == epoch) {
            self.tails.remove(id);
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.tails.len()
    }

    #[cfg(test)]
    pub(super) fn contains(&self, id: &str) -> bool {
        self.tails.contains_key(id)
    }
}
