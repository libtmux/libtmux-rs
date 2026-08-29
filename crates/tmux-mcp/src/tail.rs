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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use libtmux::{Error, Pane};

use crate::identity::{InstanceId, InstanceIdentity};

mod registry;
mod ring;

use registry::{Tail, TailTable};
#[cfg(test)]
use ring::RING_BYTES;
use ring::{Ring, resume_at};

/// Take a lock, treating a poisoned one as held rather than as fatal.
///
/// Everything these locks guard is a plain buffer whose invariants a panic
/// cannot break, and a reader task that died is a reason to keep serving what
/// it already collected rather than to fail every later call.
fn hold<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How many established pane tails may be retained at once.
///
/// Each tail holds a control-mode connection, so this is a real resource. The
/// least recently read is dropped after its replacement attaches. Replacement
/// attachment is serialized, but evicted clients close asynchronously, so the
/// server's persistent-client limit remains the hard process bound. A limit
/// with no replacement headroom refuses the new tail and preserves the old
/// ones.
const MAX_TAILS: usize = 8;

/// How many new tails may be attaching at once.
const MAX_TAIL_OPENERS: usize = 1;

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
    /// Whether this call established the retained tail.
    pub opened: bool,
}

/// The tails this server is holding.
#[derive(Debug)]
pub(crate) struct Tails {
    identity: Arc<InstanceIdentity>,
    inner: Mutex<TailTable>,
    opening: tokio::sync::Semaphore,
    next_epoch: AtomicU64,
}

#[derive(Debug)]
pub(crate) enum TailError {
    Tmux(Error),
    OwnerUnavailable,
    OpeningAtCapacity { limit: usize },
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
            inner: Mutex::new(TailTable::new(MAX_TAILS)),
            opening: tokio::sync::Semaphore::new(MAX_TAIL_OPENERS),
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
        let (ring, epoch, opened) = opened;

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
            opened,
        })
    }

    /// Return the ring for a pane, attaching a tail if there is not one.
    async fn ensure(
        &self,
        pane: &Pane,
        id: &str,
    ) -> Result<(Arc<Mutex<Ring>>, u64, bool), TailError> {
        if let Some(found) = self.touch(id) {
            return Ok((found.0, found.1, false));
        }

        // Do not retain an unbounded queue of tool calls behind a slow attach.
        // Existing tails bypass this admission through the fast path above.
        let _opening = self
            .opening
            .try_acquire()
            .map_err(|_| TailError::OpeningAtCapacity {
                limit: MAX_TAIL_OPENERS,
            })?;
        if let Some(found) = self.touch(id) {
            return Ok((found.0, found.1, false));
        }

        let epoch = self.next_epoch();
        let tail = Tail::attach(pane, epoch).await?;
        let (ring, epoch) = tail.retained();
        hold(&self.inner).insert(id.to_owned(), tail);

        Ok((ring, epoch, true))
    }

    /// Mark a tail as recently read, and hand back its ring.
    fn touch(&self, id: &str) -> Option<(Arc<Mutex<Ring>>, u64)> {
        hold(&self.inner).touch(id)
    }

    /// The next tail epoch for this owner.
    fn next_epoch(&self) -> u64 {
        self.next_epoch
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }
}

#[cfg(test)]
mod tests;
