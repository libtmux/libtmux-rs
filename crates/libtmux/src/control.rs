//! Watching a tmux server over control mode.
//!
//! Every other API in this crate spawns a tmux process per command. Control
//! mode opens one connection and keeps it: commands go down it, and tmux
//! reports what happens on the server as it happens. That is the difference
//! between asking tmux what is true and being told when it changes.
//!
//! Sending and watching are separate handles, so one thread can act on what
//! another sees:
//!
//! ```no_run
//! # fn watch(server: &libtmux::Server, id: &libtmux::SessionId) -> Result<(), libtmux::Error> {
//! use libtmux::control::{ControlMode, Event};
//!
//! let (commands, mut events) = ControlMode::attach(server, id)?.split();
//!
//! // Commands travel down the connection, so none of these spawn a process.
//! let listed = commands.send(libtmux::Command::new("list-windows"))?;
//! assert!(listed.succeeded());
//!
//! // The watcher reads, and only reads. A reply arrives on the connection
//! // the events arrive on, so a loop that stops reading in order to await one
//! // is waiting on the connection it stopped reading.
//! let watcher = std::thread::spawn(move || {
//!     while let Some(event) = events.next_event() {
//!         match event? {
//!             Event::Output { pane, bytes } => println!("{pane}: {} bytes", bytes.len()),
//!             Event::Exit { .. } => break,
//!             other => println!("{other:?}"),
//!         }
//!     }
//!
//!     // Explicit shutdown also closes a connection before stream exhaustion.
//!     events.shutdown()
//! });
//!
//! // Acting on what the watcher sees happens out here, on the other handle,
//! // which is what having two of them is for.
//! commands.send(libtmux::Command::new("list-panes"))?;
//!
//! match watcher.join() {
//!     Ok(result) => result?,
//!     Err(panic) => std::panic::resume_unwind(panic),
//! }
//! Ok(())
//! # }
//! ```
//!
//! `examples/watch.rs` is this as a program that runs, against a server it
//! starts and cleans up.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use std::thread::JoinHandle;

use crate::limits::ControlLimits;
use crate::version::since::CONTROL_PANE_OFF;
use crate::{Command, Error, IdParseError, PaneId, Server, SessionId, TmuxText, WindowId};

mod actor;
mod channel;
#[cfg(feature = "unstable-fuzzing")]
mod fuzz;
mod protocol;

#[cfg(test)]
use actor::{HELD_WHILE_AWAITING, ReplySlot, ReplySlots};
use channel::{Receiver, Sender, Submission};
#[cfg(any(test, feature = "unstable-fuzzing"))]
use protocol::Line;
#[cfg(test)]
use protocol::unescape_output;

pub use crate::protocol::Event;

/// The outcome of one command sent over control mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockResult {
    number: u64,
    succeeded: bool,
    output: Vec<TmuxText>,
    sensitive_input: bool,
    /// Which lines came from error blocks, in the same order as `output`.
    failed_lines: Vec<bool>,
}

impl BlockResult {
    /// Return the block number tmux assigned.
    ///
    /// This is the last command block in the reply. An empty alias has no
    /// command block, so its number belongs to the completion boundary.
    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }

    /// Report whether every command block succeeded.
    #[must_use]
    pub const fn succeeded(&self) -> bool {
        self.succeeded
    }

    /// Return all command output lines in their original arrival order.
    #[must_use]
    pub fn output(&self) -> &[TmuxText] {
        &self.output
    }

    /// Split output by each block's outcome, retaining order within each stream.
    pub(crate) fn split_by_outcome(
        &self,
    ) -> (
        impl Iterator<Item = &TmuxText>,
        impl Iterator<Item = &TmuxText>,
    ) {
        let lines = || self.output.iter().zip(&self.failed_lines);
        (
            lines().filter_map(|(line, failed)| (!failed).then_some(line)),
            lines().filter_map(|(line, failed)| failed.then_some(line)),
        )
    }

    /// Append another block without letting a success erase an earlier error.
    pub(super) fn followed_by(mut self, next: Self) -> Self {
        self.number = next.number;
        self.succeeded &= next.succeeded;
        self.sensitive_input |= next.sensitive_input;
        self.output.extend(next.output);
        self.failed_lines.extend(next.failed_lines);
        self
    }

    /// Classify an error block as a refusal for a named operation.
    ///
    /// Use a fixed operation name without targets or argument values. Output
    /// is withheld when the command carried sensitive input.
    ///
    /// Returns `None` when tmux closed the block successfully.
    #[must_use]
    pub fn refusal_for(&self, operation: &'static str) -> Option<Error> {
        if self.succeeded {
            return None;
        }

        let mut bytes = Vec::new();
        for line in &self.output {
            bytes.extend_from_slice(line.as_bytes());
            bytes.push(b'\n');
        }
        let classified = Error::refused(
            operation,
            None,
            String::from_utf8_lossy(&bytes).into_owned(),
            None,
        );
        Some(
            if self.sensitive_input && !matches!(&classified, Error::ServerGone { .. }) {
                Error::refused_withheld(operation, None)
            } else {
                classified
            },
        )
    }

    fn require_success(self, operation: &'static str) -> Result<Self, Error> {
        match self.refusal_for(operation) {
            Some(error) => Err(error),
            None => Ok(self),
        }
    }
}

