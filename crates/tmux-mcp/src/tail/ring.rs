//! Retained pane output with absolute cursor offsets and filter checkpoints.

use crate::identity::InstanceId;
use crate::text::{TextFilter, readable_from};

use super::Cursor;

/// How much of one pane's output a tail keeps.
pub(super) const RING_BYTES: usize = 256 * 1024;

/// One pane's ring of recent output.
#[derive(Debug)]
pub(super) struct Ring {
    pub(super) bytes: Vec<u8>,
    /// The absolute offset of `bytes[0]`, counting from the tail's start.
    pub(super) start: u64,
    /// Filter state immediately before `bytes[0]`.
    pub(super) checkpoint: TextFilter,
    pub(super) closed: bool,
}

#[derive(Debug)]
pub(super) struct RingRead {
    pub(super) bytes: Vec<u8>,
    pub(super) checkpoint: TextFilter,
    pub(super) from: usize,
    pub(super) missed: bool,
}

impl RingRead {
    pub(super) fn text(&self) -> String {
        readable_from(&self.checkpoint, &self.bytes, self.from)
    }
}

impl Ring {
    pub(super) fn new() -> Self {
        Self {
            bytes: Vec::new(),
            start: 0,
            checkpoint: TextFilter::new(),
            closed: false,
        }
    }

    pub(super) fn end(&self) -> u64 {
        self.start + self.bytes.len() as u64
    }

    pub(super) fn push(&mut self, chunk: &[u8]) {
        if chunk.len() >= RING_BYTES {
            let retained = chunk.len() - RING_BYTES;
            self.checkpoint.advance(&self.bytes);
            self.checkpoint.advance(&chunk[..retained]);
            self.start += (self.bytes.len() + retained) as u64;
            self.bytes = chunk[retained..].to_vec();
            return;
        }

        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > RING_BYTES {
            let excess = self.bytes.len() - RING_BYTES;
            self.checkpoint.advance(&self.bytes[..excess]);
            self.bytes.drain(..excess);
            self.start += excess as u64;
        }
    }

    /// Read from `offset`, saying whether anything before it was lost.
    pub(super) fn read_from(&self, offset: u64) -> (&[u8], bool) {
        if offset < self.start {
            return (&self.bytes, true);
        }
        if offset > self.end() {
            return (&[], true);
        }
        let from = usize::try_from(offset - self.start).unwrap_or(self.bytes.len());
        (self.bytes.get(from..).unwrap_or_default(), false)
    }

    pub(super) fn snapshot_from(&self, offset: u64) -> RingRead {
        let (bytes, missed) = self.read_from(offset);
        if bytes.is_empty() {
            return RingRead {
                bytes: Vec::new(),
                checkpoint: self.checkpoint.clone(),
                from: 0,
                missed,
            };
        }
        RingRead {
            bytes: self.bytes.clone(),
            checkpoint: self.checkpoint.clone(),
            from: self.bytes.len() - bytes.len(),
            missed,
        }
    }
}

/// Where a read should resume, and whether the gap before it is unaccounted
/// for.
///
/// A cursor from another owner or tail cannot name a point in this ring.
pub(super) fn resume_at(
    ring: &Ring,
    cursor: Option<&Cursor>,
    owner: InstanceId,
    epoch: u64,
) -> (u64, bool) {
    match cursor {
        Some(cursor) if cursor.owner == owner && cursor.epoch == epoch => (cursor.offset, false),
        Some(_) => (ring.end(), true),
        None => (ring.end(), false),
    }
}
