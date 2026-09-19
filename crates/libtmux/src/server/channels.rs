use std::ffi::OsString;
use std::time::Duration;

use super::{ChannelWait, Server};
use crate::internal::listing;
use crate::{Command, Error};

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
        listing::mutate(
            &self.core,
            "wait-for",
            Command::new("wait-for")
                .arg("-S")
                .arg("--")
                .arg(OsString::from(channel)),
        )
        .await
    }

    /// Hold a `wait-for` channel for the length of an operation.
    ///
    /// [`Self::lock_channel`] and [`Self::unlock_channel`] as a pair, so a
    /// locker that returns early, fails, or panics still releases the
    /// channel: a lock left held wedges every later locker on the server.
    ///
    /// This does not cover the other way a channel wedges. Dropping a
    /// *pending* lock while it is queued behind another locker leaves tmux
    /// with a queue entry it will hand the lock to and nobody to take it;
    /// that is a tmux defect (`cmd-wait-for.c`) and no scope can reach it.
    /// See [`Self::lock_channel`].
    ///
    /// # Errors
    ///
    /// [`crate::ScopeError::Creation`] when tmux refuses the lock,
    /// `Operation` when the body fails, and `Cleanup` when the unlock fails
    /// after the body succeeded.
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
    /// usually what a caller wants.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the channel name.
    ///
    /// # Cancel safety
    ///
    /// Not cancel safe: dropping this future can leave something held, and
    /// no one can release it. While the call is queued behind another locker,
    /// tmux holds a queue entry for it. If that locker's process ends without
    /// calling [`Self::unlock_channel`], `cmd_wait_for_unlock` hands the
    /// released lock to the next queued entry with no way to skip one whose
    /// client has gone, and every later call here for the same channel blocks
    /// forever. That is a tmux defect (`cmd-wait-for.c`), measured on 3.2a,
    /// 3.7c and master, and no scope in this crate reaches it. The
    /// non-locking [`Self::wait_for_channel`] is safe to drop: it is a latch
    /// check, not a queue.
    pub async fn lock_channel(&self, channel: &str) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "wait-for",
            Command::new("wait-for")
                .arg("-L")
                .arg("--")
                .arg(OsString::from(channel)),
        )
        .await
    }

    /// Unlock a `wait-for` channel.
    ///
    /// Always call this from whatever locked with [`Self::lock_channel`],
    /// including on an error path: a locker that ends without unlocking can
    /// wedge the channel for everyone else. See that method's hazard note.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the channel name.
    pub async fn unlock_channel(&self, channel: &str) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "wait-for",
            Command::new("wait-for")
                .arg("-U")
                .arg("--")
                .arg(OsString::from(channel)),
        )
        .await
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
    /// Measured directly on 3.5a and 3.7c.
    ///
    /// `within` is capped at [`Server::default_timeout`], because a dispatch
    /// is bounded and this is one: ask for longer by building the server with
    /// a longer timeout.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the channel name or cannot be
    /// reached. Running out of time is [`ChannelWait::TimedOut`] rather than
    /// an error, so "nothing signalled it" stays distinct from "the command
    /// did not get through" -- the caller retries only one of those.
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
        let budget = within.min(self.default_timeout());
        let waited = tokio::time::timeout(
            budget,
            listing::mutate(
                &self.core,
                "wait-for",
                Command::new("wait-for")
                    .arg("--")
                    .arg(OsString::from(channel)),
            ),
        )
        .await;

        match waited {
            Ok(Ok(())) => Ok(ChannelWait::Signalled),
            // The dispatch reaching its own bound first is the same event, so
            // it is reported the same way rather than as two outcomes a
            // caller would have to unify.
            Ok(Err(error)) if error.kind() == crate::ErrorKind::Timeout => {
                Ok(ChannelWait::TimedOut)
            }
            Ok(Err(error)) => Err(error),
            // Dropping the dispatch kills the waiting tmux client; the
            // channel stays usable, measured by killing a waiter and
            // signalling it after. True only for this idempotent-latch
            // form -- `Self::lock_channel`'s queue has no such guarantee.
            Err(_elapsed) => Ok(ChannelWait::TimedOut),
        }
    }
}