/// One control-mode connection to a tmux server.
///
/// Sending and receiving are separate handles, reachable through [`split`].
/// A thread can send commands while another waits for notifications.
///
/// [`split`]: ControlMode::split
#[derive(Debug)]
pub struct ControlMode {
    sender: ControlSender,
    events: ControlEvents,
}

impl ControlMode {
    /// Attach to a session in control mode.
    ///
    /// When this returns, tmux has the client attached: anything that changes
    /// the server afterwards is reported. Returning as soon as the process
    /// started would look the same and lose every notification racing the
    /// attach, which is the hardest kind of bug to see.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started, does not give the crate
    /// the pipes it asked for, exits before attaching, or does not finish its
    /// opening block before the server deadline.
    ///
    /// # Cancellation
    ///
    /// Cancelling the server token kills and reaps the attaching client before
    /// this call returns. An established connection remains open.
    pub fn attach(server: &Server, session: &SessionId) -> Result<Self, Error> {
        Self::attach_with_limits(server, session, ControlLimits::default())
    }

    /// Attach with explicit frame budgets.
    ///
    /// Control mode reads from a process that keeps running, so the framing is
    /// the only thing bounding memory: a line that never ends, or a block
    /// whose `%end` never arrives, otherwise grows until the machine notices.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection cannot be opened, as
    /// [`Self::attach`] does. [`Server::shutdown`] cancels an attach in
    /// progress and refuses later attempts.
    ///
    /// # Cancellation
    ///
    /// As for [`Self::attach`], cancelling the server token cleans up the
    /// attaching client before returning.
    pub fn attach_with_limits(
        server: &Server,
        session: &SessionId,
        limits: ControlLimits,
    ) -> Result<Self, Error> {
        // Asked before the attach so a connection never carries an unknown
        // answer: a release that cannot be read is treated as too old, which
        // costs a pane's back-pressure rather than the server.
        let pane_off_is_safe = server
            .capabilities()
            .is_ok_and(|capabilities| capabilities.tmux_version().has_behavior(&CONTROL_PANE_OFF));

        let timeout = server.default_timeout();
        let actor::OpenedConnection { commands, events } = actor::open(
            server.spawn_control(session)?,
            limits,
            timeout,
            server.cancellation_token(),
        )?;

        let sender = ControlSender {
            commands,
            timeout,
            pane_off_is_safe,
            identity: server.identity().clone(),
            cancellation: server.cancellation_token().clone(),
            next_request_id: server.control_request_ids(),
        };
        let events = ControlEvents { events };

        // Without this, tmux 3.8+ hands a control client the classic
        // `window_layout` string instead of JSON, disagreeing with a plain
        // client's snapshot; a release below 3.8 ignores the unknown flag.
        if let Err(error) = sender
            .send(Command::new("refresh-client").arg("-f").arg("new-layouts"))
            .and_then(|reply| reply.require_success("refresh-client"))
        {
            drop(sender);
            return Err(events.shutdown_after_error(error));
        }

        Ok(Self { sender, events })
    }

    /// Separate the two halves so they can be used at the same time.
    #[must_use]
    pub fn split(self) -> (ControlSender, ControlEvents) {
        (self.sender, self.events)
    }

    /// Set how long a command waits for its result block.
    ///
    /// Retunes the sender this connection sends through, and leaves the
    /// opening handshake, which has already happened, alone. See
    /// [`ControlSender::reply_timeout`].
    #[must_use]
    pub fn reply_timeout(self, timeout: Duration) -> Self {
        Self {
            sender: self.sender.reply_timeout(timeout),
            ..self
        }
    }

    /// Send one command and wait for its result block.
    ///
    /// # Errors
    ///
    /// Returns an error when the command cannot be written as a control-mode
    /// line, the connection has closed, or its deadline elapses while queued,
    /// being written, or awaiting a response.
    ///
    /// # Cancellation
    ///
    /// As for [`ControlSender::send`]: a command cancelled while queued is never
    /// written, and one already committed may run.
    pub fn send(&self, command: Command) -> Result<BlockResult, Error> {
        self.sender.send(command)
    }

    /// Return the next notification or terminal error, then `None`.
    ///
    /// See [`ControlEvents::next_event`] for termination.
    ///
    /// # Blocking
    ///
    /// The server operation token does not interrupt this read.
    /// [`Server::shutdown`] closes the connection and wakes it.
    pub fn next_event(&mut self) -> Option<Result<Event, Error>> {
        self.events.next_event()
    }

    /// Close the connection and report how it ended.
    ///
    /// # Errors
    ///
    /// Returns a connection error that was not already delivered by
    /// [`Self::next_event`].
    pub fn shutdown(self) -> Result<(), Error> {
        drop(self.sender);
        self.events.shutdown()
    }
}

/// What a subscription watches.
///
/// tmux reads this from the shape of the argument rather than from a keyword:
/// `%` introduces a pane, `@` a window, `*` stands for every one of them, and
/// anything else names the session the control client is attached to. The
/// session case is therefore spelled as nothing at all, which is the canonical
/// form rather than a special case.
///
/// # Examples
///
/// ```
/// use libtmux::control::Subscription;
///
/// assert_eq!(Subscription::AllPanes.to_string(), "%*");
/// assert_eq!(Subscription::AllWindows.to_string(), "@*");
///
/// // The session the connection is attached to, named by naming nothing.
/// assert_eq!(Subscription::Session.to_string(), "");
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Subscription {
    /// The session this connection is attached to.
    Session,
    /// One window.
    Window(WindowId),
    /// Every window in the attached session.
    AllWindows,
    /// One pane.
    Pane(PaneId),
    /// Every pane in the attached session.
    AllPanes,
}

