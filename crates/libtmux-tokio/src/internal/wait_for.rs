//! Waiting on and locking a `wait-for` channel, and the state that takes.
//!
//! On affected older tmux releases, `server_client_lost` leaves waiters and
//! lockers queued. A disconnected client can consume a later signal or lock
//! without running again. tmux 3.8 removes these entries on disconnect.
//!
//! This module parks a client instead of killing it when its caller's budget
//! ends. The caller stops waiting, the client stays until tmux releases it,
//! and what tmux released it with is accounted for here -- a signal with no
//! caller left to hear it is kept for the next wait, and a lock with no caller
//! left to hold it is released at once.

use std::collections::HashMap;
use std::ffi::OsString;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot, watch};
use tokio::time::Instant;

#[cfg(feature = "tracing")]
use tracing::instrument::{Instrument as _, WithSubscriber as _};

use crate::internal::core::Core;
use crate::internal::listing;
use crate::{ChannelWait, Command, CommandResult, Error};

/// The `wait-for` channels this process has a client on, or a signal for.
pub(crate) struct ChannelWaits {
    channels: Mutex<HashMap<ChannelKey, Channel>>,
    permits: Arc<Semaphore>,
    limit: usize,
    observations: Mutex<Observations>,
    generation: watch::Sender<Option<crate::ServerGeneration>>,
}

#[derive(Default)]
struct Observations {
    next: u64,
    reported: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct GenerationObservation {
    ticket: u64,
    previous: Option<crate::ServerGeneration>,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct ChannelKey {
    generation: crate::ServerGeneration,
    name: String,
}

struct Channel {
    reservation: Arc<OwnedSemaphorePermit>,
    /// A signal that released a parked client with no caller left to hear it.
    ///
    /// tmux keeps a signal nobody is waiting on, and cannot see that a parked
    /// client is nobody, so the next wait reads the signal from here.
    kept: bool,
    /// Callers waiting on this channel right now.
    waiting: usize,
    /// Whether a client of this process is on the channel.
    resident: bool,
    /// Why the channel's client ended, for the first caller that asks.
    failure: Option<Error>,
    /// Releases seen on this channel, which is how a caller that ran out of
    /// time at the same moment still reads the signal it was released by.
    releases: watch::Sender<u64>,
}

impl Channel {
    fn new(reservation: Arc<OwnedSemaphorePermit>) -> Self {
        Self {
            reservation,
            kept: false,
            waiting: 0,
            resident: false,
            failure: None,
            releases: watch::channel(0).0,
        }
    }

    /// Whether nothing about this channel is left to remember.
    fn is_idle(&self) -> bool {
        !self.kept && !self.resident && self.waiting == 0 && self.failure.is_none()
    }
}

impl ChannelWaits {
    pub(crate) fn new(limit: usize) -> Self {
        let limit = limit.clamp(1, Semaphore::MAX_PERMITS);
        Self {
            channels: Mutex::default(),
            permits: Arc::new(Semaphore::new(limit)),
            limit,
            observations: Mutex::default(),
            generation: watch::channel(None).0,
        }
    }

    pub(crate) fn begin_observation(&self) -> GenerationObservation {
        let mut observations = self
            .observations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        observations.next += 1;
        GenerationObservation {
            ticket: observations.next,
            previous: *self.generation.borrow(),
        }
    }

    pub(crate) fn observed_generation(
        &self,
        observation: GenerationObservation,
        generation: crate::ServerGeneration,
    ) -> Result<(), Error> {
        let mut channels = self.channels();
        let mut observations = self
            .observations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let current = *self.generation.borrow();
        let superseded =
            observation.ticket < observations.reported || current != observation.previous;
        observations.reported = observations.reported.max(observation.ticket);
        if superseded {
            if let Some(found) = current.filter(|found| *found != generation) {
                return Err(Error::ServerGenerationChanged {
                    expected: generation,
                    found,
                });
            }
        }
        channels.retain(|key, _| key.generation == generation);
        self.generation.send_replace(Some(generation));
        Ok(())
    }

    fn replaced(&self, expected: crate::ServerGeneration) -> Option<Error> {
        self.generation.borrow().and_then(|found| {
            (found != expected).then_some(Error::ServerGenerationChanged { expected, found })
        })
    }

    async fn until_replaced(
        &self,
        expected: crate::ServerGeneration,
        mut dispatch: crate::internal::executor::DispatchFuture,
    ) -> Result<CommandResult, Error> {
        let mut changed = self.generation.subscribe();
        loop {
            let _ = changed.borrow_and_update();
            if let Some(error) = self.replaced(expected) {
                return Err(error);
            }
            tokio::select! {
                result = &mut dispatch => return result,
                _ = changed.changed() => {}
            }
        }
    }

    pub(crate) fn reserve(&self) -> Result<Arc<OwnedSemaphorePermit>, Error> {
        Arc::clone(&self.permits)
            .try_acquire_owned()
            .map(Arc::new)
            .map_err(|_| Error::ParkedLimitExceeded { limit: self.limit })
    }

    fn channels(&self) -> MutexGuard<'_, HashMap<ChannelKey, Channel>> {
        self.channels.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Take a kept signal, or join the queue behind the channel's client.
    fn join(&self, channel: &ChannelKey) -> Result<Join<'_>, Error> {
        let mut channels = self.channels();
        if let Some(error) = self.replaced(channel.generation) {
            return Err(error);
        }
        let state = match channels.entry(channel.to_owned()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Channel::new(self.reserve()?))
            }
        };
        if state.kept {
            state.kept = false;
            let idle = state.is_idle();
            if idle {
                channels.remove(channel);
            }
            return Ok(Join::Kept);
        }

