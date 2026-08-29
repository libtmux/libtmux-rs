//! Bounded job slots, reservations, and terminal-only eviction.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::Notify;

use crate::identity::InstanceId;

use super::worker::Progress;
use super::{StartError, hold};

/// One background command.
#[derive(Debug)]
pub(super) struct Job {
    pub(super) pane: String,
    pub(super) command: String,
    pub(super) started: Instant,
    pub(super) progress: Arc<Mutex<Progress>>,
    /// Fires when the reader reaches a terminal state or loses its owner.
    pub(super) finished: Arc<Notify>,
    pub(super) reader: tokio::task::JoinHandle<()>,
    pub(super) last_read: Instant,
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
pub(super) enum JobSlot {
    /// Reserved while the watcher attaches, before any pane input is sent.
    Pending,
    /// A command visible to callers and owned by the table.
    Ready(Job),
}

/// Running jobs, completed jobs, and starts that are between those states.
#[derive(Debug)]
pub(super) struct JobTable {
    pub(super) slots: HashMap<String, JobSlot>,
    pub(super) limit: usize,
    pub(super) next_id: Option<u64>,
}

impl JobTable {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            slots: HashMap::new(),
            limit,
            next_id: Some(0),
        }
    }

    /// Reserve a slot, evicting only a finished job when the table is full.
    pub(super) fn reserve(&mut self, owner: InstanceId) -> Result<String, StartError> {
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
pub(super) struct Reservation<'a> {
    pub(super) table: &'a Mutex<JobTable>,
    pub(super) id: String,
    pub(super) committed: bool,
}

impl Reservation<'_> {
    pub(super) fn id(&self) -> &str {
        &self.id
    }

    pub(super) fn commit(mut self, job: Job) {
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