impl std::fmt::Display for Subscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Session => Ok(()),
            Self::Window(window) => write!(formatter, "{window}"),
            Self::AllWindows => formatter.write_str("@*"),
            Self::Pane(pane) => write!(formatter, "{pane}"),
            Self::AllPanes => formatter.write_str("%*"),
        }
    }
}

/// Refuse a name tmux would read as something other than a name.
///
/// tmux splits the argument on its first colon and treats a name with no colon
/// after it as a removal, so a name carrying one either renames the request or
/// deletes a different subscription. Both are accepted silently.
fn check_subscription_name(name: &str) -> Result<(), Error> {
    if name.is_empty() || name.contains(':') {
        return Err(Error::control_mode_invalid_subscription());
    }
    Ok(())
}

/// Sends commands down a control-mode connection.
///
/// Cheap to clone, and every method that sends takes `&self`, so several threads
/// can issue commands while another watches events.
#[derive(Clone, Debug)]
pub struct ControlSender {
    commands: Sender,
    timeout: Duration,
    /// Whether this tmux can take a pane out of the stream with `off`.
    ///
    /// Read once at attach rather than per call: the server cannot change
    /// release under a connection.
    pane_off_is_safe: bool,
    /// Which server this connection reaches.
    ///
    /// A sender says nothing about where it points, so routing one onto a
    /// handle for a different server would silently talk to this one.
    /// [`crate::Server::over_control_mode`] compares it and refuses.
    identity: crate::ServerIdentity,
    cancellation: crate::CancellationToken,
    next_request_id: Arc<AtomicU64>,
}

impl ControlSender {
    /// Set how long a command waits for its result block.
    ///
    /// The default is the server's [`default_timeout`], which also bounds the
    /// opening handshake. Those two are not comparable: attaching forks tmux
    /// and waits for a server to answer, where a command is a round trip on a
    /// connection that is already open. Set this when a command should give up
    /// sooner than attaching was allowed to take.
    ///
    /// The deadline covers the whole call, so a command large enough to be
    /// worth serializing spends part of its own budget being written.
    ///
    /// [`default_timeout`]: crate::ServerBuilder::default_timeout
    #[must_use]
    pub fn reply_timeout(self, timeout: Duration) -> Self {
        Self { timeout, ..self }
    }

    /// Send one command and wait for its result block.
    ///
    /// A block that tmux closed with `%error` is a result, not an error: it is
    /// reported through [`BlockResult::succeeded`], the same way the process
    /// API keeps a nonzero exit status as data.
    ///
    /// The reply waits for tmux to finish this request's foreground queue,
    /// including nested commands and aliases. `wait-for <channel>` and
    /// `run-shell` without `-b` therefore hold this connection until their
    /// work finishes. Send long waits through
    /// [`crate::Server::cmd`], where the wait costs one process rather than
    /// the connection everything else on it is sharing.
    ///
    /// # Errors
    ///
    /// Returns an error when the command cannot be written as a control-mode
    /// line, the connection has closed, or its deadline elapses while queued,
    /// being written, or awaiting a response.
    ///
    /// # Cancellation
    ///
    /// Cancelling the server token refuses a queued command before it is
    /// written. Once committed, tmux may run it, and its reply is still read and
    /// discarded, so later replies stay aligned.
    pub fn send(&self, command: Command) -> Result<BlockResult, Error> {
        self.send_ordered(command, None)
    }

