//! Watching a tmux server over control mode.
//!
//! Every other API in this crate spawns a tmux process per command. Control
//! mode opens one connection and keeps it: commands go down it, and tmux
//! reports what happens on the server as it happens. That is the difference
//! between asking tmux what is true and being told when it changes.
//!
//! Sending and watching are separate handles, so one task can act on what
//! another sees:
//!
//! ```no_run
//! # async fn watch(server: &libtmux::Server, id: &libtmux::SessionId) -> Result<(), libtmux::Error> {
//! use libtmux::control::{ControlMode, Event};
//!
//! let (commands, mut events) = ControlMode::attach(server, id).await?.split();
//!
//! // Commands travel down the connection, so none of these spawn a process.
//! let listed = commands.send(libtmux::Command::new("list-windows")).await?;
//! assert!(listed.succeeded());
//!
//! // The watcher reads, and only reads. A reply arrives on the connection
//! // the events arrive on, so a loop that stops reading in order to await one
//! // is waiting on the connection it stopped reading.
//! let watcher = tokio::spawn(async move {
//!     while let Some(event) = events.next_event().await {
//!         match event {
//!             Event::Output { pane, bytes } => println!("{pane}: {} bytes", bytes.len()),
//!             Event::Exit { .. } => break,
//!             other => println!("{other:?}"),
//!         }
//!     }
//!
//!     // The stream ending says the connection is over; this says why.
//!     events.shutdown().await
//! });
//!
//! // Acting on what the watcher sees happens out here, on the other handle,
//! // which is what having two of them is for.
//! commands.send(libtmux::Command::new("list-panes")).await?;
//!
//! let _ = watcher.await;
//! Ok(())
//! # }
//! ```
//!
//! `examples/watch.rs` is this as a program that runs, against a server it
//! starts and cleans up.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_core::Stream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::limits::ControlLimits;
use crate::version::since::CONTROL_PANE_OFF;
use crate::{Command, Error, IdParseError, PaneId, Server, SessionId, TmuxText, WindowId};

mod actor;
mod protocol;

#[cfg(test)]
use actor::{HELD_WHILE_AWAITING, ReplySlot, ReplySlots, admit_request};
use actor::{Request, deadline_elapsed};
#[cfg(any(test, feature = "unstable-fuzzing"))]
use protocol::Line;
#[cfg(test)]
use protocol::unescape_output;

