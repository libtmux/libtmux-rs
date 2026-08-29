//! Live tails, so "what has this pane written since I last looked" has an
//! exact answer.
//!
//! The obvious way to answer it is to anchor into scrollback and capture from
//! there next time. That anchor is not the crate's to keep: tmux frees the
//! oldest scrollback once `history-limit` is reached, and a pane that is
//! productive enough to be worth tailing is exactly the pane whose anchor gets
//! freed. The loss is silent, which is the worst part.
//!
//! So the bytes are kept here instead. A tail attaches once and holds a ring
//! of what the pane wrote; a cursor names an offset in that ring. A cursor
//! that no longer names retained output reports the gap.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use libtmux::{Error, Pane};

use crate::identity::{InstanceId, InstanceIdentity};
use crate::text::{TextFilter, readable_from};

/// Take a lock, treating a poisoned one as held rather than as fatal.
///
/// Everything these locks guard is a plain buffer whose invariants a panic
/// cannot break, and a reader task that died is a reason to keep serving what
/// it already collected rather than to fail every later call.
fn hold<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How much of one pane's output a tail keeps.
const RING_BYTES: usize = 256 * 1024;

/// How many panes may be tailed at once.
///
/// Each tail holds a control-mode connection, so this is a real resource. The
/// least recently read is dropped to make room.
const MAX_TAILS: usize = 8;

/// A place in one pane's output.
///
/// Opaque by contract: it is rendered as text for the protocol to carry, and
/// callers pass back what they were given rather than constructing one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cursor {
    pane: String,
    owner: InstanceId,
    epoch: u64,
    offset: u64,
}

impl Cursor {
    /// Render the cursor for a caller to hold.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.pane, self.owner, self.epoch, self.offset
        )
    }

    /// Read back a cursor this crate rendered.
    ///
    /// # Errors
    ///
    /// Returns the given text when it is not one of ours.
    pub fn decode(text: &str) -> Result<Self, &str> {
        let mut fields = text.rsplitn(4, ':');
        let offset = fields.next().and_then(|field| field.parse().ok());
        let epoch = fields.next().and_then(|field| field.parse().ok());
        let owner = fields.next().and_then(InstanceId::decode);
        let pane = fields.next();
        match (pane, owner, epoch, offset) {
            (Some(pane), Some(owner), Some(epoch), Some(offset)) if pane.starts_with('%') => {
                Ok(Self {
                    pane: pane.to_owned(),
                    owner,
                    epoch,
                    offset,
                })
            }
            _ => Err(text),
        }
    }

    /// The pane this cursor belongs to.
    #[must_use]
    pub fn pane(&self) -> &str {
        &self.pane
    }
}

/// What a pane wrote since a cursor.
#[derive(Debug)]
pub(crate) struct Since {
    /// The text, with escape sequences removed.
    pub text: String,
    /// Where to resume.
    pub cursor: Cursor,
    /// Whether output between the cursor and this text was dropped.
    ///
    /// True when the cursor no longer names retained output in this tail.
    pub missed: bool,
    /// Whether the pane has stopped writing for good.
    pub closed: bool,
}

/// One pane's ring of recent output.
#[derive(Debug)]
struct Ring {
    bytes: Vec<u8>,
    /// The absolute offset of `bytes[0]`, counting from the tail's start.
    start: u64,
    /// Filter state immediately before `bytes[0]`.
    checkpoint: TextFilter,
    closed: bool,
}

#[derive(Debug)]
struct RingRead {
    bytes: Vec<u8>,
    checkpoint: TextFilter,
    from: usize,
    missed: bool,
}

impl RingRead {
    fn text(&self) -> String {
        readable_from(&self.checkpoint, &self.bytes, self.from)
    }
}