    /// Return the server this connection reaches.
    pub(crate) const fn identity(&self) -> &crate::ServerIdentity {
        &self.identity
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "Commands use the same ownership contract in both clients."
    )]
    fn begin_ordered(
        &self,
        command: Command,
        boundary: Option<Boundary>,
    ) -> Result<(Submission, bool), Error> {
        let summary = command.summary();
        let sensitive_input = summary.sensitive_argument_count() > 0;
        let line = command
            .control_mode_line()
            .ok_or_else(Error::control_mode_unrepresentable)?;
        let submission = self.commands.begin(
            line,
            Instant::now().checked_add(self.timeout),
            boundary,
            self.cancellation.clone(),
            self.next_request_id.fetch_add(1, Ordering::Relaxed),
            summary,
        );
        Ok((submission, sensitive_input))
    }

    fn send_ordered(
        &self,
        command: Command,
        boundary: Option<Boundary>,
    ) -> Result<BlockResult, Error> {
        let (submission, sensitive_input) = self.begin_ordered(command, boundary)?;
        let mut block = submission.wait()?;
        block.sensitive_input = sensitive_input;
        Ok(block)
    }

    pub(crate) fn send_line(
        &self,
        line: String,
        request_id: u64,
        summary: crate::CommandSummary,
        cancellation: crate::CancellationToken,
    ) -> Result<BlockResult, Error> {
        let sensitive_input = summary.sensitive_argument_count() > 0;
        let mut block = self
            .commands
            .begin(
                line,
                Instant::now().checked_add(self.timeout),
                None,
                cancellation,
                request_id,
                summary,
            )
            .wait()?;
        block.sensitive_input = sensitive_input;
        Ok(block)
    }

    /// Stop tmux sending this connection what a pane writes.
    ///
    /// A control client is sent the output of every pane in the session it
    /// attached to, which is one window's worth or a hundred. One pane running
    /// `yes` moves more than 20 MB in two seconds, and a client tmux judges
    /// five minutes behind is disconnected with `too far behind`, so
    /// discarding the unwanted panes on arrival is not enough.
    ///
    /// Muting a pane that does not exist is not an error; tmux ignores an
    /// unresolvable id here.
    ///
    /// Below [`crate::since::CONTROL_PANE_OFF`] this pauses the pane rather
    /// than taking it out of the stream, because taking it out crashes the
    /// server. tmux keeps reading the pane's pty either way, discarding what
    /// this connection does not want; a paused pane costs only that
    /// connection's own back-pressure, and [`ControlEvents`] sees
    /// [`Event::Paused`] for it.
    ///
    /// At or above that release, taking the pane out of the stream also stops
    /// tmux reading its pty at all -- for every attached client, not only this
    /// connection, and for a one-shot reader such as `capture-pane` too --
    /// until [`Self::unmute_pane`] or [`Self::resume_pane`] turns it back on.
    /// A human attached to the same pane sees it stop updating, and the pane's
    /// own program can block on `write` once the kernel's pty buffer fills.
    /// What arrives once resumed is therefore a backlog, not a gap: see
    /// [`Self::unmute_pane`].
    ///
    /// # Errors
    ///
    /// Returns an error when the connection has closed or tmux refuses the
    /// stream change.
    pub fn mute_pane(&self, pane: &PaneId) -> Result<(), Error> {
        self.set_pane_stream(
            pane,
            if self.pane_off_is_safe {
                "off"
            } else {
                "pause"
            },
        )
    }

    /// Resume sending what a pane writes, after [`Self::mute_pane`].
    ///
    /// Below [`crate::since::CONTROL_PANE_OFF`], [`Self::mute_pane`] paused the
    /// pane rather than taking it out of the stream, and this continues it:
    /// tmux resumes from the pane's current output rather than replaying what
    /// was skipped, so the caller sees a gap, not a backlog.
    ///
    /// At or above that release, [`Self::mute_pane`] took the pane out of the
    /// stream instead, which also stopped tmux reading its pty; this turns
    /// that back on, and everything written while muted arrives at once, as a
    /// backlog rather than a gap.
    ///
    /// This sends both tmux commands in one dispatch rather than choosing
    /// between them, so it is exactly [`Self::resume_pane`] -- either name
    /// undoes [`Self::mute_pane`] correctly regardless of which mechanism the
    /// running tmux used.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection has closed or tmux refuses the
    /// stream change.
    pub fn unmute_pane(&self, pane: &PaneId) -> Result<(), Error> {
        self.recover_pane(pane)
    }

    /// Resume a pane tmux paused because this connection fell behind, or one
    /// [`Self::mute_pane`] muted.
    ///
    /// Pairs with [`Event::Paused`], which arrives once a caller has asked for
    /// pausing with [`Self::pause_after`]. It is also exactly
    /// [`Self::unmute_pane`]: both send the same two commands, so calling
    /// either after [`Self::mute_pane`] recovers the pane on every version,
    /// rather than only the one whose mechanism happens to match.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection has closed or tmux refuses the
    /// stream change.
    pub fn resume_pane(&self, pane: &PaneId) -> Result<(), Error> {
        self.recover_pane(pane)
    }

    /// Undo whichever of `off` or `pause` a pane is under, in one dispatch.
    ///
    /// `refresh-client -A` accepts more than one `pane:state` pair, so `on`
    /// and `continue` travel together. tmux's `control_set_pane_on` and
    /// `control_continue_pane` are each a no-op when their own flag is not
    /// set, so sending both is correct whether the pane was taken out of the
    /// stream, paused, both, or neither.
    fn recover_pane(&self, pane: &PaneId) -> Result<(), Error> {
        self.send(
            Command::new("refresh-client")
                .arg("-A")
                .arg(format!("{pane}:on"))
                .arg("-A")
                .arg(format!("{pane}:continue")),
        )?
        .require_success("refresh-client")
        .map(|_| ())
    }

    /// Ask tmux to report a format whenever it changes.
    ///
    /// tmux answers with [`Event::SubscriptionChanged`] carrying the name given
    /// here, so one connection can hold several subscriptions and tell them
    /// apart. Reporting is coalesced to at most once a second, so this says
    /// what a value became and not every step it took getting there.
    ///
    /// A name already in use is replaced rather than added to.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection has closed, tmux refuses the
    /// subscription, or the name is empty or contains a colon.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use libtmux::control::{ControlMode, Event, Subscription};
    ///
    /// let guard = libtmux::test::TestServer::new()?;
    /// let session = guard.server().new_session("watched")?;
    /// let (commands, mut events) = ControlMode::attach(guard.server(), session.id())
    ///     ?
    ///     .split();
    ///
    /// commands
    ///     .subscribe("title", &Subscription::Session, "#{session_name}")
    ///     ?;
    ///
    /// // The first report arrives without anything having changed, which is
    /// // what makes a subscription usable for reading the value as well.
    /// while let Some(event) = events.next_event() {
    ///     if let Event::SubscriptionChanged { name, value, .. } = event? {
    ///         assert_eq!(name.as_str()?, "title");
    ///         assert_eq!(value.as_str()?, "watched");
    ///         break;
    ///     }
    /// }
    ///
    /// commands.unsubscribe("title")?;
    /// events.shutdown()?;
    /// # guard.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn subscribe(
        &self,
        name: &str,
        watching: &Subscription,
        format: &str,
    ) -> Result<(), Error> {
        check_subscription_name(name)?;
        self.send(
            Command::new("refresh-client")
                .arg("-B")
                .arg(format!("{name}:{watching}:{format}")),
        )?
        .require_success("refresh-client")
        .map(|_| ())
    }

    /// Stop reporting a format this connection subscribed to.
    ///
    /// tmux removes a subscription when it is named with no colon after it,
    /// which is why this cannot be spelled as [`Self::subscribe`] with an empty
    /// format: that would replace the subscription rather than remove it.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection has closed, tmux refuses the
    /// request, or the name is empty or contains a colon.
    pub fn unsubscribe(&self, name: &str) -> Result<(), Error> {
        check_subscription_name(name)?;
        self.send(Command::new("refresh-client").arg("-B").arg(name))?
            .require_success("refresh-client")
            .map(|_| ())
    }

    /// Have tmux pause a pane rather than let this connection fall behind.
    ///
    /// Without this, tmux disconnects a control client that falls more than
    /// five minutes behind, losing everything the connection was for. With it,
    /// tmux instead reports [`Event::Paused`] for the offending pane and keeps
    /// the connection, and every [`Event::Output`] becomes an
    /// [`Event::ExtendedOutput`] carrying how far behind it was.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection has closed or tmux refuses the
    /// pause policy.
    pub fn pause_after(&self, behind: Duration) -> Result<(), Error> {
        self.send(
            Command::new("refresh-client")
                .arg("-f")
                .arg(format!("pause-after={}", behind.as_secs())),
        )?
        .require_success("refresh-client")
        .map(|_| ())
    }

    /// Receive output from these panes and no others.
    ///
    /// Lists panes in the session this connection attached to -- over this
    /// same connection, so the answer cannot disagree with it -- then mutes
    /// every one not named. See [`Self::mute_pane`] for why this beats
    /// filtering what arrives, and why the listing must not reach past this
    /// connection's own session: `off` stops tmux reading a muted pane's pty
    /// for every client, not just this connection, so muting a pane outside
    /// this session would widen the change to whatever else is watching it,
    /// and a control client is sent only its attached session's output in
    /// the first place, so panes outside it were never part of this stream.
    ///
    /// A pane created after this call is not muted, because tmux publishes no
    /// notification for a pane appearing. Repeat this whenever
    /// [`Event::may_have_added_a_pane`] answers `true`.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection has closed, tmux would not list a
    /// pane, returned an unreadable pane ID, or a later mute fails. A failure
    /// after an accepted mute is [`Error::AfterEffect`].
    ///
    /// # Cancellation
    ///
    /// The effect can be partial: panes are muted one at a time, so a cancelled
    /// call can leave some muted and others not. Muting is idempotent, so
    /// calling again finishes the job.
    pub fn watch_only(&self, panes: &[PaneId]) -> Result<(), Error> {
        let listed = self
            .send(
                Command::new("list-panes")
                    .arg("-s")
                    .arg("-F")
                    .arg("#{pane_id}"),
            )?
            .require_success("list-panes")?;

        let mut effect_seen = false;
        for line in listed.output() {
            let found = decode_watched_pane_id(line).map_err(|error| {
                if effect_seen {
                    error.after_effect("watch-only")
                } else {
                    error
                }
            })?;
            if !panes.contains(&found) {
                self.mute_pane(&found).map_err(|error| {
                    if effect_seen {
                        error.after_effect("watch-only")
                    } else {
                        error
                    }
                })?;
                effect_seen = true;
            }
        }

        Ok(())
    }

    /// Report whether tmux still lists this pane, anywhere on the server.
    ///
    /// Used to tell a pane's death from an unrelated listing change: an event
    /// that may mean a pane appeared can equally mean one left, and tmux
    /// publishes no notification that names which.
    pub(crate) fn pane_exists(&self, pane: &PaneId) -> Result<bool, Error> {
        let listed = self
            .send(
                Command::new("list-panes")
                    .arg("-a")
                    .arg("-F")
                    .arg("#{pane_id}"),
            )?
            .require_success("list-panes")?;

        for line in listed.output() {
            if decode_watched_pane_id(line)? == *pane {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn set_pane_stream(&self, pane: &PaneId, state: &str) -> Result<(), Error> {
        self.send(
            Command::new("refresh-client")
                .arg("-A")
                .arg(format!("{pane}:{state}")),
        )?
        .require_success("refresh-client")
        .map(|_| ())
    }

    /// Report whether the connection has closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.commands.is_closed()
    }
}

fn decode_watched_pane_id(line: &TmuxText) -> Result<PaneId, Error> {
    let invalid = |detail| Error::UnreadableFormatValue {
        format: "#{pane_id}",
        detail,
    };
    let id = line.as_str().map_err(|_| invalid(IdParseError::new('%')))?;
    id.parse().map_err(invalid)
}

/// One private marker in the ordered control-mode delivery stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Boundary(u64);

/// Public events and private ordering markers share one bounded FIFO.
#[derive(Debug)]
enum Delivery {
    Event(Event),
    Boundary(Boundary),
}

/// Receives tmux notifications and terminal connection errors.
///
/// This is an iterator of `Result<Event, Error>`. Notifications
/// arrive in order. A connection failure follows all buffered notifications
/// as one `Err`; subsequent calls return `None`. A normal tmux `%exit` arrives
/// as [`Event::Exit`] and is followed by `None` after cleanup succeeds. EOF
/// without `%exit` is [`crate::ControlModeErrorKind::Closed`].
/// [`Server::shutdown`] may discard notifications still waiting for delivery
/// so an unread stream cannot prevent executor shutdown.
///
/// Exhaustion waits for connection cleanup. [`Self::shutdown`] closes early
/// and returns any terminal error the stream has not already delivered.
///
/// Events are buffered, and a consumer that stops reading eventually stops the
/// connection reading from tmux, which is the backpressure tmux already
/// expects from a slow client. During normal operation nothing is dropped;
/// commands wait instead. Drop this handle to opt out of events entirely and
/// the connection runs on.
#[derive(Debug)]
pub struct ControlEvents {
    events: Receiver,
}

impl ControlEvents {
    fn next_delivery(&mut self) -> Option<Delivery> {
        loop {
            let revision = self.events.revision();
            if let Some(delivery) = self.events.try_delivery() {
                return Some(delivery);
            }
            if self.events.finished() {
                return None;
            }
            self.events.wait(revision, None);
        }
    }

    pub(crate) fn shutdown_after_error(self, mut primary: Error) -> Error {
        if let Err(terminal) = self.shutdown() {
            let mut cause = &mut primary;
            while let Error::AfterEffect { source, .. } = cause {
                cause = source.as_mut();
            }
            if matches!(
                cause,
                Error::ControlMode {
                    kind: crate::ControlModeErrorKind::Closed,
                    ..
                }
            ) {
                *cause = terminal;
            }
        }
        primary
    }

    /// Wait for the next notification or terminal error, then return `None`.
    ///
    /// Buffered notifications precede one terminal error. Exhaustion waits
    /// for the client process to be reaped. Subsequent calls return `None`.
    ///
    /// # Errors
    ///
    /// Yields a terminal error for transport failure, unexpected EOF, frame
    /// limits, command timeout, executor shutdown, or failed cleanup. A panic
    /// in the connection thread resumes here.
    ///
    /// # Blocking
    ///
    /// The server operation token does not interrupt this read. Use
    /// [`Self::next_event_timeout`] to bound it, or [`Server::shutdown`] to
    /// close the connection.
    pub fn next_event(&mut self) -> Option<Result<Event, Error>> {
        self.next()
    }

    /// Wait at most `timeout` for the next notification.
    ///
    /// Returns `None` after connection cleanup. A timeout consumes no event
    /// and leaves this iterator usable.
    ///
    /// # Errors
    ///
    /// Returns a transient dispatch timeout when no event arrives in time,
    /// or the terminal connection error described by [`Self::next_event`].
    pub fn next_event_timeout(&mut self, timeout: Duration) -> Result<Option<Event>, Error> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            let revision = self.events.revision();
            match self.events.try_delivery() {
                Some(Delivery::Event(event)) => return Ok(Some(event)),
                Some(Delivery::Boundary(_)) => continue,
                None => {}
            }
            if self.events.finished() {
                self.events.finish()?;
                return Ok(None);
            }
            if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
                return Err(Error::control_mode_dispatch_timeout());
            }
            self.events.wait(revision, deadline);
        }
    }

    /// Close the connection and wait for its process to be reaped.
    ///
    /// Discards unread notifications, even while senders remain alive.
    ///
    /// # Errors
    ///
    /// Returns a connection or cleanup error not already delivered by the
    /// iterator. Requested closure succeeds when cleanup succeeds.
    pub fn shutdown(mut self) -> Result<(), Error> {
        self.events.shutdown()
    }
}

