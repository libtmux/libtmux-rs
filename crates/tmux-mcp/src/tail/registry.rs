//! Owned tail readers and the bounded least-recently-read registry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use libtmux::{Error, Pane};

use super::hold;
use super::ring::Ring;

/// One tailed pane.
#[derive(Debug)]
pub(super) struct Tail {
    pub(super) epoch: u64,
    pub(super) ring: Arc<Mutex<Ring>>,
    pub(super) reader: tokio::task::JoinHandle<()>,
    pub(super) last_read: Instant,
}

impl Tail {
    /// Attach one owned pane reader before the registry can publish it.
    pub(super) async fn attach(pane: &Pane, epoch: u64) -> Result<Self, Error> {
        let mut output = pane.stream_output().await?;
        let ring = Arc::new(Mutex::new(Ring::new()));
        let reader = {
            let ring = Arc::clone(&ring);
            tokio::spawn(async move {
                while let Some(chunk) = output.next_chunk().await {
                    hold(&ring).push(&chunk);
                }
                hold(&ring).closed = true;
            })
        };

        Ok(Self {
            epoch,
            ring,
            reader,
            last_read: Instant::now(),
        })
    }

    pub(super) fn retained(&self) -> (Arc<Mutex<Ring>>, u64) {
        (Arc::clone(&self.ring), self.epoch)
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
    pub(super) fn touch(&mut self, id: &str) -> Option<(Arc<Mutex<Ring>>, u64)> {
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

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.tails.len()
    }

    #[cfg(test)]
    pub(super) fn contains(&self, id: &str) -> bool {
        self.tails.contains_key(id)
    }
}
