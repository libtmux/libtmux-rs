use std::time::Duration;

use super::{ChannelWait, Server};
use crate::Error;
use crate::internal::wait_for;

impl Server {
    /// Signal a `wait-for` channel, releasing anything waiting on it.
    ///
    /// [`Server::wait_for_channel`] is the other half, and either order works:
    /// tmux keeps a signal nobody is waiting on, so a command that finishes
    /// before its watcher starts does not lose the race. The latch releases
    /// one wait, and one signal releases every waiter already there.
    /// Signalling the same channel twice before a wait clears the latch, so
    /// this operation is not idempotent.
    ///
    /// Signalling is not scoped to a pane or a session. The channel is a name
    /// on the server, so anything that can reach the socket can signal it,
    /// which is what makes it useful for telling an orchestrator that a
    /// command inside a pane is done.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the channel name.
    pub async fn signal_channel(&self, channel: &str) -> Result<(), Error> {
        wait_for::signal(&self.core, channel).await
    }

    /// Hold a `wait-for` channel for the length of an operation.
    ///
    /// [`Self::lock_channel`] and [`Self::unlock_channel`] as a pair, so a
    /// locker that returns early, fails, or panics still releases the
    /// channel: a lock left held wedges every later locker on the server.
    ///
    /// # Errors
    ///
    /// [`crate::ScopeError::Creation`] when tmux refuses the lock or the lock
    /// runs out of time, `Operation` when the body fails, and `Cleanup` when
    /// the unlock fails after the body succeeded.
    ///
    /// # Cancel safety
    ///
    /// Nothing is left held by a drop. The lock is taken and released in tasks
    /// of their own, so a scope dropped while its lock is still queued unlocks
    /// once tmux grants it, as [`Self::lock_channel`] describes.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let server = guard.server();
    ///
    /// let count = server
    ///     .with_channel_lock("deploy", async |server| {
    ///         Ok::<_, libtmux::Error>(server.sessions().await?.len())
    ///     })
    ///     .await?;
    ///
    /// // The channel is free again, so the next locker is not blocked.
    /// server.lock_channel("deploy").await?;
    /// server.unlock_channel("deploy").await?;
    /// # let _ = count;
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn with_channel_lock<T, E>(
        &self,
        channel: &str,
        operation: impl AsyncFnOnce(&Self) -> Result<T, E>,
    ) -> Result<T, crate::ScopeError<T, E>> {
        let server = self.clone();
        let channel = channel.to_owned();
        let held = channel.clone();
        let unlocking = self.clone();

        crate::internal::scoped::run(
            "with-channel-lock",
            async move { server.lock_channel(&held).await.map(|()| server.clone()) },
            move |_| async move { unlocking.unlock_channel(&channel).await },
            async |_| operation(self).await,
        )
        .await
    }

    /// Lock a `wait-for` channel, blocking later lock attempts on it.
    ///
    /// [`Self::with_channel_lock`] pairs this with the unlock, which is
    /// usually what a caller wants. A lock another locker holds waits for its
    /// turn, up to [`Server::default_timeout`].
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the channel name, and
    /// [`crate::ErrorKind::Timeout`] when the channel is still held after
    /// [`Server::default_timeout`]. A handle from `Server::over_control_mode`
    /// is refused, for the reason [`Self::wait_for_channel`] gives.
    ///
    /// # Cancel safety
    ///
    /// Nothing is left held. The lock is taken in a task of its own, so a lock
    /// that is dropped -- or that runs out of time -- while queued behind
    /// another locker is not withdrawn from tmux, which cannot withdraw one:
    /// it is granted in turn and released at once. Between those two, later
    /// lockers wait as they would for any other holder.
    ///
    /// Shutting the server down while such a lock is queued is the exception,
    /// because it kills the client: tmux then grants the lock to a client that
    /// is gone and every later lock on the channel blocks forever. A tmux
    /// defect (`cmd_wait_for_unlock` in `cmd-wait-for.c`), measured on 3.2a,
    /// 3.7d and 3.8-rc.
    pub async fn lock_channel(&self, channel: &str) -> Result<(), Error> {
        wait_for::lock(&self.core, channel, self.default_timeout()).await
    }

