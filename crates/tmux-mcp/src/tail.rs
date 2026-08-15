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
//! of what the pane wrote; a cursor names an offset in that ring. Output is
//! missed only when the ring itself overflows, which is a thing this crate can
//! see and say.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use libtmux::{Error, Pane};

use crate::text::TextFilter;

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
    epoch: u64,
    offset: u64,
}

impl Cursor {
    /// Render the cursor for a caller to hold.
    #[must_use]
    pub fn encode(&self) -> String {
        format!("{}:{}:{}", self.pane, self.epoch, self.offset)
    }

    /// Read back a cursor this crate rendered.
    ///
    /// # Errors
    ///
    /// Returns the given text when it is not one of ours.
    pub fn decode(text: &str) -> Result<Self, &str> {
        let mut fields = text.rsplitn(3, ':');
        let offset = fields.next().and_then(|field| field.parse().ok());
        let epoch = fields.next().and_then(|field| field.parse().ok());
        let pane = fields.next();
        match (pane, epoch, offset) {
            (Some(pane), Some(epoch), Some(offset)) if pane.starts_with('%') => Ok(Self {
                pane: pane.to_owned(),
                epoch,
                offset,
            }),
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
    /// True when the ring overflowed, and when the cursor predates the tail
    /// that is answering — a tail evicted and reopened cannot vouch for what
    /// happened while it was gone.
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
    closed: bool,
}

impl Ring {
    fn end(&self) -> u64 {
        self.start + self.bytes.len() as u64
    }

    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > RING_BYTES {
            let excess = self.bytes.len() - RING_BYTES;
            self.bytes.drain(..excess);
            self.start += excess as u64;
        }
    }

    /// Read from `offset`, saying whether anything before it was lost.
    fn read_from(&self, offset: u64) -> (&[u8], bool) {
        if offset < self.start {
            return (&self.bytes, true);
        }
        let from = usize::try_from(offset - self.start).unwrap_or(self.bytes.len());
        (self.bytes.get(from..).unwrap_or_default(), false)
    }
}

/// Where a read should resume, and whether the gap before it is unaccounted
/// for.
///
/// A cursor issued by a previous tail is the interesting case. Tails are
/// evicted to bound how many control-mode connections this server holds, and
/// whatever a pane wrote while none was attached is exactly what no one can
/// recover. Resuming at the end and saying so is the only honest answer.
fn resume_at(ring: &Ring, cursor: Option<&Cursor>, epoch: u64) -> (u64, bool) {
    match cursor {
        Some(cursor) if cursor.epoch == epoch => (cursor.offset, false),
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
#[derive(Debug, Default)]
pub(crate) struct Tails {
    inner: Mutex<HashMap<String, Tail>>,
    epochs: Mutex<HashMap<String, u64>>,
}

impl Tails {
    /// Hold no tails yet.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Read what a pane wrote since `cursor`, opening a tail if needed.
    ///
    /// A call with no cursor starts one and returns nothing but a place to
    /// resume from: there is no history to report, because the tail did not
    /// exist to record any.
    ///
    /// # Errors
    ///
    /// Returns an error when the pane cannot be watched.
    pub(crate) async fn read(&self, pane: &Pane, cursor: Option<&Cursor>) -> Result<Since, Error> {
        let id = pane.id().to_string();

        let opened = self.ensure(pane, &id).await?;
        let (ring, epoch) = opened;

        let (bytes, missed, closed, end) = {
            let ring = hold(&ring);
            let (from, stale) = resume_at(&ring, cursor, epoch);
            let (bytes, dropped) = ring.read_from(from);
            (bytes.to_vec(), dropped || stale, ring.closed, ring.end())
        };

        let mut filter = TextFilter::new();
        let mut text = Vec::new();
        filter.push(&bytes, &mut text);

        Ok(Since {
            text: String::from_utf8_lossy(&text).into_owned(),
            cursor: Cursor {
                pane: id,
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
        let epoch = self.next_epoch(id);
        let ring = Arc::new(Mutex::new(Ring {
            bytes: Vec::new(),
            start: 0,
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

    /// The next epoch for a pane, so a reopened tail is distinguishable.
    fn next_epoch(&self, id: &str) -> u64 {
        let mut epochs = hold(&self.epochs);
        let epoch = epochs.entry(id.to_owned()).or_insert(0);
        *epoch += 1;
        *epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring() -> Ring {
        Ring {
            bytes: Vec::new(),
            start: 0,
            closed: false,
        }
    }

    #[test]
    fn a_cursor_survives_a_round_trip() {
        let cursor = Cursor {
            pane: "%12".to_owned(),
            epoch: 3,
            offset: 4096,
        };

        assert_eq!(Cursor::decode(&cursor.encode()), Ok(cursor));
    }

    #[test]
    fn foreign_text_is_not_a_cursor() {
        assert!(Cursor::decode("nonsense").is_err());
        assert!(Cursor::decode("%1:notanumber:0").is_err());
        assert!(
            Cursor::decode("1:2:3").is_err(),
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
    fn an_offset_past_the_end_yields_nothing_rather_than_panicking() {
        let mut ring = ring();
        ring.push(b"hello");

        assert_eq!(ring.read_from(99), (&b""[..], false));
    }

    #[test]
    fn overflow_drops_the_oldest_and_says_so() {
        let mut ring = ring();
        ring.push(&vec![b'a'; RING_BYTES]);
        ring.push(b"tail");

        assert_eq!(ring.start, 4);
        assert_eq!(ring.end(), RING_BYTES as u64 + 4);
        let (bytes, missed) = ring.read_from(0);
        assert!(missed, "the bytes at offset 0 are gone");
        assert_eq!(bytes.len(), RING_BYTES);
        assert!(bytes.ends_with(b"tail"));
    }

    #[test]
    fn a_first_read_resumes_at_the_end_and_has_missed_nothing() {
        let mut ring = ring();
        ring.push(b"written before anyone looked");

        assert_eq!(resume_at(&ring, None, 1), (ring.end(), false));
    }

    #[test]
    fn a_cursor_from_this_tail_resumes_where_it_says() {
        let mut ring = ring();
        ring.push(b"hello");
        let cursor = Cursor {
            pane: "%1".to_owned(),
            epoch: 1,
            offset: 2,
        };

        assert_eq!(resume_at(&ring, Some(&cursor), 1), (2, false));
    }

    #[test]
    fn a_cursor_from_an_evicted_tail_admits_the_gap() {
        let mut ring = ring();
        ring.push(b"written after the tail came back");
        let cursor = Cursor {
            pane: "%1".to_owned(),
            epoch: 1,
            offset: 0,
        };

        assert_eq!(
            resume_at(&ring, Some(&cursor), 2),
            (ring.end(), true),
            "an offset from a previous tail names a place in a buffer that is gone"
        );
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
}