        let opening = !state.resident;
        if opening {
            // Only the channel owns a unique reservation after child cleanup.
            if Arc::strong_count(&state.reservation) != 1 {
                state.reservation = self.reserve()?;
            }
            state.resident = true;
            state.failure = None;
        }
        state.waiting += 1;
        Ok(Join::Waiting(Waiter {
            waits: self,
            channel: channel.to_owned(),
            seen: *state.releases.borrow(),
            released: state.releases.subscribe(),
            opening: opening.then(|| Arc::clone(&state.reservation)),
            settled: false,
        }))
    }

    /// Record how the channel's client ended.
    fn finish(&self, channel: &ChannelKey, outcome: Result<(), Error>) {
        let mut channels = self.channels();
        let Some(state) = channels.get_mut(channel) else {
            return;
        };
        state.resident = false;
        match outcome {
            Ok(()) => {
                state.releases.send_modify(|count| *count += 1);
                state.kept = state.waiting == 0;
            }
            Err(error) => {
                if state.waiting == 0 {
                    channels.remove(channel);
                    return;
                }
                state.failure = Some(error);
                state.releases.send_modify(|_| ());
            }
        }
        if state.is_idle() {
            channels.remove(channel);
        }
    }
}

impl Default for ChannelWaits {
    fn default() -> Self {
        Self::new(crate::DispatchLimits::DEFAULT_PARKED)
    }
}

enum Join<'a> {
    /// A signal arrived while this process had nobody waiting.
    Kept,
    /// This caller is queued behind the channel's client.
    Waiting(Waiter<'a>),
}

/// One caller's place on a channel, given up however the caller leaves.
struct Waiter<'a> {
    waits: &'a ChannelWaits,
    channel: ChannelKey,
    seen: u64,
    released: watch::Receiver<u64>,
    /// Whether this caller is the one that opens the channel's client.
    opening: Option<Arc<OwnedSemaphorePermit>>,
    settled: bool,
}

/// What a caller leaves a channel with.
enum Settled {
    Signalled,
    /// The client ended without a signal, and this caller takes the reason.
    Failed(Error),
    /// The client ended without a signal, and another caller took the reason.
    Gone,
    TimedOut,
}