impl Ring {
    fn end(&self) -> u64 {
        self.start + self.bytes.len() as u64
    }

    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > RING_BYTES {
            let excess = self.bytes.len() - RING_BYTES;
            self.checkpoint.advance(&self.bytes[..excess]);
            self.bytes.drain(..excess);
            self.start += excess as u64;
        }
    }

    /// Read from `offset`, saying whether anything before it was lost.
    fn read_from(&self, offset: u64) -> (&[u8], bool) {
        if offset < self.start {
            return (&self.bytes, true);
        }
        if offset > self.end() {
            return (&[], true);
        }
        let from = usize::try_from(offset - self.start).unwrap_or(self.bytes.len());
        (self.bytes.get(from..).unwrap_or_default(), false)
    }

    fn snapshot_from(&self, offset: u64) -> RingRead {
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
fn resume_at(ring: &Ring, cursor: Option<&Cursor>, owner: InstanceId, epoch: u64) -> (u64, bool) {
    match cursor {
        Some(cursor) if cursor.owner == owner && cursor.epoch == epoch => (cursor.offset, false),
        Some(_) => (ring.end(), true),
        None => (ring.end(), false),
    }
}

/// One tailed pane.
#[derive(Debug)]
struct Tail {
    epoch: u64,
    ring: Arc<Mutex<Ring>>,
    reader: tokio::task::JoinHandle<()>,
    last_read: Instant,
}

impl Drop for Tail {
    fn drop(&mut self) {
        // Dropping the reader drops its `PaneOutput`, which is how libtmux is
        // told the connection is no longer wanted.
        self.reader.abort();
    }
}

/// The tails this server is holding.
#[derive(Debug)]
pub(crate) struct Tails {
    identity: Arc<InstanceIdentity>,
    inner: Mutex<HashMap<String, Tail>>,
    next_epoch: AtomicU64,
}

#[derive(Debug)]
pub(crate) enum TailError {
    Tmux(Error),
    OwnerUnavailable,
}

impl From<Error> for TailError {
    fn from(error: Error) -> Self {
        Self::Tmux(error)
    }
}

impl Tails {
    /// Hold no tails yet.
    #[must_use]
    pub(crate) fn new(identity: Arc<InstanceIdentity>) -> Self {
        Self {
            identity,
            inner: Mutex::new(HashMap::new()),
            next_epoch: AtomicU64::new(0),
        }
    }

    #[cfg(test)]
    fn with_owner(owner: u128) -> Self {
        Self::new(Arc::new(InstanceIdentity::fixed(owner)))
    }

    fn owner(&self) -> Result<InstanceId, TailError> {
        self.identity.get().map_err(|_| TailError::OwnerUnavailable)
    }

    /// Read what a pane wrote since `cursor`, opening a tail if needed.
    ///
    /// A call with no cursor starts one and returns nothing but a place to
    /// resume from: there is no history to report, because the tail did not
    /// exist to record any.
    ///
    /// # Errors
    ///
    /// Returns an error when a cursor identity cannot be generated or the pane
    /// cannot be watched.
    pub(crate) async fn read(
        &self,
        pane: &Pane,
        cursor: Option<&Cursor>,
    ) -> Result<Since, TailError> {
        let owner = self.owner()?;
        let id = pane.id().to_string();

        let opened = self.ensure(pane, &id).await?;
        let (ring, epoch) = opened;

        let (read, missed, closed, end) = {
            let ring = hold(&ring);
            let (from, stale) = resume_at(&ring, cursor, owner, epoch);
            let read = ring.snapshot_from(from);
            let missed = read.missed || stale;
            (read, missed, ring.closed, ring.end())
        };

        Ok(Since {
            text: read.text(),
            cursor: Cursor {
                pane: id,
                owner,
                epoch,
                offset: end,
            },
            // A first call has nothing to have missed.
            missed: cursor.is_some() && missed,
            closed,
        })
    }

    /// Return the ring for a pane, attaching a tail if there is not one.
    async fn ensure(&self, pane: &Pane, id: &str) -> Result<(Arc<Mutex<Ring>>, u64), Error> {
        if let Some(found) = self.touch(id) {
            return Ok(found);
        }

        // Attaching is the slow part, and it must not happen under a lock.
        let mut output = pane.stream_output().await?;
        let epoch = self.next_epoch();
        let ring = Arc::new(Mutex::new(Ring {
            bytes: Vec::new(),
            start: 0,
            checkpoint: TextFilter::new(),
            closed: false,
        }));

        let reader = {
            let ring = Arc::clone(&ring);
            tokio::spawn(async move {
                while let Some(chunk) = output.next_chunk().await {
                    hold(&ring).push(&chunk);
                }
                hold(&ring).closed = true;
            })
        };

        let mut tails = hold(&self.inner);
        // Another call may have attached the same pane while this one was
        // awaiting. Theirs is as good as ours, and keeping one avoids two
        // connections to the same pane.
        if let Some(existing) = tails.get_mut(id) {
            existing.last_read = Instant::now();
            reader.abort();
            return Ok((Arc::clone(&existing.ring), existing.epoch));
        }
        if tails.len() >= MAX_TAILS
            && let Some(stale) = tails
                .iter()
                .min_by_key(|(_, tail)| tail.last_read)
                .map(|(id, _)| id.clone())
        {
            tails.remove(&stale);
        }
        tails.insert(
            id.to_owned(),
            Tail {
                epoch,
                ring: Arc::clone(&ring),
                reader,
                last_read: Instant::now(),
            },
        );

        Ok((ring, epoch))
    }

    /// Mark a tail as recently read, and hand back its ring.
    fn touch(&self, id: &str) -> Option<(Arc<Mutex<Ring>>, u64)> {
        let mut tails = hold(&self.inner);
        let tail = tails.get_mut(id)?;
        tail.last_read = Instant::now();
        Some((Arc::clone(&tail.ring), tail.epoch))
    }

    /// The next tail epoch for this owner.
    fn next_epoch(&self) -> u64 {
        self.next_epoch
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring() -> Ring {
        Ring {
            bytes: Vec::new(),
            start: 0,
            checkpoint: TextFilter::new(),
            closed: false,
        }
    }

    fn tails_with_owner(owner: u128) -> Tails {
        Tails::with_owner(owner)
    }

    fn cursor_for(tails: &Tails, pane: &str, epoch: u64, offset: u64) -> Cursor {
        Cursor {
            pane: pane.to_owned(),
            owner: tails.owner().expect("the test owner is available"),
            epoch,
            offset,
        }
    }

    fn resume_for(tails: &Tails, ring: &Ring, cursor: Option<&Cursor>, epoch: u64) -> (u64, bool) {
        resume_at(
            ring,
            cursor,
            tails.owner().expect("the test owner is available"),
            epoch,
        )
    }

    #[test]
    fn a_cursor_survives_a_round_trip() {
        let tails = tails_with_owner(1);
        let cursor = cursor_for(&tails, "%12", 3, 4096);

        assert_eq!(
            cursor.encode(),
            "%12:00000000000000000000000000000001:3:4096"
        );
        assert_eq!(Cursor::decode(&cursor.encode()), Ok(cursor));
    }

    #[test]
    fn foreign_text_is_not_a_cursor() {
        assert!(Cursor::decode("nonsense").is_err());
        assert!(
            Cursor::decode("%1:1:0").is_err(),
            "old cursors lack an owner"
        );
        assert!(
            Cursor::decode("%1:notanowner:1:0").is_err(),
            "an owner is fixed-width hexadecimal"
        );
        assert!(
            Cursor::decode("1:00000000000000000000000000000001:2:3").is_err(),
            "a pane id always starts with %"
        );
    }

    #[test]
    fn a_ring_reads_from_an_offset() {
        let mut ring = ring();
        ring.push(b"hello world");

        assert_eq!(ring.read_from(6), (&b"world"[..], false));
        assert_eq!(ring.end(), 11);
    }

    #[test]
    fn reading_at_the_end_yields_nothing() {
        let mut ring = ring();
        ring.push(b"hello");

        assert_eq!(ring.read_from(5), (&b""[..], false));
    }

    #[test]
    fn an_offset_past_the_end_admits_the_cursor_is_invalid() {
        let mut ring = ring();
        ring.push(b"hello");

        assert_eq!(ring.read_from(99), (&b""[..], true));
    }

    #[test]
    fn overflow_drops_the_oldest_and_says_so() {
        let mut ring = ring();
        let mut stream = b"\x1b[31mred".to_vec();
        stream.resize(RING_BYTES + 4, b'a');
        ring.push(&stream);

        assert_eq!(ring.start, 4);
        assert_eq!(ring.end(), RING_BYTES as u64 + 4);
        let (bytes, missed) = ring.read_from(0);
        assert!(missed, "the bytes at offset 0 are gone");
        assert_eq!(bytes.len(), RING_BYTES);
        let read = ring.snapshot_from(0);
        let text = read.text();
        assert!(text.starts_with("red"));
        assert_eq!(text.len(), RING_BYTES - 1);
    }

    #[test]
    fn a_first_read_resumes_at_the_end_and_has_missed_nothing() {
        let tails = tails_with_owner(1);
        let mut ring = ring();
        ring.push(b"written before anyone looked");

        assert_eq!(resume_for(&tails, &ring, None, 1), (ring.end(), false));
    }

    #[test]
    fn a_cursor_from_this_tail_resumes_where_it_says() {
        let tails = tails_with_owner(1);
        let mut ring = ring();
        ring.push(b"hello");
        let cursor = cursor_for(&tails, "%1", 1, 2);

        assert_eq!(resume_for(&tails, &ring, Some(&cursor), 1), (2, false));
    }

    #[test]
    fn a_cursor_from_an_evicted_tail_admits_the_gap() {
        let tails = tails_with_owner(1);
        let mut ring = ring();
        ring.push(b"written after the tail came back");
        let cursor = cursor_for(&tails, "%1", 1, 0);

        assert_eq!(
            resume_for(&tails, &ring, Some(&cursor), 2),
            (ring.end(), true),
            "an offset from a previous tail names a place in a buffer that is gone"
        );
    }

    #[test]
    fn a_cursor_from_a_fresh_owner_admits_the_gap() {
        let first = tails_with_owner(1);
        let second = tails_with_owner(2);
        let mut ring = ring();
        ring.push(b"written after the server restarted");
        let cursor = cursor_for(&first, "%1", first.next_epoch(), 0);
        let epoch = second.next_epoch();

        assert_eq!(
            resume_for(&second, &ring, Some(&cursor), epoch),
            (ring.end(), true),
            "an offset from another cursor owner names a different buffer"
        );
    }

    #[test]
    fn tail_epochs_increase_within_one_owner() {
        let tails = Tails::new(Arc::new(InstanceIdentity::new()));

        assert_eq!(tails.next_epoch(), 1);
        assert_eq!(tails.next_epoch(), 2);
    }

    #[test]
    fn a_cursor_still_inside_the_ring_is_not_a_miss() {
        let mut ring = ring();
        ring.push(&vec![b'a'; RING_BYTES]);
        ring.push(b"tail");

        let (bytes, missed) = ring.read_from(ring.end() - 4);
        assert!(!missed);
        assert_eq!(bytes, b"tail");
    }

    #[test]
    fn a_cursor_inside_a_control_sequence_resumes_its_state() {
        let mut ring = ring();
        ring.push(b"before\x1b[31");
        let cursor = ring.end();
        ring.push(b"mred");

        let read = ring.snapshot_from(cursor);

        assert_eq!(read.text(), "red");
        assert!(!read.missed);
    }

    #[test]
    fn reusing_an_old_cursor_preserves_a_pending_return() {
        let mut ring = ring();
        ring.push(b"working\r");
        let cursor = ring.end();
        ring.push(b"done");

        assert_eq!(ring.snapshot_from(cursor).text(), "\ndone");

        ring.push(b"!\n");

        assert_eq!(ring.snapshot_from(cursor).text(), "\ndone!\n");
    }
}