/// Something tmux reported that no command asked for.
///
/// The variants cover every notification tmux publishes across the supported
/// releases. [`Event::Other`] keeps an unrecognized one rather than dropping
/// it, because tmux adds notifications between releases; its name is the tmux
/// notification without the leading `%`.
///
/// Four of these are newer than the oldest tmux this crate supports:
/// `%config-error`, `%message`, `%paste-buffer-changed` and
/// `%paste-buffer-deleted` are never emitted by 3.2a. Nothing else in the
/// vocabulary is version-dependent.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// A pane produced output.
    ///
    /// The bytes are exactly what the pane wrote. tmux escapes only what
    /// would break the line protocol -- bytes below `0x20`, and backslash --
    /// so everything above `0x7f` arrives literally and the line as a whole
    /// is not necessarily UTF-8. That is why this is bytes.
    Output {
        /// The pane that produced it.
        pane: PaneId,
        /// The output bytes.
        bytes: Vec<u8>,
    },
    /// A pane produced output, and tmux said how far behind it is.
    ///
    /// Replaces [`Event::Output`] for the whole connection once a client asks
    /// for `pause-after`, so a caller that sets that flag must handle both.
    ExtendedOutput {
        /// The pane that produced it.
        pane: PaneId,
        /// How long this output sat before tmux sent it.
        age: Duration,
        /// The output bytes.
        bytes: Vec<u8>,
    },
    /// tmux stopped sending a pane's output.
    ///
    /// Two things ask for this: [`ControlSender::pause_after`], after which
    /// tmux pauses a pane this client has fallen behind on, and
    /// [`ControlSender::mute_pane`] below [`crate::since::CONTROL_PANE_OFF`],
    /// which pauses rather than take a pane out of the stream. Resume with
    /// [`ControlSender::resume_pane`].
    Paused {
        /// The pane that was paused.
        pane: PaneId,
    },
    /// tmux resumed a pane it had paused.
    Continued {
        /// The pane that resumed.
        pane: PaneId,
    },
    /// The attached session changed.
    SessionChanged {
        /// The session now attached.
        session: SessionId,
    },
    /// A session was renamed.
    SessionRenamed {
        /// The session that was renamed.
        session: SessionId,
        /// Its new name.
        name: TmuxText,
    },
    /// A session's active window changed.
    SessionWindowChanged {
        /// The session whose active window changed.
        session: SessionId,
        /// The window now active in it.
        window: WindowId,
    },
    /// A session was created or destroyed, so the session list is now wrong.
    ///
    /// tmux says only that the set changed, not which session it was.
    SessionsChanged,
    /// A window was linked into the attached session.
    WindowAdded {
        /// The window that appeared.
        window: WindowId,
    },
    /// A window in the attached session closed.
    WindowClosed {
        /// The window that closed.
        window: WindowId,
    },
    /// A window in the attached session was renamed.
    WindowRenamed {
        /// The window that was renamed.
        window: WindowId,
        /// Its new name.
        name: TmuxText,
    },
    /// A window's active pane changed.
    WindowPaneChanged {
        /// The window whose active pane changed.
        window: WindowId,
        /// The pane now active in it.
        pane: PaneId,
    },
    /// A window appeared that the attached session does not link.
    UnlinkedWindowAdded {
        /// The window that appeared.
        window: WindowId,
    },
    /// A window the attached session does not link closed.
    UnlinkedWindowClosed {
        /// The window that closed.
        window: WindowId,
    },
    /// A window the attached session does not link was renamed.
    UnlinkedWindowRenamed {
        /// The window that was renamed.
        window: WindowId,
        /// Its new name.
        name: TmuxText,
    },
    /// A window's panes were rearranged, added to, or removed from.
    ///
    /// tmux has no notification for a pane appearing, so this is the one that
    /// reports it: every split changes the layout, including a detached split
    /// that leaves the active pane alone and reports nothing else.
    LayoutChanged {
        /// The window whose layout changed.
        window: WindowId,
        /// The new layout, in tmux's own layout syntax.
        layout: TmuxText,
        /// The layout as displayed, which differs when a pane is zoomed.
        visible_layout: TmuxText,
        /// The window's flags, such as `*` for active.
        flags: TmuxText,
    },
    /// A pane entered or left a mode, such as copy mode.
    ///
    /// tmux says the pane changed mode, not which mode it is now; read
    /// `pane_mode` to learn that.
    PaneModeChanged {
        /// The pane whose mode changed.
        pane: PaneId,
    },
    /// A client detached from the server.
    ClientDetached {
        /// The client that left.
        client: TmuxText,
    },
    /// A client switched to a different session.
    ClientSessionChanged {
        /// The client that switched.
        client: TmuxText,
        /// The session it switched to.
        session: SessionId,
        /// That session's name.
        name: TmuxText,
    },
    /// A paste buffer was created or replaced.
    PasteBufferChanged {
        /// The buffer's name.
        name: TmuxText,
    },
    /// A paste buffer was deleted.
    PasteBufferDeleted {
        /// The buffer's name.
        name: TmuxText,
    },
    /// A format this client subscribed to with `refresh-client -B` changed.
    SubscriptionChanged {
        /// The subscription name the caller chose.
        name: TmuxText,
        /// The session it is about.
        session: SessionId,
        /// The window it is about, when the subscription names one.
        window: Option<WindowId>,
        /// That window's index, when the subscription names one.
        index: Option<u32>,
        /// The pane it is about, when the subscription names one.
        pane: Option<PaneId>,
        /// The format's new value.
        value: TmuxText,
    },
    /// tmux could not read part of its configuration.
    ConfigError {
        /// What tmux said was wrong.
        message: TmuxText,
    },
    /// A message tmux was asked to display, by `display-message` or a hook.
    Message {
        /// The message text.
        message: TmuxText,
    },
    /// The server is going away, so no further events will arrive.
    Exit {
        /// Why, when tmux gave a reason. `None` is an ordinary shutdown.
        reason: Option<TmuxText>,
    },
    /// A notification this crate does not model.
    Other {
        /// The notification name, without its `%`.
        name: String,
        /// The rest of the line.
        rest: TmuxText,
    },
}