impl Iterator for ControlEvents {
    type Item = Result<Event, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.next_delivery() {
                Some(Delivery::Event(event)) => return Some(Ok(event)),
                Some(Delivery::Boundary(_)) => {}
                None => return self.events.finish().err().map(Err),
            }
        }
    }
}

const NARROW_IDLE: u8 = 0;
const NARROW_RUNNING: u8 = 1;
const NARROW_DIRTY: u8 = 2;

/// What one pane writes, as it writes it.
///
/// Built by [`crate::Pane::stream_output`]. This is an iterator of the bytes
/// that pane produced, in order.
/// Its infallible items combine normal termination, connection failure, and
/// the watched pane being killed into `None`, once whatever was already
/// buffered has drained. Call [`Self::shutdown`] to observe a connection
/// error, or use [`ControlEvents`] to receive errors during iteration; a
/// killed pane is not an error either way, since ending is the correct answer
/// once nothing more will arrive.
///
/// tmux is told to send this connection nothing but the watched pane. A
/// neighbouring pane running `yes` otherwise moves tens of megabytes a second
/// through it, and the watched pane's output queues behind that.
///
/// Each of these owns a control-mode connection, so a caller watching many
/// panes at once is better served by [`ControlEvents`] and one connection,
/// narrowed with [`ControlSender::watch_only`].
#[derive(Debug)]
pub struct PaneOutput {
    pane: PaneId,
    events: ControlEvents,
    boundary: u64,
    closed: bool,
    /// Kept to re-narrow the subscription and to check the watched pane still
    /// exists, not to send a caller's commands.
    ///
    /// tmux has no notification for a pane being created, so a pane that
    /// appears after the attach arrives unmuted; the event loop below repairs
    /// that when an event says the set of panes may have grown.
    sender: ControlSender,
    /// Whether re-narrowing is idle, running, or needs another pass.
    ///
    /// Each pass costs a `list-panes` round trip, so a burst coalesces.
    narrowing: Arc<AtomicU8>,
    /// The re-narrowing pass in flight, if any, polled for whether it found
    /// the watched pane still listed.
    narrow_handle: Option<JoinHandle<bool>>,
    narrow_complete: Arc<AtomicBool>,
    narrow_error: Option<Error>,
}

