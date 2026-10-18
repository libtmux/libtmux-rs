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
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::internal::core::Core;
use crate::internal::listing;
use crate::{ChannelWait, Command, CommandResult, Error};

#[derive(Default)]
pub(crate) struct ChannelWaits {
    channels: Mutex<HashMap<String, Channel>>,
    changed: Condvar,
}
#[derive(Default)]
struct Channel {
    kept: bool,
    waiting: usize,
    resident: bool,
    failure: Option<Error>,
    releases: u64,
}
impl Channel {
    fn is_idle(&self) -> bool {
        !self.kept && !self.resident && self.waiting == 0 && self.failure.is_none()
    }
}
impl ChannelWaits {
    fn channels(&self) -> MutexGuard<'_, HashMap<String, Channel>> {
        self.channels.lock().unwrap_or_else(PoisonError::into_inner)
    }
    fn join(&self, channel: &str) -> Join<'_> {
        let mut channels = self.channels();
        let state = channels.entry(channel.to_owned()).or_default();
        if state.kept {
            state.kept = false;
            if state.is_idle() {
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
            seen: state.releases,
            opening,
            settled: false,
        })
    }
    fn finish(&self, channel: &str, outcome: Result<(), Error>) {
        let mut channels = self.channels();
        let Some(state) = channels.get_mut(channel) else {
            return;
        };
        state.resident = false;
        match outcome {
            Ok(()) => {
                state.releases += 1;
                state.kept = state.waiting == 0;
            }
            Err(error) => {
                state.failure = Some(error);
            }
        }
        if state.is_idle() {
            channels.remove(channel);
        }
        self.changed.notify_all();
    }
}
enum Join<'a> {
    Kept,
    Waiting(Waiter<'a>),
}
struct Waiter<'a> {
    waits: &'a ChannelWaits,
    channel: String,
    seen: u64,
    opening: bool,
    settled: bool,
}
enum Settled {
    Signalled,
    Failed(Error),
    Gone,
    TimedOut,
}
impl Waiter<'_> {
    fn wait(&self, core: &Arc<Core>, deadline: Option<Instant>) {
        let waking = Arc::clone(core);
        let _registration = core.cancellation_token().on_cancel(move || {
            let waits = waking.channel_waits();
            let _guard = waits.channels();
            waits.changed.notify_all();
        });
        let mut channels = self.waits.channels();
        loop {
            if core.cancellation_token().is_cancelled() || out_of_time(deadline) {
                break;
            }
            if channels
                .get(&self.channel)
                .is_none_or(|state| state.releases > self.seen || !state.resident)
            {
                break;
            }
            channels = match deadline {
                Some(deadline) => {
                    self.waits
                        .changed
                        .wait_timeout(channels, deadline.saturating_duration_since(Instant::now()))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self
                    .waits
                    .changed
                    .wait(channels)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
    }
    fn settle(&mut self) -> Settled {
        self.settled = true;
        let mut channels = self.waits.channels();
        let Some(state) = channels.get_mut(&self.channel) else {
            return Settled::TimedOut;
        };
        state.waiting -= 1;
        let outcome = if state.releases > self.seen {
            Settled::Signalled
        } else if state.resident {
            Settled::TimedOut
        } else {
            state.failure.take().map_or(Settled::Gone, Settled::Failed)
        };
        if state.is_idle() {
            channels.remove(&self.channel);
        }
        outcome
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
        state.kept = state.releases > self.seen && state.waiting == 0;
        if state.is_idle() {
            channels.remove(&self.channel);
        }
    }
}

pub(crate) fn wait(
    core: &Arc<Core>,
    channel: &str,
    budget: Duration,
) -> Result<ChannelWait, Error> {
    #[cfg(feature = "control-mode")]
    refuse_if_routed(core)?;
    let deadline = Instant::now().checked_add(budget);
    loop {
        if core.cancellation_token().is_cancelled() {
            return Err(core.cancelled_error(&channel_command(None, channel)));
        }
        let mut waiter = match core.channel_waits().join(channel) {
            Join::Kept => return Ok(ChannelWait::Signalled),
            Join::Waiting(waiter) => waiter,
        };
        if waiter.opening {
            park(core, channel);
        }
        waiter.wait(core, deadline);
        if core.cancellation_token().is_cancelled() {
            // Drop returns a release racing cancellation to the kept-signal ledger.
            return Err(core.cancelled_error(&channel_command(None, channel)));
        }
        match waiter.settle() {
            Settled::Signalled => return Ok(ChannelWait::Signalled),
            Settled::Failed(error) => return Err(error),
            Settled::Gone if !out_of_time(deadline) => {}
            Settled::Gone | Settled::TimedOut => return Ok(ChannelWait::TimedOut),
        }
    }
}
fn park(core: &Arc<Core>, channel: &str) {
    let worker = Arc::clone(core);
    let name = channel.to_owned();
    if let Err(error) = spawn(move || {
        let (_, dispatch) = worker.dispatch_without_deadline(channel_command(None, &name));
        worker
            .channel_waits()
            .finish(&name, mutated(dispatch.run()));
    }) {
        core.channel_waits().finish(channel, Err(error));
    }
}

#[derive(Default)]
struct GrantState {
    outcome: Option<Result<(), Error>>,
    abandoned: bool,
}
struct Grant {
    state: Arc<(Mutex<GrantState>, Condvar)>,
    core: Arc<Core>,
    channel: String,
    settled: bool,
}
impl Drop for Grant {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let outcome = {
            let mut state = self.state.0.lock().unwrap_or_else(PoisonError::into_inner);
            state.abandoned = true;
            state.outcome.take()
        };
        if matches!(outcome, Some(Ok(()))) {
            let _ = unlock(&self.core.for_cleanup(), &self.channel);
        }
    }
}
pub(crate) fn lock(core: &Arc<Core>, channel: &str, budget: Duration) -> Result<(), Error> {
    #[cfg(feature = "control-mode")]
    refuse_if_routed(core)?;
    let command = channel_command(Some("-L"), channel);
    if core.cancellation_token().is_cancelled() {
        return Err(core.cancelled_error(&command));
    }
    let summary = command.summary();
    let (request_id, dispatch) = core.dispatch_without_deadline(command);
    let state = Arc::new((Mutex::new(GrantState::default()), Condvar::new()));
    let worker_state = Arc::clone(&state);
    let holder = core.for_cleanup();
    let name = channel.to_owned();
    spawn(move || {
        let outcome = mutated(dispatch.run());
        let mut state = worker_state
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if state.abandoned {
            drop(state);
            if outcome.is_ok() {
                let _ = unlock(&holder, &name);
            }
        } else {
            state.outcome = Some(outcome);
            worker_state.1.notify_all();
        }
    })?;
    let mut grant = Grant {
        state: Arc::clone(&state),
        core: Arc::clone(core),
        channel: channel.to_owned(),
        settled: false,
    };
    let notify = Arc::clone(&state);
    let _registration = core.cancellation_token().on_cancel(move || {
        let _guard = notify.0.lock().unwrap_or_else(PoisonError::into_inner);
        notify.1.notify_all();
    });
    let deadline = Instant::now().checked_add(budget);
    let mut state = state.0.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        if core.cancellation_token().is_cancelled() {
            drop(state);
            return Err(Error::cancelled(request_id.get(), summary));
        }
        if let Some(outcome) = state.outcome.take() {
            grant.settled = true;
            return outcome;
        }
        if out_of_time(deadline) {
            state.abandoned = true;
            grant.settled = true;
            return Err(Error::timeout(request_id.get(), summary, budget));
        }
        state = match deadline {
            Some(deadline) => {
                grant
                    .state
                    .1
                    .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => grant
                .state
                .1
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner),
        };
    }
}

pub(crate) fn unlock(core: &Arc<Core>, channel: &str) -> Result<(), Error> {
    listing::mutate(core, "wait-for", channel_command(Some("-U"), channel))
}
pub(crate) fn signal(core: &Arc<Core>, channel: &str) -> Result<(), Error> {
    listing::mutate(core, "wait-for", channel_command(Some("-S"), channel))
}
#[cfg(feature = "control-mode")]
fn refuse_if_routed(core: &Arc<Core>) -> Result<(), Error> {
    if core.routes_over_control_mode() {
        return Err(Error::control_mode_blocking());
    }
    Ok(())
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
fn out_of_time(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}
fn spawn(task: impl FnOnce() + Send + 'static) -> Result<(), Error> {
    #[cfg(feature = "tracing")]
    let task = {
        let span = tracing::Span::current();
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        move || {
            tracing::dispatcher::with_default(&dispatcher, || {
                let _entered = span.enter();
                task();
            });
        }
    };
    std::thread::Builder::new()
        .name("libtmux-channel".to_owned())
        .spawn(task)
        .map(drop)
        .map_err(|source| Error::RuntimeUnavailable { source })
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