    /// Unlock a `wait-for` channel.
    ///
    /// Always call this from whatever locked with [`Self::lock_channel`],
    /// including on an error path: a locker that ends without unlocking can
    /// wedge the channel for everyone else.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the channel name.
    pub async fn unlock_channel(&self, channel: &str) -> Result<(), Error> {
        wait_for::unlock(&self.core, channel).await
    }

    /// Wait for a `wait-for` channel to be signalled.
    ///
    /// The blocking half of [`Server::signal_channel`]. Nothing polls: tmux
    /// releases the wait when the channel is signalled, so a caller costs one
    /// idle client rather than a loop.
    ///
    /// This waits for something to *say* it happened. It does not watch a
    /// pane, so what signals the channel is the caller's to arrange -- a
    /// command ending with `tmux wait-for -S <channel>` is the usual shape.
    ///
    /// The channel latches. Signalling one nothing is waiting on is kept, and
    /// the next wait returns at once; the latch is one-shot, so a second wait
    /// blocks again. One signal releases every waiter present at the time. So
    /// signalling before the wait starts is safe, which is what makes this
    /// usable for a command that may finish first.
    ///
    /// That holds across the supported range. `cmd-wait-for.c` is identical
    /// between 3.5a and 3.7c, and the only changes since 3.2a are an argument
    /// table gaining a field, an accessor replacing a direct index, and a
    /// local being renamed -- none of them near the flag the latch is kept in.
    /// Measured directly on 3.2a, 3.5a, 3.7c and 3.7d.
    ///
    /// A wait that runs out of time leaves its client on the channel, because
    /// tmux cannot withdraw one and a killed client would eat the channel's
    /// next signal. Another wait on the same channel joins that client rather
    /// than opening a second, and a signal that releases it with nobody
    /// waiting is kept for the next wait, which is where the latch above
    /// survives a wait that gave up. The client is this process's, so it ends
    /// with [`Server::shutdown`]; it does not count against
    /// [`crate::DispatchLimits`], because signalling the channel is itself a
    /// dispatch.
    ///
    /// `within` is capped at [`Server::default_timeout`]: ask for longer by
    /// building the server with a longer timeout.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the channel name or cannot be
    /// reached. Running out of time is [`ChannelWait::TimedOut`] rather than
    /// an error, so "nothing signalled it" stays distinct from "the command
    /// did not get through" -- the caller retries only one of those.
    ///
    /// A handle from `Server::over_control_mode` is refused with
    /// [`crate::ControlModeErrorKind::BlockingCommand`]: a connection runs one
    /// command at a time, and tmux closes a blocking `wait-for` the moment it
    /// queues it, so the wait would neither wait nor let anything else
    /// through. `Server::signal_channel` routes as usual.
    ///
    /// # Cancel safety
    ///
    /// Nothing is lost by a drop, and nothing is left for the next caller to
    /// find: the client stays on the channel exactly as it does after
    /// [`ChannelWait::TimedOut`], and a signal that releases it is kept for
    /// the next wait on this server handle or any clone of it.
    ///
    /// A process outside this one is the exception. The signal that releases
    /// the parked client is spent in tmux, so a *different* process waiting on
    /// the same channel afterwards does not see it; tmux offers no way to take
    /// a waiter back out, and forging a replacement signal would release
    /// somebody else's wait.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::ChannelWait;
    /// use std::time::Duration;
    ///
    /// # let guard = libtmux::test::TestServer::builder().start().await?;
    /// # let server = guard.server();
    /// // Signalling first is safe: the channel keeps it.
    /// server.signal_channel("ready").await?;
    /// let outcome = server.wait_for_channel("ready", Duration::from_secs(5)).await?;
    /// assert_eq!(outcome, ChannelWait::Signalled);
    ///
    /// // The latch is spent, so a second wait runs out of time instead.
    /// let again = server.wait_for_channel("ready", Duration::from_millis(200)).await?;
    /// assert_eq!(again, ChannelWait::TimedOut);
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn wait_for_channel(
        &self,
        channel: &str,
        within: Duration,
    ) -> Result<ChannelWait, Error> {
        wait_for::wait(&self.core, channel, within.min(self.default_timeout())).await
    }
}