impl Waiter<'_> {
    /// Leave the channel, saying what this caller takes with it.
    fn settle(&mut self) -> Settled {
        self.settled = true;
        let mut channels = self.waits.channels();
        let Some(state) = channels.get_mut(&self.channel) else {
            return self
                .waits
                .replaced(self.channel.generation)
                .map_or(Settled::TimedOut, Settled::Failed);
        };
        state.waiting -= 1;

        let settled = if *state.releases.borrow() > self.seen {
            Settled::Signalled
        } else if state.resident {
            Settled::TimedOut
        } else {
            state.failure.take().map_or(Settled::Gone, Settled::Failed)
        };
        if state.is_idle() {
            channels.remove(&self.channel);
        }
        settled
    }
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let mut channels = self.waits.channels();
        let Some(state) = channels.get_mut(&self.channel) else {
            return;
        };
        state.waiting -= 1;
        // A signal this caller was released by and then dropped is nobody's,
        // so it goes back to being kept for the next wait.
        state.kept = *state.releases.borrow() > self.seen && state.waiting == 0;
        if state.waiting == 0 && !state.resident && !state.kept {
            state.failure = None;
        }
        if state.is_idle() {
            channels.remove(&self.channel);
        }
    }
}

/// Wait for `channel` to be signalled, for at most `budget`.
///
/// The client outlives the wait: a caller that runs out of time leaves it
/// parked, a later wait on the same channel joins it rather than opening a
/// second, and a signal that releases it with nobody waiting is kept.
pub(crate) async fn wait(
    core: &Arc<Core>,
    channel: &str,
    budget: Duration,
) -> Result<ChannelWait, Error> {
    let deadline = Instant::now().checked_add(budget);
    let key = ChannelKey {
        generation: core
            .expected_generation()
            .ok_or(libtmux_core::Error::InvalidKey)?,
        name: channel.to_owned(),
    };
    loop {
        let mut waiter = match core.channel_waits().join(&key)? {
            Join::Kept => return Ok(ChannelWait::Signalled),
            Join::Waiting(waiter) => waiter,
        };
        if let Some(reservation) = waiter.opening.take() {
            park(core, key.clone(), reservation);
        }

        tokio::select! {
            result = waiter.released.changed() => {
                let _ = result;
            }
            () = elapsed(deadline) => {}
        }

        match waiter.settle() {
            Settled::Signalled => return Ok(ChannelWait::Signalled),
            Settled::Failed(error) => return Err(error),
            // The client ended with neither a signal nor a reason left to
            // report, so this caller opens one of its own with the time it
            // has left.
            Settled::Gone if !out_of_time(deadline) => {}
            Settled::Gone | Settled::TimedOut => return Ok(ChannelWait::TimedOut),
        }
    }
}

/// Open the channel's client, which stays until tmux releases it.
fn park(core: &Arc<Core>, channel: ChannelKey, reservation: Arc<OwnedSemaphorePermit>) {
    let core = Arc::clone(core);
    spawn(async move {
        let (_, dispatch) =
            core.dispatch_without_deadline(channel_command(None, &channel.name), reservation);
        let outcome = mutated(
            core.channel_waits()
                .until_replaced(channel.generation, dispatch)
                .await,
        );
        core.channel_waits().finish(&channel, outcome);
    });
}

/// Lock `channel`, giving up after `budget` without leaving it wedged.
///
/// On affected older tmux releases, a disconnected locker can retain its
/// place. This call keeps the client queued when the caller gives up and
/// unlocks as soon as the lock is granted.
pub(crate) async fn lock(core: &Arc<Core>, channel: &str, budget: Duration) -> Result<(), Error> {
    let command = channel_command(Some("-L"), channel);
    let summary = command.summary();
    let reservation = core.channel_waits().reserve()?;
    let generation = core
        .expected_generation()
        .ok_or(libtmux_core::Error::InvalidKey)?;
    let (request_id, dispatch) = core.dispatch_without_deadline(command, reservation);

    let (granted, taken) = oneshot::channel();
    let holder = Arc::clone(core);
    let name = channel.to_owned();
    spawn(async move {
        let outcome = mutated(
            holder
                .channel_waits()
                .until_replaced(generation, dispatch)
                .await,
        );
        if let Err(outcome) = granted.send(outcome) {
            // The caller is gone, and tmux grants this client the lock all the
            // same: a lock nobody holds wedges every later locker.
            if outcome.is_ok() {
                let _ = unlock(&holder, &name).await;
            }
        }
    });

    let mut grant = Grant {
        taken,
        core: Arc::clone(core),
        channel: channel.to_owned(),
        settled: false,
    };
    let deadline = Instant::now().checked_add(budget);
    tokio::select! {
        outcome = &mut grant.taken => match outcome {
            Ok(outcome) => outcome,
            Err(_) => Err(Error::supervisor_lost(request_id.get(), summary)),
        },
        () = elapsed(deadline) => grant
            .close()
            .unwrap_or_else(|| Err(Error::timeout(request_id.get(), summary, budget))),
    }
}