impl PaneOutput {
    pub(crate) fn new(pane: PaneId, events: ControlEvents, sender: ControlSender) -> Self {
        Self {
            pane,
            events,
            boundary: 0,
            closed: false,
            sender,
            narrowing: Arc::new(AtomicU8::new(NARROW_IDLE)),
            narrow_handle: None,
            narrow_complete: Arc::new(AtomicBool::new(false)),
            narrow_error: None,
        }
    }

    /// Tell tmux again to send only this pane, and check it is still there.
    ///
    /// A background thread repairs subscriptions while the caller keeps
    /// draining events. tmux publishes no notification naming a pane that left the
    /// server -- only ones consistent with a pane having *appeared* -- so a
    /// pane's death is read from the same re-listing this already does to
    /// repair the mute set, rather than from a second round trip. A failure
    /// checking or re-narrowing leaves the caller its own pane alongside
    /// noise, so it does not end the stream by itself; only a listing that
    /// completes without the watched pane in it does.
    fn narrow(&mut self) {
        if self.narrow_handle.is_some() {
            self.narrowing.store(NARROW_DIRTY, Ordering::Release);
            return;
        }
        self.narrowing.store(NARROW_RUNNING, Ordering::Release);
        self.narrow_complete.store(false, Ordering::Release);
        let complete = Arc::clone(&self.narrow_complete);
        let sender = self.sender.clone();
        let pane = self.pane.clone();
        let narrowing = Arc::clone(&self.narrowing);
        let shared = Arc::clone(&self.events.events.shared);
        let work = move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                loop {
                    let _ = sender.watch_only(std::slice::from_ref(&pane));
                    match narrowing.compare_exchange(
                        NARROW_RUNNING,
                        NARROW_IDLE,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(state) => {
                            debug_assert_eq!(state, NARROW_DIRTY);
                            narrowing.store(NARROW_RUNNING, Ordering::Release);
                        }
                    }
                }
                sender.pane_exists(&pane).unwrap_or(true)
            }));
            complete.store(true, Ordering::Release);
            shared.wake();
            match outcome {
                Ok(found) => found,
                Err(panic) => std::panic::resume_unwind(panic),
            }
        };
        match std::thread::Builder::new()
            .name("libtmux-control-narrow".into())
            .spawn(work)
        {
            Ok(handle) => self.narrow_handle = Some(handle),
            Err(error) => {
                self.narrowing.store(NARROW_IDLE, Ordering::Release);
                self.narrow_error = Some(Error::control_mode(error));
                self.closed = true;
            }
        }
    }

    fn check_narrowing(&mut self) {
        if self.narrow_complete.load(Ordering::Acquire)
            && let Some(handle) = self.narrow_handle.take()
        {
            match handle.join() {
                Ok(true) => {}
                Ok(false) => self.closed = true,
                Err(panic) => std::panic::resume_unwind(panic),
            }
            if !self.closed && self.narrowing.load(Ordering::Acquire) == NARROW_DIRTY {
                self.narrow();
            }
        }
    }

    /// Return the pane being watched.
    #[must_use]
    pub const fn pane(&self) -> &PaneId {
        &self.pane
    }

    /// Return the connection this stream reads, to mute or resume panes on it.
    ///
    /// The watched pane's own stream cannot be muted through [`Self`] alone:
    /// [`ControlSender::mute_pane`], [`ControlSender::unmute_pane`], and
    /// [`ControlSender::resume_pane`] all take a target, and [`Self::pane`]
    /// names this one.
    #[must_use]
    pub const fn sender(&self) -> &ControlSender {
        &self.sender
    }

    /// Capture the pane's visible screen at an ordered point in this stream.
    ///
    /// `on_output` receives every unread chunk tmux ordered before the capture
    /// block. The callback owns any retention policy, so libtmux does not
    /// retain those chunks. It runs synchronously and should return promptly.
    ///
    /// Each chunk passed to `on_output` is consumed from this stream and is
    /// not repeated by [`Self::next_chunk`], even when this returns an error.
    ///
    /// The visible screen and preceding output may overlap: the screen is
    /// tmux's rendered grid, while the callback receives the raw terminal
    /// byte stream that reached that grid.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn inspect(pane: &libtmux::Pane) -> Result<(), libtmux::Error> {
    /// let mut output = pane.stream_output()?;
    /// let mut preceding = Vec::new();
    /// let visible = output.snapshot(|chunk| preceding.extend_from_slice(chunk))?;
    ///
    /// println!("{} visible lines after {} raw bytes", visible.len(), preceding.len());
    /// output.shutdown()
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error when the connection closes, the command deadline
    /// elapses, or tmux refuses the capture, including when the pane vanished.
    ///
    /// # Cancellation
    ///
    /// Cancelling the server token leaves chunks already passed to
    /// `on_output` consumed. The stream stays usable.
    pub fn snapshot(&mut self, mut on_output: impl FnMut(&[u8])) -> Result<Vec<TmuxText>, Error> {
        self.boundary = self.boundary.wrapping_add(1);
        let boundary = Boundary(self.boundary);
        let command = Command::new("capture-pane")
            .arg("-p")
            .arg("-t")
            .arg(self.pane.to_string());
        let (mut reply, _) = self.sender.begin_ordered(command, Some(boundary))?;
        let mut answer = None;
        loop {
            let revision = self.events.events.revision();
            if answer.is_none()
                && let Some(outcome) = reply.poll()
            {
                answer = Some(outcome?);
            }
            let mut reached = false;
            match self.events.events.try_delivery() {
                Some(Delivery::Event(
                    Event::Output { pane, bytes } | Event::ExtendedOutput { pane, bytes, .. },
                )) if pane == self.pane => on_output(&bytes),
                Some(Delivery::Event(Event::Exit { .. })) => self.closed = true,
                Some(Delivery::Event(event)) => {
                    if event.may_have_added_a_pane() {
                        self.narrow();
                    }
                }
                Some(Delivery::Boundary(found)) if found == boundary => reached = true,
                Some(Delivery::Boundary(_)) => {}
                None => {
                    if self.events.events.finished() {
                        self.closed = true;
                        if answer.is_none() {
                            reply.wait()?;
                        }
                        return Err(Error::control_mode_closed());
                    }
                    self.events.events.wait(revision, reply.wait_deadline());
                }
            }
            if reached {
                let block = match answer.take() {
                    Some(block) => block,
                    None => reply.wait()?,
                }
                .require_success("capture-pane")?;
                return Ok(block.output);
            }
        }
    }

    /// Return the next chunk this pane wrote, or `None` once it stops.
    ///
    /// A chunk is what tmux chose to report at once, which is not a line and
    /// not a fixed size. Callers wanting lines should buffer.
    ///
    /// # Blocking
    ///
    /// The server operation token does not interrupt this read. Use
    /// [`Self::next_chunk_timeout`] to bound it, or [`Server::shutdown`] to
    /// close the connection.
    pub fn next_chunk(&mut self) -> Option<Vec<u8>> {
        self.next()
    }

    /// Wait at most `timeout` for the next output chunk.
    ///
    /// Returns `None` when the pane stream ends. A timeout consumes no chunk
    /// and leaves this iterator usable. Call [`Self::shutdown`] to inspect a
    /// terminal connection error.
    ///
    /// # Errors
    ///
    /// Returns a transient dispatch timeout when no chunk arrives in time.
    pub fn next_chunk_timeout(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, Error> {
        self.next_chunk_until(Instant::now().checked_add(timeout))
    }

    /// End the connection and report how it went.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection failed before it was closed.
    pub fn shutdown(mut self) -> Result<(), Error> {
        let outcome = self.events.events.shutdown();
        if let Some(handle) = self.narrow_handle.take()
            && let Err(panic) = handle.join()
        {
            std::panic::resume_unwind(panic);
        }
        outcome?;
        match self.narrow_error.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl PaneOutput {
    fn next_chunk_until(&mut self, deadline: Option<Instant>) -> Result<Option<Vec<u8>>, Error> {
        loop {
            let revision = self.events.events.revision();
            self.check_narrowing();
            if self.closed {
                return Ok(None);
            }
            match self.events.events.try_delivery() {
                Some(Delivery::Event(
                    Event::Output { pane, bytes } | Event::ExtendedOutput { pane, bytes, .. },
                )) if pane == self.pane => return Ok(Some(bytes)),
                Some(Delivery::Event(Event::Exit { .. })) => {
                    self.closed = true;
                    return Ok(None);
                }
                Some(Delivery::Event(event)) => {
                    if event.may_have_added_a_pane()
                        || matches!(
                            event,
                            Event::WindowClosed { .. } | Event::UnlinkedWindowClosed { .. }
                        )
                    {
                        self.narrow();
                    }
                }
                Some(Delivery::Boundary(_)) => {}
                None => {
                    if self.events.events.finished() {
                        self.closed = true;
                        return Ok(None);
                    }
                    if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
                        return Err(Error::control_mode_dispatch_timeout());
                    }
                    self.events.events.wait(revision, deadline);
                }
            }
        }
    }
}

impl Iterator for PaneOutput {
    type Item = Vec<u8>;
    fn next(&mut self) -> Option<Self::Item> {
        self.next_chunk_until(None).unwrap_or(None)
    }
}

/// Parse one control-mode protocol line, for fuzzing only.
///
/// The parser is the crate's most exposed surface: it reads bytes from a
/// process that keeps running, and every other decoder sits behind a tmux
/// command that ended. Nothing here is a supported API -- it exists so a
/// fuzzer can reach `Line::parse` without it becoming public -- and it is
/// gated behind a feature no release turns on.
#[cfg(feature = "unstable-fuzzing")]
#[doc(hidden)]
pub fn __fuzz_parse_control_line(line: &[u8]) {
    let _ = Line::parse(line);
}

#[cfg(feature = "unstable-fuzzing")]
#[doc(hidden)]
pub use fuzz::__fuzz_control_blocks;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "control/lifecycle_tests.rs"]
mod lifecycle_tests;