impl Event {
    /// Report whether a listing taken before this event may now be wrong.
    ///
    /// Output and the flow-control events say nothing about the shape of the
    /// server. [`Event::Other`] counts as invalidating, because an unmodelled
    /// notification is one whose meaning is not known here.
    #[must_use]
    pub const fn invalidates_listings(&self) -> bool {
        !matches!(
            self,
            Self::Output { .. }
                | Self::ExtendedOutput { .. }
                | Self::Paused { .. }
                | Self::Continued { .. }
                | Self::SubscriptionChanged { .. }
                | Self::ConfigError { .. }
                | Self::Message { .. }
        )
    }

    /// Report whether a pane may have appeared since the last look.
    ///
    /// tmux publishes no notification for a pane being created, so this is a
    /// conservative union of the events that can accompany one. A caller that
    /// narrowed with [`ControlSender::watch_only`] must repeat it when this
    /// answers `true`.
    #[must_use]
    pub const fn may_have_added_a_pane(&self) -> bool {
        matches!(
            self,
            Self::LayoutChanged { .. }
                | Self::WindowAdded { .. }
                | Self::UnlinkedWindowAdded { .. }
                | Self::SessionsChanged
                | Self::SessionChanged { .. }
                | Self::Other { .. }
        )
    }

    /// Return the pane this event is about, when it is about one.
    #[must_use]
    pub const fn pane(&self) -> Option<&PaneId> {
        match self {
            Self::Output { pane, .. }
            | Self::ExtendedOutput { pane, .. }
            | Self::Paused { pane }
            | Self::Continued { pane }
            | Self::PaneModeChanged { pane }
            | Self::WindowPaneChanged { pane, .. } => Some(pane),
            Self::SubscriptionChanged { pane, .. } => pane.as_ref(),
            _ => None,
        }
    }

    /// Return the window this event is about, when it is about one.
    #[must_use]
    pub const fn window(&self) -> Option<&WindowId> {
        match self {
            Self::WindowAdded { window }
            | Self::WindowClosed { window }
            | Self::WindowRenamed { window, .. }
            | Self::WindowPaneChanged { window, .. }
            | Self::UnlinkedWindowAdded { window }
            | Self::UnlinkedWindowClosed { window }
            | Self::UnlinkedWindowRenamed { window, .. }
            | Self::LayoutChanged { window, .. }
            | Self::SessionWindowChanged { window, .. } => Some(window),
            Self::SubscriptionChanged { window, .. } => window.as_ref(),
            _ => None,
        }
    }
}

/// The outcome of one command sent over control mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockResult {
    number: u64,
    succeeded: bool,
    output: Vec<TmuxText>,
    sensitive_input: bool,
}