/// The caller's end of a queued lock, which gives back a grant it cannot take.
///
/// A grant that crosses the channel as the caller stops waiting would
/// otherwise be dropped with the receiver, leaving the lock held by nobody:
/// closing the receiver first makes the task's send fail, and anything that
/// arrived before the close is read back here.
struct Grant {
    taken: oneshot::Receiver<Result<(), Error>>,
    core: Arc<Core>,
    channel: String,
    settled: bool,
}

impl Grant {
    /// Stop the lock task sending, and take what it already sent.
    fn close(&mut self) -> Option<Result<(), Error>> {
        self.settled = true;
        self.taken.close();
        self.taken.try_recv().ok()
    }
}

impl Drop for Grant {
    fn drop(&mut self) {
        if self.settled || !matches!(self.close(), Some(Ok(()))) {
            return;
        }
        // The caller was dropped holding a lock it never saw. Without a
        // runtime there is nothing left to release it with, and the client
        // holding it is going with the process.
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let core = Arc::clone(&self.core);
        let channel = std::mem::take(&mut self.channel);
        spawn(async move {
            let _ = unlock(&core, &channel).await;
        });
    }
}

/// Release `channel`, whoever holds it.
pub(crate) async fn unlock(core: &Arc<Core>, channel: &str) -> Result<(), Error> {
    listing::mutate(core, "wait-for", channel_command(Some("-U"), channel)).await
}

/// Signal `channel`, releasing everything waiting on it.
pub(crate) async fn signal(core: &Arc<Core>, channel: &str) -> Result<(), Error> {
    listing::mutate(core, "wait-for", channel_command(Some("-S"), channel)).await
}

fn channel_command(flag: Option<&'static str>, channel: &str) -> Command {
    let command = Command::new("wait-for");
    let command = match flag {
        Some(flag) => command.arg(flag),
        None => command,
    };
    command.arg("--").arg(OsString::from(channel))
}

fn mutated(result: Result<CommandResult, Error>) -> Result<(), Error> {
    let result = result?;
    if result.success() {
        Ok(())
    } else {
        Err(listing::mutation_failure("wait-for", &result, None))
    }
}

