//! Waiting on and locking a `wait-for` channel, and the state that takes.
//!
//! tmux queues a `wait-for` client on the channel and has no way to withdraw
//! it: `cmd_wait_for_signal` releases every waiter it finds and keeps the
//! signal only when it finds none, `cmd_wait_for_unlock` hands the lock to the
//! first queued locker, and `server_client_lost` removes neither. A client
//! killed for running out of time therefore stays in tmux's list, where it
//! eats the channel's next signal or takes its lock and never gives it back.
//!
//! So a client is never killed for running out of time. It is parked instead:
//! the caller stops waiting, the client stays until tmux releases it, and what
//! tmux released it with is accounted for here -- a signal with no caller left
//! to hear it is kept for the next wait, and a lock with no caller left to
//! hold it is released at once.

use std::collections::HashMap;
use std::ffi::OsString;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{oneshot, watch};
use tokio::time::Instant;

#[cfg(feature = "tracing")]
use tracing::instrument::{Instrument as _, WithSubscriber as _};

use crate::internal::core::Core;
use crate::internal::listing;
use crate::{ChannelWait, Command, CommandResult, Error};

/// The `wait-for` channels this process has a client on, or a signal for.
#[derive(Default)]
pub(crate) struct ChannelWaits {
    channels: Mutex<HashMap<String, Channel>>,
}

struct Channel {
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

impl Default for Channel {
    fn default() -> Self {
        Self {
            kept: false,
            waiting: 0,
            resident: false,
            failure: None,
            releases: watch::channel(0).0,
        }
    }
}

impl Channel {
    /// Whether nothing about this channel is left to remember.
    fn is_idle(&self) -> bool {
        !self.kept && !self.resident && self.waiting == 0 && self.failure.is_none()
    }
}

impl ChannelWaits {
    fn channels(&self) -> MutexGuard<'_, HashMap<String, Channel>> {
        self.channels.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Take a kept signal, or join the queue behind the channel's client.
    fn join(&self, channel: &str) -> Join<'_> {
        let mut channels = self.channels();
        let state = channels.entry(channel.to_owned()).or_default();
        if state.kept {
            state.kept = false;
            let idle = state.is_idle();
            if idle {
                channels.remove(channel);
            }
            return Join::Kept;
        }

        state.waiting += 1;
        let opening = !state.resident;
        if opening {
            state.resident = true;
            state.failure = None;
        }
        Join::Waiting(Waiter {
            waits: self,
            channel: channel.to_owned(),
            seen: *state.releases.borrow(),
            released: state.releases.subscribe(),
            opening,
            settled: false,
        })
    }

    /// Record how the channel's client ended.
    fn finish(&self, channel: &str, outcome: Result<(), Error>) {
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
                state.failure = Some(error);
                state.releases.send_modify(|_| ());
            }
        }
        if state.is_idle() {
            channels.remove(channel);
        }
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
    channel: String,
    seen: u64,
    released: watch::Receiver<u64>,
    /// Whether this caller is the one that opens the channel's client.
    opening: bool,
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
            return Settled::TimedOut;
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
    loop {
        let mut waiter = match core.channel_waits().join(channel) {
            Join::Kept => return Ok(ChannelWait::Signalled),
            Join::Waiting(waiter) => waiter,
        };
        if waiter.opening {
            park(core, channel);
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
fn park(core: &Arc<Core>, channel: &str) {
    let core = Arc::clone(core);
    let channel = channel.to_owned();
    spawn(async move {
        let (_, dispatch) = core.dispatch_without_deadline(channel_command(None, &channel));
        let outcome = mutated(dispatch.await);
        core.channel_waits().finish(&channel, outcome);
    });
}

/// Lock `channel`, giving up after `budget` without leaving it wedged.
///
/// tmux grants a released lock to the first client queued for it, dead or
/// alive, so the client stays queued after the caller gives up and unlocks as
/// soon as it is granted.
pub(crate) async fn lock(core: &Arc<Core>, channel: &str, budget: Duration) -> Result<(), Error> {
    let command = channel_command(Some("-L"), channel);
    let summary = command.summary();
    let (request_id, dispatch) = core.dispatch_without_deadline(command);

    let (granted, taken) = oneshot::channel();
    let holder = Arc::clone(core);
    let name = channel.to_owned();
    spawn(async move {
        let outcome = mutated(dispatch.await);
        if let Err(outcome) = granted.send(outcome) {
            // The caller is gone, and tmux grants this client the lock all the
            // same: a lock nobody holds wedges every later locker.
            if outcome.is_ok() {
                let _ = unlock(&holder, &name).await;
            }
        }
    });

    let deadline = Instant::now().checked_add(budget);
    tokio::select! {
        outcome = taken => match outcome {
            Ok(outcome) => outcome,
            Err(_) => Err(Error::supervisor_lost(request_id.get(), summary)),
        },
        () = elapsed(deadline) => Err(Error::timeout(request_id.get(), summary, budget)),
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
    use super::{ChannelWaits, Join, Settled};

    /// What a release does depends on who is left to hear it, and the three
    /// answers are what keeps a signal from being lost or heard twice.
    #[test]
    fn a_release_is_kept_only_when_no_caller_is_left_to_hear_it() {
        let waits = ChannelWaits::default();

        // The caller gave up and left the client parked: the release is kept,
        // and the next caller takes it once.
        let Join::Waiting(gave_up) = waits.join("build") else {
            panic!("the channel starts with nothing kept");
        };
        drop(gave_up);
        waits.finish("build", Ok(()));
        assert!(matches!(waits.join("build"), Join::Kept));

        // A caller is waiting: the release is theirs, and nothing is kept.
        let Join::Waiting(mut waiting) = waits.join("build") else {
            panic!("a kept release is one-shot");
        };
        waits.finish("build", Ok(()));
        assert!(matches!(waiting.settle(), Settled::Signalled));
        assert!(matches!(waits.join("build"), Join::Waiting(_)));

        // Released, then dropped before settling: the release is nobody's
        // again rather than lost.
        let Join::Waiting(raced) = waits.join("build") else {
            panic!("the release went to the caller that was waiting");
        };
        waits.finish("build", Ok(()));
        drop(raced);
        assert!(matches!(waits.join("build"), Join::Kept));
    }
}