impl BlockResult {
    /// Return the block number tmux assigned.
    ///
    /// tmux assigns this, and correlation uses it rather than counting
    /// commands: a command that fails early can leave a caller waiting
    /// forever for a block tmux will never send.
    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }

    /// Report whether tmux closed the block with `%end` rather than `%error`.
    #[must_use]
    pub const fn succeeded(&self) -> bool {
        self.succeeded
    }

    /// Return the lines tmux printed inside the block.
    #[must_use]
    pub fn output(&self) -> &[TmuxText] {
        &self.output
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
/// That is not decoration: the point of control mode is to act on what you
/// observe, and a single object would need `&mut` for both, so a task awaiting
/// an event could never send the command that event implies.
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
    pub async fn attach(server: &Server, session: &SessionId) -> Result<Self, Error> {
        Self::attach_with_limits(server, session, ControlLimits::default()).await
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
    pub async fn attach_with_limits(
        server: &Server,
        session: &SessionId,
        limits: ControlLimits,
    ) -> Result<Self, Error> {
        // Asked before the attach so a connection never carries an unknown
        // answer: a release that cannot be read is treated as too old, which
        // costs a pane's back-pressure rather than the server.
        let pane_off_is_safe = server
            .capabilities()
            .await
            .is_ok_and(|capabilities| capabilities.tmux_version().meets(&CONTROL_PANE_OFF));

        let timeout = server.default_timeout();
        let actor::OpenedConnection {
            commands,
            events,
            stop,
            connection,
        } = actor::open(server.spawn_control(session).await?, limits, timeout).await?;

        Ok(Self {
            sender: ControlSender {
                commands,
                timeout,
                pane_off_is_safe,
            },
            events: ControlEvents {
                events,
                stop,
                connection,
            },
        })
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
    /// being written, or awaiting a response. Cancellation has the same write
    /// boundary as [`ControlSender::send`].
    pub async fn send(&self, command: Command) -> Result<BlockResult, Error> {
        self.sender.send(command).await
    }

    /// Return the next notification, or `None` once the connection closes.
    pub async fn next_event(&mut self) -> Option<Event> {
        self.events.next_event().await
    }

    /// Close the connection and report how it ended.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection failed before it was closed.
    pub async fn shutdown(self) -> Result<(), Error> {
        drop(self.sender);
        self.events.shutdown().await
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
/// Cheap to clone, and every method takes `&self`, so several tasks can issue
/// commands while another watches events.
#[derive(Clone, Debug)]
pub struct ControlSender {
    commands: mpsc::Sender<Request>,
    timeout: Duration,
    /// Whether this tmux can take a pane out of the stream with `off`.
    ///
    /// Read once at attach rather than per call: the server cannot change
    /// release under a connection.
    pane_off_is_safe: bool,
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
    /// The block says tmux answered the command, not that what the command
    /// asked for has happened. Almost always those are the same moment. They
    /// are not for the commands tmux answers at once and then parks this
    /// client's queue behind: `wait-for <channel>` until something signals it,
    /// and `run-shell` without `-b` for as long as its shell command runs.
    /// Each reports success, neither has finished, and the next command sent
    /// waits for it however long that is. Send those through
    /// [`crate::Server::cmd`], where the wait costs one process rather than
    /// the connection everything else on it is sharing.
    ///
    /// Dropping this future while it is queued prevents the command from being
    /// written. Once the connection commits it for writing, tmux may execute
    /// it; its reply position stays reserved so later replies remain aligned.
    ///
    /// # Errors
    ///
    /// Returns an error when the command cannot be written as a control-mode
    /// line, the connection has closed, or its deadline elapses while queued,
    /// being written, or awaiting a response.
    pub async fn send(&self, command: Command) -> Result<BlockResult, Error> {
        self.send_ordered(command, None).await
    }

    /// Send a command whose completed block marks one point in event order.
    async fn send_ordered(
        &self,
        command: Command,
        boundary: Option<Boundary>,
    ) -> Result<BlockResult, Error> {
        let deadline = Instant::now().checked_add(self.timeout);
        let sensitive_input = command.summary().sensitive_argument_count() > 0;
        let line = command
            .control_mode_line()
            .ok_or_else(Error::control_mode_unrepresentable)?;
        let (result, mut answer) = oneshot::channel();
        let (commit, mut commitment) = oneshot::channel();
        let finish = |answer: Result<Result<BlockResult, Error>, oneshot::error::RecvError>| {
            let mut block = answer.map_err(|_| Error::control_mode_closed())??;
            block.sensitive_input = sensitive_input;
            Ok(block)
        };

        if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
            return Err(Error::control_mode_dispatch_timeout());
        }
        let permit = tokio::select! {
            biased;
            permit = self.commands.reserve() => {
                permit.map_err(|_| Error::control_mode_closed())?
            }
            () = deadline_elapsed(deadline) => {
                return Err(Error::control_mode_dispatch_timeout());
            }
        };
        permit.send(Request {
            line,
            deadline,
            result,
            commit,
            boundary,
        });

        tokio::select! {
            biased;
            answer = &mut answer => return finish(answer),
            () = deadline_elapsed(deadline) => {
                commitment.close();
                match commitment.try_recv() {
                    Ok(()) => {}
                    Err(oneshot::error::TryRecvError::Closed | oneshot::error::TryRecvError::Empty) => {
                        return Err(Error::control_mode_dispatch_timeout());
                    }
                }
            }
            committed = &mut commitment => {
                if committed.is_err() {
                    return finish(answer.await);
                }
            }
        }

        finish(answer.await)
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
    /// server. tmux reports a paused pane, so a caller reading
    /// [`ControlEvents`] sees [`Event::Paused`] for it there and not on a
    /// newer tmux. The pane stops arriving either way; what a paused pane
    /// costs is the back-pressure, since tmux keeps draining its terminal.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection has closed or tmux refuses the
    /// stream change.
    pub async fn mute_pane(&self, pane: &PaneId) -> Result<(), Error> {
        self.set_pane_stream(
            pane,
            if self.pane_off_is_safe {
                "off"
            } else {
                "pause"
            },
        )
        .await
    }

    /// Resume sending what a pane writes, after [`Self::mute_pane`].
    ///
    /// tmux resumes from the pane's current output rather than replaying what
    /// was skipped, so a caller unmuting a pane has a gap, not a backlog.
    ///
    /// Below [`crate::since::CONTROL_PANE_OFF`] this continues the pane that
    /// [`Self::mute_pane`] paused, which is the same gap by another name.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection has closed or tmux refuses the
    /// stream change.
    pub async fn unmute_pane(&self, pane: &PaneId) -> Result<(), Error> {
        self.set_pane_stream(
            pane,
            if self.pane_off_is_safe {
                "on"
            } else {
                "continue"
            },
        )
        .await
    }

    /// Resume a pane tmux paused because this connection fell behind.
    ///
    /// Pairs with [`Event::Paused`], which only arrives once a caller has
    /// asked for pausing with [`Self::pause_after`].
    ///
    /// # Errors
    ///
    /// Returns an error when the connection has closed or tmux refuses the
    /// stream change.
    pub async fn resume_pane(&self, pane: &PaneId) -> Result<(), Error> {
        self.set_pane_stream(pane, "continue").await
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
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::control::{ControlMode, Event, Subscription};
    ///
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("watched").await?;
    /// let (commands, mut events) = ControlMode::attach(guard.server(), session.id())
    ///     .await?
    ///     .split();
    ///
    /// commands
    ///     .subscribe("title", &Subscription::Session, "#{session_name}")
    ///     .await?;
    ///
    /// // The first report arrives without anything having changed, which is
    /// // what makes a subscription usable for reading the value as well.
    /// while let Some(event) = events.next_event().await {
    ///     if let Event::SubscriptionChanged { name, value, .. } = event {
    ///         assert_eq!(name.as_str()?, "title");
    ///         assert_eq!(value.as_str()?, "watched");
    ///         break;
    ///     }
    /// }
    ///
    /// commands.unsubscribe("title").await?;
    /// events.shutdown().await?;
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn subscribe(
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
        )
        .await?
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
    pub async fn unsubscribe(&self, name: &str) -> Result<(), Error> {
        check_subscription_name(name)?;
        self.send(Command::new("refresh-client").arg("-B").arg(name))
            .await?
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
    pub async fn pause_after(&self, behind: Duration) -> Result<(), Error> {
        self.send(
            Command::new("refresh-client")
                .arg("-f")
                .arg(format!("pause-after={}", behind.as_secs())),
        )
        .await?
        .require_success("refresh-client")
        .map(|_| ())
    }

    /// Receive output from these panes and no others.
    ///
    /// Lists panes over this same connection, so the answer cannot disagree
    /// with the connection it configures, then mutes every pane not named.
    /// See [`Self::mute_pane`] for why this beats filtering what arrives.
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
    pub async fn watch_only(&self, panes: &[PaneId]) -> Result<(), Error> {
        let listed = self
            .send(
                Command::new("list-panes")
                    .arg("-a")
                    .arg("-F")
                    .arg("#{pane_id}"),
            )
            .await?
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
                self.mute_pane(&found).await.map_err(|error| {
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

    async fn set_pane_stream(&self, pane: &PaneId, state: &str) -> Result<(), Error> {
        self.send(
            Command::new("refresh-client")
                .arg("-A")
                .arg(format!("{pane}:{state}")),
        )
        .await?
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

/// Receives what tmux reports without being asked.
///
/// This is a [`Stream`], so it composes with `select!`, timeouts, and the rest
/// of the async ecosystem rather than demanding a loop of its own.
///
/// Events are buffered, and a consumer that stops reading eventually stops the
/// connection reading from tmux, which is the backpressure tmux already
/// expects from a slow client. Nothing is dropped; commands wait instead. Drop
/// this handle to opt out of events entirely and the connection runs on.
#[derive(Debug)]
pub struct ControlEvents {
    events: mpsc::Receiver<Delivery>,
    /// Ends the connection when this handle asks, or when it is dropped.
    stop: watch::Sender<()>,
    connection: tokio::task::JoinHandle<Result<(), Error>>,
}

impl ControlEvents {
    async fn next_delivery(&mut self) -> Option<Delivery> {
        self.events.recv().await
    }

    /// Return the next notification, or `None` once the connection closes.
    pub async fn next_event(&mut self) -> Option<Event> {
        loop {
            match self.next_delivery().await? {
                Delivery::Event(event) => return Some(event),
                Delivery::Boundary(_) => {}
            }
        }
    }

    /// End the connection and report how it went.
    ///
    /// The stream running out says only that the connection is over. This says
    /// why, which is the difference between a session that ended and a pipe
    /// that broke. It ends the connection outright rather than waiting for the
    /// senders, so it is the same call whether the connection is still healthy
    /// or tmux hung up an hour ago.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection failed before it was closed.
    pub async fn shutdown(mut self) -> Result<(), Error> {
        let _ = self.stop.send(());
        // Draining releases a connection that is parked handing over an event,
        // so it reaches its own shutdown rather than waiting for a reader that
        // is not coming back.
        self.events.close();
        while self.events.recv().await.is_some() {}

        self.connection
            .await
            .map_err(|_| Error::control_mode_closed())?
    }
}

impl Stream for ControlEvents {
    type Item = Event;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Event>> {
        loop {
            match std::task::ready!(self.events.poll_recv(context)) {
                Some(Delivery::Event(event)) => return Poll::Ready(Some(event)),
                Some(Delivery::Boundary(_)) => {}
                None => return Poll::Ready(None),
            }
        }
    }
}

const NARROW_IDLE: u8 = 0;
const NARROW_RUNNING: u8 = 1;
const NARROW_DIRTY: u8 = 2;

/// What one pane writes, as it writes it.
///
/// Built by [`crate::Pane::stream_output`]. This is a [`Stream`] of the bytes
/// that pane produced, in order.
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
    /// Kept to re-narrow the subscription, not to send a caller's commands.
    ///
    /// tmux has no notification for a pane being created, so a pane that
    /// appears after the attach arrives unmuted; the event loop below repairs
    /// that when an event says the set of panes may have grown.
    sender: ControlSender,
    /// Whether re-narrowing is idle, running, or needs another pass.
    ///
    /// Each pass costs a `list-panes` round trip, so a burst coalesces.
    narrowing: Arc<AtomicU8>,
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
        }
    }

    /// Tell tmux again to send only this pane.
    ///
    /// Detached rather than awaited so [`Stream::poll_next`], which cannot
    /// await, repairs the subscription the same way [`Self::next_chunk`] does.
    /// A failure leaves the caller its own pane alongside noise, so it does
    /// not end the stream.
    fn narrow(&self) {
        let transition =
            self.narrowing
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
                    NARROW_IDLE => Some(NARROW_RUNNING),
                    NARROW_RUNNING => Some(NARROW_DIRTY),
                    _ => None,
                });
        if !matches!(transition, Ok(NARROW_IDLE)) {
            return;
        }

        let sender = self.sender.clone();
        let pane = self.pane.clone();
        let narrowing = Arc::clone(&self.narrowing);
        tokio::spawn(async move {
            loop {
                let _ = sender.watch_only(std::slice::from_ref(&pane)).await;
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
        });
    }

    /// Return the pane being watched.
    #[must_use]
    pub const fn pane(&self) -> &PaneId {
        &self.pane
    }

    /// Capture the pane's visible screen at an ordered point in this stream.
    ///
    /// `on_output` receives every unread chunk tmux ordered before the capture
    /// block. The callback owns any retention policy, so libtmux does not
    /// retain those chunks. It runs synchronously and should return promptly.
    ///
    /// Each chunk passed to `on_output` is consumed from this stream and is
    /// not repeated by [`Self::next_chunk`]. That remains true when this
    /// future is cancelled or returns an error: caller-owned storage keeps
    /// the prefix it already accepted.
    ///
    /// The visible screen and preceding output may overlap: the screen is
    /// tmux's rendered grid, while the callback receives the raw terminal
    /// byte stream that reached that grid.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn inspect(pane: &libtmux::Pane) -> Result<(), libtmux::Error> {
    /// let mut output = pane.stream_output().await?;
    /// let mut preceding = Vec::new();
    /// let visible = output
    ///     .snapshot(|chunk| preceding.extend_from_slice(chunk))
    ///     .await?;
    ///
    /// println!("{} visible lines after {} raw bytes", visible.len(), preceding.len());
    /// output.shutdown().await
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error when the connection closes, the command deadline
    /// elapses, or tmux refuses the capture, including when the pane vanished.
    pub async fn snapshot(
        &mut self,
        mut on_output: impl FnMut(&[u8]),
    ) -> Result<Vec<TmuxText>, Error> {
        self.boundary = self.boundary.wrapping_add(1);
        let boundary = Boundary(self.boundary);
        let command = Command::new("capture-pane")
            .arg("-p")
            .arg("-t")
            .arg(self.pane.to_string());
        let sender = self.sender.clone();
        let reply = sender.send_ordered(command, Some(boundary));
        tokio::pin!(reply);
        let mut answer = None;

        loop {
            let mut reached = false;
            tokio::select! {
                biased;
                outcome = reply.as_mut(), if answer.is_none() => {
                    match outcome {
                        Ok(block) => answer = Some(block),
                        Err(error) => return Err(error),
                    }
                }
                delivery = self.events.next_delivery() => {
                    match delivery {
                        Some(Delivery::Event(
                            Event::Output { pane, bytes }
                            | Event::ExtendedOutput { pane, bytes, .. },
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
                            self.closed = true;
                            if answer.is_none() {
                                reply.as_mut().await?;
                            }
                            return Err(Error::control_mode_closed());
                        }
                    }
                }
            }

            if !reached {
                continue;
            }
            let block = match answer.take() {
                Some(block) => block,
                None => reply.as_mut().await?,
            }
            .require_success("capture-pane")?;
            return Ok(block.output);
        }
    }

    /// Return the next chunk this pane wrote, or `None` once it stops.
    ///
    /// A chunk is what tmux chose to report at once, which is not a line and
    /// not a fixed size. Callers wanting lines should buffer.
    pub async fn next_chunk(&mut self) -> Option<Vec<u8>> {
        if self.closed {
            return None;
        }
        loop {
            let delivery = self.events.next_delivery().await;
            match delivery {
                Some(Delivery::Event(
                    Event::Output { pane, bytes } | Event::ExtendedOutput { pane, bytes, .. },
                )) if pane == self.pane => {
                    return Some(bytes);
                }
                Some(Delivery::Event(Event::Exit { .. })) | None => {
                    self.closed = true;
                    return None;
                }
                Some(Delivery::Event(event)) if event.may_have_added_a_pane() => self.narrow(),
                _ => {}
            }
        }
    }

    /// End the connection and report how it went.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection failed before it was closed.
    pub async fn shutdown(self) -> Result<(), Error> {
        drop(self.sender);
        self.events.shutdown().await
    }
}

impl Stream for PaneOutput {
    type Item = Vec<u8>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Vec<u8>>> {
        if self.closed {
            return Poll::Ready(None);
        }
        loop {
            match std::task::ready!(self.events.events.poll_recv(context)) {
                Some(Delivery::Event(
                    Event::Output { pane, bytes } | Event::ExtendedOutput { pane, bytes, .. },
                )) if pane == self.pane => {
                    return Poll::Ready(Some(bytes));
                }
                Some(Delivery::Event(Event::Exit { .. })) | None => {
                    self.closed = true;
                    return Poll::Ready(None);
                }
                Some(Delivery::Event(event)) => {
                    if event.may_have_added_a_pane() {
                        self.narrow();
                    }
                }
                Some(Delivery::Boundary(_)) => {}
            }
        }
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

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "control/lifecycle_tests.rs"]
mod lifecycle_tests;