async fn elapsed(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn out_of_time(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

fn spawn(task: impl Future<Output = ()> + Send + 'static) {
    #[cfg(feature = "tracing")]
    tokio::spawn(task.in_current_span().with_current_subscriber());
    #[cfg(not(feature = "tracing"))]
    tokio::spawn(task);
}

#[cfg(test)]
mod tests {
    use super::{ChannelKey, ChannelWaits, Join, Settled};

    #[test]
    fn out_of_order_observations_cannot_retire_the_new_generations_signal() {
        for stale_registered_last in [false, true] {
            let waits = ChannelWaits::new(1);
            let original = crate::ServerGeneration::from_raw(12, 1);
            let replacement = crate::ServerGeneration::from_raw(13, 1);
            waits
                .observed_generation(waits.begin_observation(), original)
                .expect("initial report");
            let first = waits.begin_observation();
            let second = waits.begin_observation();
            let (stale, fresh) = if stale_registered_last {
                (second, first)
            } else {
                (first, second)
            };
            waits
                .observed_generation(fresh, replacement)
                .expect("replacement report");
            let key = ChannelKey {
                generation: replacement,
                name: "ready".into(),
            };
            let Join::Waiting(waiter) = waits.join(&key).expect("replacement wait") else {
                panic!("no initial signal");
            };
            drop(waiter);
            waits.finish(&key, Ok(()));
            assert!(
                matches!(waits.observed_generation(stale, original), Err(crate::Error::ServerGenerationChanged { expected, found }) if expected == original && found == replacement)
            );
            assert!(
                matches!(waits.join(&key), Ok(Join::Kept)),
                "a delayed old report cannot discard the replacement's signal"
            );
            assert!(
                waits.reserve().is_ok(),
                "consuming the current signal frees capacity"
            );
        }
    }

    #[test]
    fn orphaned_failures_return_map_capacity_after_child_ownership_ends() {
        let waits = ChannelWaits::new(1);
        for index in 0..8 {
            let key = ChannelKey {
                generation: crate::ServerGeneration::from_raw(12, 1),
                name: format!("failed-{index}"),
            };
            let Join::Waiting(mut waiter) = waits.join(&key).expect("channel admitted") else {
                panic!("new channel cannot have a kept signal");
            };
            let child = waiter.opening.take().expect("child reservation");
            drop(waiter);
            waits.finish(&key, Err(crate::Error::invalid_command_input(1, "fixture")));
            assert!(
                waits.channels().is_empty(),
                "no unobserved error entry remains"
            );
            assert!(waits.reserve().is_err(), "cleanup still owns the child");
            drop(child);
            assert!(waits.reserve().is_ok(), "cleanup returns capacity");
        }
    }

    #[test]
    fn a_verified_replacement_retires_a_signal_without_delivering_it_again() {
        let waits = ChannelWaits::new(1);
        let original = crate::ServerGeneration::from_raw(12, 1);
        let replacement = crate::ServerGeneration::from_raw(13, 1);
        let key = ChannelKey {
            generation: original,
            name: "ready".into(),
        };
        waits
            .observed_generation(waits.begin_observation(), original)
            .expect("original report");
        let Join::Waiting(waiter) = waits.join(&key).expect("channel admitted") else {
            panic!("no initial signal");
        };
        drop(waiter);
        waits.finish(&key, Ok(()));
        assert!(waits.reserve().is_err(), "verified signal owns its place");
        waits
            .observed_generation(waits.begin_observation(), replacement)
            .expect("replacement report");
        assert!(
            matches!(waits.join(&key), Err(crate::Error::ServerGenerationChanged { expected, found }) if expected == original && found == replacement)
        );
        assert!(
            matches!(
                waits.join(&ChannelKey {
                    generation: replacement,
                    ..key
                }),
                Ok(Join::Waiting(_))
            ),
            "the replacement cannot consume the retired signal"
        );
    }

    /// What a release does depends on who is left to hear it, and the three
    /// answers are what keeps a signal from being lost or heard twice.
    #[test]
    fn a_release_is_kept_only_when_no_caller_is_left_to_hear_it() {
        let waits = ChannelWaits::default();
        let key = ChannelKey {
            generation: crate::ServerGeneration::from_raw(12, 1),
            name: "build".into(),
        };

        // The caller gave up and left the client parked: the release is kept,
        // and the next caller takes it once.
        let Join::Waiting(gave_up) = waits.join(&key).expect("channel admitted") else {
            panic!("the channel starts with nothing kept");
        };
        drop(gave_up);
        waits.finish(&key, Ok(()));
        let replacement = ChannelKey {
            generation: crate::ServerGeneration::from_raw(13, 1),
            ..key.clone()
        };
        assert!(matches!(
            waits.join(&replacement).expect("channel admitted"),
            Join::Waiting(_)
        ));
        assert!(matches!(
            waits.join(&key).expect("channel admitted"),
            Join::Kept
        ));

        // A caller is waiting: the release is theirs, and nothing is kept.
        let Join::Waiting(mut waiting) = waits.join(&key).expect("channel admitted") else {
            panic!("a kept release is one-shot");
        };
        waits.finish(&key, Ok(()));
        assert!(matches!(waiting.settle(), Settled::Signalled));
        assert!(matches!(
            waits.join(&key).expect("channel admitted"),
            Join::Waiting(_)
        ));

        // Released, then dropped before settling: the release is nobody's
        // again rather than lost.
        let Join::Waiting(raced) = waits.join(&key).expect("channel admitted") else {
            panic!("the release went to the caller that was waiting");
        };
        waits.finish(&key, Ok(()));
        drop(raced);
        assert!(matches!(
            waits.join(&key).expect("channel admitted"),
            Join::Kept
        ));
    }
}
