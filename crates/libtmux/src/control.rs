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

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_core::Stream;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::internal::process::PersistentChild;
use crate::limits::ControlLimits;
use crate::version::since::CONTROL_PANE_OFF;
use crate::{Command, Error, IdParseError, PaneId, Server, SessionId, TmuxText, WindowId};

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

        let mut child = server.spawn_control(session).await?;
        let Some(stdin) = child.take_stdin() else {
            let _ = child.terminate().await;
            return Err(Error::control_mode_pipes());
        };
        let Some(stdout) = child.take_stdout() else {
            let _ = child.terminate().await;
            return Err(Error::control_mode_pipes());
        };
        let core_stopped = child.stopped();

        let (commands, queue) = mpsc::channel(COMMAND_QUEUE);
        let (events, received) = mpsc::channel(EVENT_QUEUE);
        let (stop, stopped) = watch::channel(());
        let connection = Connection {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            limits,
            timeout: server.default_timeout(),
            line: Vec::new(),
            commands: queue,
            events,
            stopped,
            core_stopped,
            awaiting: ReplySlots::default(),
            pending: VecDeque::new(),
        };

        let (ready, mut opened) = oneshot::channel();
        let mut connection = tokio::spawn(connection.run(ready));
        tokio::select! {
            biased;
            result = &mut opened => {
                if result.is_err() {
                    return match connection.await {
                        Ok(Err(error)) => Err(error),
                        Ok(Ok(())) | Err(_) => Err(Error::control_mode_closed()),
                    };
                }
            }
            result = &mut connection => {
                return match result {
                    Ok(Err(error)) => Err(error),
                    Ok(Ok(())) | Err(_) => Err(Error::control_mode_closed()),
                };
            }
        }

        Ok(Self {
            sender: ControlSender {
                commands,
                timeout: server.default_timeout(),
                pane_off_is_safe,
            },
            events: ControlEvents {
                events: received,
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
    /// A control client is sent the output of *every* pane on the server. One
    /// pane running `yes` moves more than 20 MB in two seconds, and a client
    /// tmux judges five minutes behind is disconnected with `too far behind`,
    /// so discarding the unwanted panes on arrival is not enough.
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
    events: mpsc::Receiver<Event>,
    /// Ends the connection when this handle asks, or when it is dropped.
    stop: watch::Sender<()>,
    connection: tokio::task::JoinHandle<Result<(), Error>>,
}

impl ControlEvents {
    /// Return the next notification, or `None` once the connection closes.
    pub async fn next_event(&mut self) -> Option<Event> {
        self.events.recv().await
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
        self.events.poll_recv(context)
    }
}

/// How many commands may queue before a sender waits.
const COMMAND_QUEUE: usize = 16;

/// How many events may buffer before the connection stops reading tmux.
const EVENT_QUEUE: usize = 256;

/// How many events may be held while a reply is outstanding.
///
/// Reading continues while something is waiting for a reply, because the reply
/// arrives on the connection that would otherwise pause. That is bounded by
/// how long a reply takes, and tmux answers most commands at once -- but not
/// all. `run-shell` without `-b` answers its own block immediately and then
/// parks the queue for as long as its shell command runs, so the next command
/// sent is outstanding for that long and this end would hold events for the
/// duration. A ceiling turns that into a pause rather than a memory leak.
const HELD_WHILE_AWAITING: usize = EVENT_QUEUE * 8;

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
    /// Kept to re-narrow the subscription, not to send a caller's commands.
    ///
    /// tmux has no notification for a pane being created, so a pane that
    /// appears after the attach arrives unmuted; the event loop below repairs
    /// that when an event says the set of panes may have grown.
    sender: ControlSender,
    /// Whether a re-narrow is already in flight.
    ///
    /// Each one costs a `list-panes` round trip, and a burst of splits reports
    /// an event apiece.
    narrowing: Arc<AtomicBool>,
}

impl PaneOutput {
    pub(crate) fn new(pane: PaneId, events: ControlEvents, sender: ControlSender) -> Self {
        Self {
            pane,
            events,
            sender,
            narrowing: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Tell tmux again to send only this pane.
    ///
    /// Detached rather than awaited so [`Stream::poll_next`], which cannot
    /// await, repairs the subscription the same way [`Self::next_chunk`] does.
    /// A failure leaves the caller its own pane alongside noise, so it does
    /// not end the stream.
    fn narrow(&self) {
        if self.narrowing.swap(true, Ordering::AcqRel) {
            return;
        }

        let sender = self.sender.clone();
        let pane = self.pane.clone();
        let narrowing = Arc::clone(&self.narrowing);
        tokio::spawn(async move {
            let _ = sender.watch_only(&[pane]).await;
            narrowing.store(false, Ordering::Release);
        });
    }

    /// Return the pane being watched.
    #[must_use]
    pub const fn pane(&self) -> &PaneId {
        &self.pane
    }

    /// Return the next chunk this pane wrote, or `None` once it stops.
    ///
    /// A chunk is what tmux chose to report at once, which is not a line and
    /// not a fixed size. Callers wanting lines should buffer.
    pub async fn next_chunk(&mut self) -> Option<Vec<u8>> {
        loop {
            let event = self.events.next_event().await?;
            match event {
                Event::Output { pane, bytes } | Event::ExtendedOutput { pane, bytes, .. }
                    if pane == self.pane =>
                {
                    return Some(bytes);
                }
                Event::Exit { .. } => return None,
                event if event.may_have_added_a_pane() => self.narrow(),
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
        loop {
            match std::task::ready!(self.events.events.poll_recv(context)) {
                Some(Event::Output { pane, bytes } | Event::ExtendedOutput { pane, bytes, .. })
                    if pane == self.pane =>
                {
                    return Poll::Ready(Some(bytes));
                }
                Some(Event::Exit { .. }) | None => return Poll::Ready(None),
                Some(event) => {
                    if event.may_have_added_a_pane() {
                        self.narrow();
                    }
                }
            }
        }
    }
}

/// One command waiting for its result block.
#[derive(Debug)]
struct Request {
    line: String,
    deadline: Option<Instant>,
    result: oneshot::Sender<Result<BlockResult, Error>>,
    commit: oneshot::Sender<()>,
}

/// A request whose caller can no longer prevent the first write.
#[derive(Debug)]
struct CommittedRequest {
    line: String,
    deadline: Option<Instant>,
    result: oneshot::Sender<Result<BlockResult, Error>>,
}

impl Request {
    fn commit(self) -> Option<CommittedRequest> {
        let Self {
            line,
            deadline,
            result,
            commit,
        } = self;
        commit.send(()).ok()?;
        Some(CommittedRequest {
            line,
            deadline,
            result,
        })
    }
}

fn admit_request(request: Request, pending_events: usize) -> Option<Request> {
    if pending_events < HELD_WHILE_AWAITING {
        return Some(request);
    }

    let _ = request.result.send(Err(Error::control_mode_unread()));
    None
}

/// Reply ownership in the order tmux will answer it.
#[derive(Debug)]
enum ReplySlot {
    Live {
        result: oneshot::Sender<Result<BlockResult, Error>>,
        deadline: Option<Instant>,
    },
    /// Consume this block without giving it to a later caller.
    Tombstone { deadline: Option<Instant> },
}

impl ReplySlot {
    const fn deadline(&self) -> Option<Instant> {
        match self {
            Self::Live { deadline, .. } | Self::Tombstone { deadline } => *deadline,
        }
    }
}

/// Ordered reply slots, including blocks whose callers were refused.
#[derive(Debug, Default)]
struct ReplySlots {
    slots: VecDeque<ReplySlot>,
    live: usize,
    earliest: Option<Instant>,
}

impl ReplySlots {
    fn push(
        &mut self,
        result: oneshot::Sender<Result<BlockResult, Error>>,
        deadline: Option<Instant>,
    ) {
        self.earliest = earliest_deadline(self.earliest, deadline);
        if result.is_closed() {
            self.slots.push_back(ReplySlot::Tombstone { deadline });
        } else {
            self.slots.push_back(ReplySlot::Live { result, deadline });
            self.live += 1;
        }
    }

    const fn has_live(&self) -> bool {
        self.live != 0
    }

    fn has_slots(&self) -> bool {
        !self.slots.is_empty()
    }

    const fn earliest_deadline(&self) -> Option<Instant> {
        self.earliest
    }

    fn block_deadline(&self, timeout: Duration) -> Option<Instant> {
        if self.has_slots() {
            self.earliest
        } else {
            Instant::now().checked_add(timeout)
        }
    }

    fn refuse_live(&mut self) {
        for slot in &mut self.slots {
            let deadline = slot.deadline();
            let ReplySlot::Live { result, .. } =
                std::mem::replace(slot, ReplySlot::Tombstone { deadline })
            else {
                continue;
            };
            let _ = result.send(Err(Error::control_mode_unread()));
        }
        self.live = 0;
    }

    fn complete(&mut self, block: BlockResult) {
        let Some(slot) = self.slots.pop_front() else {
            return;
        };
        let deadline = slot.deadline();
        if let ReplySlot::Live { result, .. } = slot {
            self.live -= 1;
            let _ = result.send(Ok(block));
        }
        if deadline == self.earliest {
            self.earliest = self.slots.iter().filter_map(ReplySlot::deadline).min();
        }
    }

    fn fail_all(&mut self, mut reason: impl FnMut() -> Error) {
        while let Some(slot) = self.slots.pop_front() {
            if let ReplySlot::Live { result, .. } = slot {
                let _ = result.send(Err(reason()));
            }
        }
        self.live = 0;
        self.earliest = None;
    }
}

/// What one turn of the connection loop found to do.
enum Step {
    /// The watching half has room for one held event, or has gone.
    Deliver(bool),
    Read(Result<Option<Line>, Error>),
    Send(Option<Request>),
    /// The watching half asked to stop, or went away.
    Unwatched {
        asked: bool,
    },
    CoreStopped,
    TimedOut,
}

enum BlockRead {
    Complete(BlockResult),
    Stopped,
}

#[derive(Clone, Copy)]
enum TerminalError {
    Closed,
    Frame(&'static str, usize),
    Shutdown,
    TimedOut,
}

impl TerminalError {
    fn build(self, child: &PersistentChild) -> Error {
        match self {
            Self::Closed => Error::control_mode_closed(),
            Self::Frame(frame, limit) => Error::control_mode_frame_too_large(frame, limit),
            Self::Shutdown => child.shutdown_error(),
            Self::TimedOut => Error::control_mode_timeout(),
        }
    }
}

/// The task that owns the pipes and multiplexes both directions.
struct Connection {
    child: PersistentChild,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// What one line and one block may accumulate before this gives up.
    limits: ControlLimits,
    timeout: Duration,
    /// Bytes of a line that is not complete yet.
    ///
    /// This outlives one read because a cancelled read leaves what it got
    /// here, and the next read continues from it.
    line: Vec<u8>,
    commands: mpsc::Receiver<Request>,
    events: mpsc::Sender<Event>,
    /// Resolves when the watching half asks to stop, or is dropped.
    stopped: watch::Receiver<()>,
    core_stopped: watch::Receiver<bool>,
    /// Commands whose result block has not arrived yet.
    ///
    /// tmux answers in order and blocks do not nest, so the front of this
    /// queue owns the next block that completes.
    awaiting: ReplySlots,
    /// Events tmux has reported that the caller has not taken yet.
    ///
    /// The reader puts an event here rather than waiting for the caller to
    /// have room, because waiting would stop it reading the connection, and
    /// the connection is where a caller's reply comes from.
    pending: VecDeque<Event>,
}

impl Connection {
    async fn run(mut self, mut ready: oneshot::Sender<()>) -> Result<(), Error> {
        let opening_deadline = Instant::now().checked_add(self.timeout);
        let outcome = match self
            .discard_opening_block(&mut ready, opening_deadline)
            .await
        {
            Ok(true) if ready.send(()).is_ok() => self.serve().await,
            Ok(_) => Ok(()),
            Err(error) => Err(error),
        };

        // Whatever is still waiting will never be answered. It is told why
        // where the reason is more specific than "closed": a caller who blew
        // a frame budget can raise it, where one who merely lost the
        // connection can only reconnect.
        let reason = match &outcome {
            Err(Error::ControlModeFrameTooLarge { frame, limit }) => {
                TerminalError::Frame(frame, *limit)
            }
            Err(Error::ControlMode {
                kind: crate::ControlModeErrorKind::TimedOut,
                ..
            }) => TerminalError::TimedOut,
            Err(Error::ExecutorShutdown { .. }) => TerminalError::Shutdown,
            _ => TerminalError::Closed,
        };
        let child = &self.child;
        self.awaiting.fail_all(|| reason.build(child));
        while let Ok(request) = self.commands.try_recv() {
            let _ = request.result.send(Err(reason.build(child)));
        }
        drop(self.stdin);
        let cleanup = self.child.terminate().await;

        match outcome {
            Err(error) => Err(error),
            Ok(()) => cleanup,
        }
    }

    async fn serve(&mut self) -> Result<(), Error> {
        // The connection outlives either half on its own: a caller who only
        // watches drops the sender, and a caller who only sends drops the
        // events. It ends when both are gone, when the watcher asks, or when
        // tmux hangs up.
        let mut sending = true;
        let mut watching = true;

        while sending || watching {
            if self.awaiting.has_live() && self.pending.len() >= HELD_WHILE_AWAITING {
                self.awaiting.refuse_live();
            }

            let held_back = !self.awaiting.has_live() && self.pending.len() >= EVENT_QUEUE;
            let reply_deadline = self.awaiting.earliest_deadline();

            let step = tokio::select! {
                line = read_line(&mut self.stdout, &mut self.line, self.limits.max_line_bytes),
                    if !held_back => Step::Read(line),
                room = self.events.reserve(), if !self.pending.is_empty() => Step::Deliver(room.is_ok()),
                request = self.commands.recv(), if sending => Step::Send(request),
                asked = self.stopped.changed(), if watching => Step::Unwatched {
                    asked: asked.is_ok(),
                },
                () = cancellation_requested(&mut self.core_stopped) => Step::CoreStopped,
                () = deadline_elapsed(reply_deadline), if self.awaiting.has_slots() => Step::TimedOut,
            };

            match step {
                Step::Read(Err(error)) => return Err(error),
                // tmux hung up, or the watcher asked to stop. Either ends the
                // connection whatever the other half is doing.
                Step::Read(Ok(None)) | Step::Unwatched { asked: true } => {
                    return Ok(());
                }
                Step::CoreStopped => return Err(self.child.shutdown_error()),
                Step::TimedOut => return Err(Error::control_mode_timeout()),
                Step::Read(Ok(Some(line))) => {
                    if !self.dispatch(line, &mut watching).await? {
                        return Ok(());
                    }
                }
                Step::Send(Some(request)) => {
                    if request
                        .deadline
                        .is_some_and(|deadline| deadline <= Instant::now())
                    {
                        let _ = request
                            .result
                            .send(Err(Error::control_mode_dispatch_timeout()));
                        continue;
                    }
                    let Some(request) = admit_request(request, self.pending.len()) else {
                        continue;
                    };
                    let Some(request) = request.commit() else {
                        continue;
                    };
                    let write_deadline = earliest_deadline(reply_deadline, request.deadline);
                    let write = write_line(&mut self.stdin, &request.line);
                    tokio::pin!(write);
                    loop {
                        let result = tokio::select! {
                            biased;
                            () = cancellation_requested(&mut self.core_stopped) => {
                                let _ = request.result.send(Err(self.child.shutdown_error()));
                                return Err(self.child.shutdown_error());
                            }
                            changed = self.stopped.changed(), if watching => {
                                if changed.is_ok() {
                                    let _ = request.result.send(Err(Error::control_mode_closed()));
                                    return Ok(());
                                }
                                watching = false;
                                continue;
                            }
                            () = deadline_elapsed(write_deadline) => {
                                let _ = request.result.send(Err(Error::control_mode_timeout()));
                                return Err(Error::control_mode_timeout());
                            }
                            result = &mut write => result,
                        };
                        if let Err(error) = result {
                            let _ = request.result.send(Err(Error::control_mode_closed()));
                            return Err(error);
                        }
                        break;
                    }
                    self.awaiting.push(request.result, request.deadline);
                }
                // The caller took one, so the next one can go.
                Step::Deliver(true) => {
                    if let Some(event) = self.pending.pop_front() {
                        let _ = self.events.try_send(event);
                    }
                }
                // Nobody is watching any more. What is held becomes
                // unreachable rather than undelivered, and the connection
                // carries on for whoever is still sending.
                Step::Deliver(false) => self.pending.clear(),
                // Every sender is gone, so no further commands can arrive.
                Step::Send(None) => sending = false,
                // The watching handle was dropped rather than asked to stop,
                // which leaves any sender still working.
                Step::Unwatched { asked: false } => watching = false,
            }
        }

        Ok(())
    }

    /// Consume the block tmux answers an attach with.
    ///
    /// tmux writes this once the client is attached, before it has read
    /// anything from this end, so it replies to nothing. Correlation is by
    /// arrival order, and leaving this block to the serving loop would hand it
    /// to the first command's caller as that command's result -- an empty
    /// success, whatever the command was.
    async fn discard_opening_block(
        &mut self,
        ready: &mut oneshot::Sender<()>,
        deadline: Option<Instant>,
    ) -> Result<bool, Error> {
        loop {
            let held_back = self.events.capacity() == 0;
            let line = tokio::select! {
                biased;
                () = ready.closed() => return Ok(false),
                () = cancellation_requested(&mut self.core_stopped) => {
                    return Err(self.child.shutdown_error());
                }
                () = deadline_elapsed(deadline) => {
                    return Err(Error::control_mode_timeout());
                }
                line = read_line(
                    &mut self.stdout,
                    &mut self.line,
                    self.limits.max_line_bytes,
                ), if !held_back => line?,
            };
            match line {
                Some(Line::BlockStart(number)) => {
                    return match self.read_opening_block(number, ready, deadline).await? {
                        Some(true) => Ok(true),
                        Some(false) => Err(Error::control_mode_closed()),
                        None => Ok(false),
                    };
                }
                Some(Line::Event(exit @ Event::Exit { .. })) => {
                    let _ = self.events.try_send(exit);
                    return Err(Error::control_mode_closed());
                }
                Some(Line::Event(event)) => {
                    let _ = self.events.try_send(event);
                }
                Some(Line::Text(_) | Line::BlockEnd { .. }) => {}
                None => return Err(Error::control_mode_closed()),
            }
        }
    }

    async fn read_opening_block(
        &mut self,
        number: u64,
        ready: &mut oneshot::Sender<()>,
        deadline: Option<Instant>,
    ) -> Result<Option<bool>, Error> {
        let mut accumulated = 0usize;
        loop {
            let line = tokio::select! {
                biased;
                () = ready.closed() => return Ok(None),
                () = cancellation_requested(&mut self.core_stopped) => {
                    return Err(self.child.shutdown_error());
                }
                () = deadline_elapsed(deadline) => {
                    return Err(Error::control_mode_timeout());
                }
                line = read_line_within(
                    &mut self.stdout,
                    &mut self.line,
                    self.limits.max_line_bytes,
                    Some(number),
                ) => line?,
            };
            match line {
                Some(Line::BlockEnd {
                    number: end,
                    succeeded,
                }) if end == number => return Ok(Some(succeeded)),
                Some(Line::Text(text)) => {
                    accumulated = accumulated.saturating_add(text.as_bytes().len());
                    if accumulated > self.limits.max_block_bytes {
                        return Err(Error::control_mode_frame_too_large(
                            "block",
                            self.limits.max_block_bytes,
                        ));
                    }
                }
                Some(Line::Event(_) | Line::BlockStart(_) | Line::BlockEnd { .. }) => {}
                None => return Err(Error::control_mode_closed()),
            }
        }
    }

    /// Act on one protocol line, reporting whether to keep reading.
    async fn dispatch(&mut self, line: Line, watching: &mut bool) -> Result<bool, Error> {
        match line {
            Line::BlockStart(number) => {
                let deadline = self.awaiting.block_deadline(self.timeout);
                match self.read_block(number, deadline, watching).await? {
                    BlockRead::Complete(block) => {
                        self.awaiting.complete(block);
                        Ok(true)
                    }
                    BlockRead::Stopped => Ok(false),
                }
            }
            Line::Event(exit @ Event::Exit { .. }) => {
                self.report(exit);
                Ok(false)
            }
            Line::Event(event) => {
                self.report(event);
                Ok(true)
            }
            // A block terminator with no block open, or output outside one.
            Line::Text(_) | Line::BlockEnd { .. } => Ok(true),
        }
    }

    /// Hand an event to the receiver, if one is still listening.
    ///
    /// A receiver that has gone away is not a reason to stop: commands may
    /// still be in flight, and a caller who only sends is a valid caller.
    fn report(&mut self, event: Event) {
        // Anything already held goes first. The channel can drain between one
        // event and the next, so handing this one straight over while older
        // ones wait would deliver them out of order, and a pane's output is a
        // byte stream where that reads exactly like loss.
        if !self.pending.is_empty() {
            self.pending.push_back(event);
            return;
        }

        // Never `send().await`: this runs on the task that reads the
        // connection, so waiting here stops the reads that a reply arrives on.
        //
        // Anything but a full channel is finished with here. A receiver that
        // has gone away is not a reason to stop, because commands may still be
        // in flight and a caller who only sends is a valid caller.
        let Err(mpsc::error::TrySendError::Full(event)) = self.events.try_send(event) else {
            return;
        };

        self.pending.push_back(event);
    }

    /// Read to the end of a block that has already begun.
    async fn read_block(
        &mut self,
        number: u64,
        deadline: Option<Instant>,
        watching: &mut bool,
    ) -> Result<BlockRead, Error> {
        let mut output = Vec::new();
        let mut accumulated = 0usize;
        loop {
            let line = tokio::select! {
                biased;
                () = cancellation_requested(&mut self.core_stopped) => {
                    return Err(self.child.shutdown_error());
                }
                changed = self.stopped.changed(), if *watching => {
                    if changed.is_ok() {
                        return Ok(BlockRead::Stopped);
                    }
                    *watching = false;
                    continue;
                }
                () = deadline_elapsed(deadline) => {
                    return Err(Error::control_mode_timeout());
                }
                line = read_line_within(
                    &mut self.stdout,
                    &mut self.line,
                    self.limits.max_line_bytes,
                    Some(number),
                ) => line?,
            };
            match line {
                Some(Line::BlockEnd {
                    number: end,
                    succeeded,
                }) if end == number => {
                    return Ok(BlockRead::Complete(BlockResult {
                        number,
                        succeeded,
                        output,
                        sensitive_input: false,
                    }));
                }
                Some(Line::Text(text)) => {
                    // A block whose `%end` never arrives grows without bound,
                    // and unlike a line it can do so one valid line at a time.
                    accumulated = accumulated.saturating_add(text.as_bytes().len());
                    if accumulated > self.limits.max_block_bytes {
                        return Err(Error::control_mode_frame_too_large(
                            "block",
                            self.limits.max_block_bytes,
                        ));
                    }
                    output.push(text);
                }
                // Inside a block every other line is output, so once one is
                // open its reply never waits on a caller draining events.
                // Only once it is open: the `%begin` that opens it is read by
                // the loop above, which does report events.
                Some(Line::Event(_) | Line::BlockStart(_) | Line::BlockEnd { .. }) => {}
                None => return Err(Error::control_mode_closed()),
            }
        }
    }
}

async fn cancellation_requested(stopped: &mut watch::Receiver<bool>) {
    loop {
        if *stopped.borrow() {
            return;
        }
        if stopped.changed().await.is_err() {
            return;
        }
    }
}

async fn deadline_elapsed(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn earliest_deadline(left: Option<Instant>, right: Option<Instant>) -> Option<Instant> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}

/// Read and classify one protocol line.
///
/// `pending` carries a line across calls. `read_until` appends what it read
/// before it was cancelled, which is what makes this usable in `select!` --
/// `read_line` would lose those bytes, and would also reject the pane output
/// that is not UTF-8.
async fn read_line(
    stdout: &mut BufReader<ChildStdout>,
    pending: &mut Vec<u8>,
    limit: usize,
) -> Result<Option<Line>, Error> {
    read_line_within(stdout, pending, limit, None).await
}

/// Read one line, classifying it for the block it arrived in.
///
/// `within` names the open block, if any. tmux(1) guarantees a notification
/// never appears inside an output block, so every line but its own terminator
/// is command output -- including one that looks like a notification, which
/// is what `list-panes -F '#{pane_id}'` produces for every row.
async fn read_line_within(
    stdout: &mut BufReader<ChildStdout>,
    pending: &mut Vec<u8>,
    limit: usize,
    within: Option<u64>,
) -> Result<Option<Line>, Error> {
    let read = stdout
        .read_until(b'\n', pending)
        .await
        .map_err(Error::control_mode)?;
    if read == 0 && pending.is_empty() {
        return Ok(None);
    }
    // A line that never ends is the one shape a framed protocol cannot
    // recover from by reading further, so it stops here rather than growing.
    // The connection is not resynchronizable afterwards: the caller reopens.
    if pending.len() > limit {
        pending.clear();
        return Err(Error::control_mode_frame_too_large("line", limit));
    }

    // read_until stops at the newline or at end of input, so what is left
    // without one is the last line tmux managed to write.
    let bytes = pending.strip_suffix(b"\n").unwrap_or(pending);
    let line = match within {
        Some(number) => Line::parse_within_block(bytes, number),
        None => Line::parse(bytes),
    };
    pending.clear();

    Ok(Some(line))
}

/// Write one command line to the connection.
async fn write_line(stdin: &mut ChildStdin, line: &str) -> Result<(), Error> {
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(Error::control_mode)?;
    stdin.write_all(b"\n").await.map_err(Error::control_mode)?;
    stdin.flush().await.map_err(Error::control_mode)?;

    Ok(())
}

/// One classified line of the control-mode protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Line {
    BlockStart(u64),
    BlockEnd { number: u64, succeeded: bool },
    Event(Event),
    Text(TmuxText),
}

impl Line {
    /// Classify a line arriving inside the block numbered `number`.
    ///
    /// Only that block's own terminator is structure. Everything else is
    /// output, however much it resembles a notification.
    fn parse_within_block(line: &[u8], number: u64) -> Self {
        match Self::parse(line) {
            end @ Self::BlockEnd { number: found, .. } if found == number => end,
            _ => Self::Text(TmuxText::from_bytes(line)),
        }
    }

    fn parse(line: &[u8]) -> Self {
        let text = || Self::Text(TmuxText::from_bytes(line));

        let Some(rest) = line.strip_prefix(b"%") else {
            return text();
        };
        let (name, arguments) = split_once(rest, b' ');
        // Every notification tmux names is ASCII. Anything else is a line
        // that happens to start with a percent, not a notification.
        let Ok(name) = std::str::from_utf8(name) else {
            return text();
        };

        // A recognized notification that will not parse answers `Text` rather
        // than falling through, so a malformed line is never reported as an
        // unmodelled one.
        Self::framing(name, arguments, line)
            .or_else(|| Self::about_output(name, arguments, line))
            .or_else(|| Self::about_a_session(name, arguments, line))
            .or_else(|| Self::about_a_window(name, arguments, line))
            .or_else(|| Self::about_the_server(name, arguments, line))
            .unwrap_or_else(|| {
                Self::Event(Event::Other {
                    name: name.to_owned(),
                    rest: TmuxText::from_bytes(arguments),
                })
            })
    }

    /// `%begin`, `%end` and `%error`, which bracket a command's result.
    ///
    /// Each carries a timestamp, a number, and flags. The number correlates a
    /// result with its command; a header without one is text, because guessing
    /// would hand the result to the wrong caller.
    fn framing(name: &str, arguments: &[u8], line: &[u8]) -> Option<Self> {
        if !matches!(name, "begin" | "end" | "error") {
            return None;
        }

        let number = std::str::from_utf8(arguments).ok().and_then(|arguments| {
            arguments
                .split_whitespace()
                .nth(1)
                .and_then(|value| value.parse().ok())
        });

        Some(match (name, number) {
            ("begin", Some(number)) => Self::BlockStart(number),
            (_, Some(number)) => Self::BlockEnd {
                number,
                succeeded: name == "end",
            },
            (_, None) => Self::Text(TmuxText::from_bytes(line)),
        })
    }

    /// What a pane wrote, and the flow control around it.
    fn about_output(name: &str, arguments: &[u8], line: &[u8]) -> Option<Self> {
        let text = || Self::Text(TmuxText::from_bytes(line));

        Some(match name {
            "output" => {
                let (pane, bytes) = split_once(arguments, b' ');
                parsed(pane).map_or_else(text, |pane| {
                    Self::Event(Event::Output {
                        pane,
                        bytes: unescape_output(bytes),
                    })
                })
            }
            // `%extended-output %1 42 : data`. The `:` separator is tmux's,
            // not a delimiter that could occur inside the age.
            "extended-output" => {
                let (pane, rest) = split_once(arguments, b' ');
                let (age, rest) = split_once(rest, b' ');
                let bytes = rest.strip_prefix(b": ").unwrap_or(rest);
                match (parsed(pane), parsed::<u64>(age)) {
                    (Some(pane), Some(age)) => Self::Event(Event::ExtendedOutput {
                        pane,
                        age: Duration::from_millis(age),
                        bytes: unescape_output(bytes),
                    }),
                    _ => text(),
                }
            }
            "pause" => pane_event(arguments, text, |pane| Event::Paused { pane }),
            "continue" => pane_event(arguments, text, |pane| Event::Continued { pane }),
            "pane-mode-changed" => {
                pane_event(arguments, text, |pane| Event::PaneModeChanged { pane })
            }
            _ => return None,
        })
    }

    /// Notifications naming a session.
    fn about_a_session(name: &str, arguments: &[u8], line: &[u8]) -> Option<Self> {
        let text = || Self::Text(TmuxText::from_bytes(line));

        Some(match name {
            "session-changed" => {
                let (session, _) = split_once(arguments, b' ');
                parsed(session).map_or_else(text, |session| {
                    Self::Event(Event::SessionChanged { session })
                })
            }
            "session-renamed" => {
                let (session, new_name) = split_once(arguments, b' ');
                parsed(session).map_or_else(text, |session| {
                    Self::Event(Event::SessionRenamed {
                        session,
                        name: TmuxText::from_bytes(new_name),
                    })
                })
            }
            "session-window-changed" => {
                let (session, window) = split_once(arguments, b' ');
                match (parsed(session), parsed(window)) {
                    (Some(session), Some(window)) => {
                        Self::Event(Event::SessionWindowChanged { session, window })
                    }
                    _ => text(),
                }
            }
            "sessions-changed" => Self::Event(Event::SessionsChanged),
            _ => return None,
        })
    }

    /// Notifications naming a window, linked into the attached session or not.
    fn about_a_window(name: &str, arguments: &[u8], line: &[u8]) -> Option<Self> {
        let text = || Self::Text(TmuxText::from_bytes(line));

        Some(match name {
            "window-add" => window_event(arguments, text, |window| Event::WindowAdded { window }),
            "window-close" => {
                window_event(arguments, text, |window| Event::WindowClosed { window })
            }
            "unlinked-window-add" => window_event(arguments, text, |window| {
                Event::UnlinkedWindowAdded { window }
            }),
            "unlinked-window-close" => window_event(arguments, text, |window| {
                Event::UnlinkedWindowClosed { window }
            }),
            "window-renamed" | "unlinked-window-renamed" => {
                let (window, new_name) = split_once(arguments, b' ');
                parsed(window).map_or_else(text, |window| {
                    let new_name = TmuxText::from_bytes(new_name);
                    Self::Event(if name == "window-renamed" {
                        Event::WindowRenamed {
                            window,
                            name: new_name,
                        }
                    } else {
                        Event::UnlinkedWindowRenamed {
                            window,
                            name: new_name,
                        }
                    })
                })
            }
            "window-pane-changed" => {
                let (window, pane) = split_once(arguments, b' ');
                match (parsed(window), parsed(pane)) {
                    (Some(window), Some(pane)) => {
                        Self::Event(Event::WindowPaneChanged { window, pane })
                    }
                    _ => text(),
                }
            }
            // Built from a format template rather than a printf, so it carries
            // whatever `#{window_raw_flags}` expanded to -- possibly nothing.
            "layout-change" => {
                let (window, rest) = split_once(arguments, b' ');
                let (layout, rest) = split_once(rest, b' ');
                let (visible_layout, flags) = split_once(rest, b' ');
                parsed(window).map_or_else(text, |window| {
                    Self::Event(Event::LayoutChanged {
                        window,
                        layout: TmuxText::from_bytes(layout),
                        visible_layout: TmuxText::from_bytes(visible_layout),
                        flags: TmuxText::from_bytes(flags),
                    })
                })
            }
            _ => return None,
        })
    }

    /// Notifications about clients, buffers, subscriptions, and the server.
    fn about_the_server(name: &str, arguments: &[u8], line: &[u8]) -> Option<Self> {
        let text = || Self::Text(TmuxText::from_bytes(line));

        Some(match name {
            "client-detached" => Self::Event(Event::ClientDetached {
                client: TmuxText::from_bytes(arguments),
            }),
            "client-session-changed" => {
                let (client, rest) = split_once(arguments, b' ');
                let (session, session_name) = split_once(rest, b' ');
                parsed(session).map_or_else(text, |session| {
                    Self::Event(Event::ClientSessionChanged {
                        client: TmuxText::from_bytes(client),
                        session,
                        name: TmuxText::from_bytes(session_name),
                    })
                })
            }
            "paste-buffer-changed" => Self::Event(Event::PasteBufferChanged {
                name: TmuxText::from_bytes(arguments),
            }),
            "paste-buffer-deleted" => Self::Event(Event::PasteBufferDeleted {
                name: TmuxText::from_bytes(arguments),
            }),
            "subscription-changed" => Self::subscription(arguments).unwrap_or_else(text),
            "config-error" => Self::Event(Event::ConfigError {
                message: TmuxText::from_bytes(arguments),
            }),
            "message" => Self::Event(Event::Message {
                message: TmuxText::from_bytes(arguments),
            }),
            // A bare `%exit` is an ordinary shutdown; tmux adds a reason when
            // it has one, such as falling too far behind.
            "exit" => Self::Event(Event::Exit {
                reason: (!arguments.is_empty()).then(|| TmuxText::from_bytes(arguments)),
            }),
            _ => return None,
        })
    }

    /// Parse `%subscription-changed <name> $0 @1 2 %3 : <value>`.
    ///
    /// tmux writes `-` for each of window, index and pane when the
    /// subscription is not that specific, so an absent field is a real answer
    /// rather than a parse failure.
    fn subscription(arguments: &[u8]) -> Option<Self> {
        let (name, rest) = split_once(arguments, b' ');
        let (session, rest) = split_once(rest, b' ');
        let (window, rest) = split_once(rest, b' ');
        let (index, rest) = split_once(rest, b' ');
        let (pane, rest) = split_once(rest, b' ');

        Some(Self::Event(Event::SubscriptionChanged {
            name: TmuxText::from_bytes(name),
            session: parsed(session)?,
            window: named(window),
            index: named(index),
            pane: named(pane),
            value: TmuxText::from_bytes(rest.strip_prefix(b": ").unwrap_or(rest)),
        }))
    }
}

/// Parse a subscription field that tmux writes as `-` when it names nothing.
fn named<T: std::str::FromStr>(field: &[u8]) -> Option<T> {
    if field == b"-" {
        return None;
    }
    parsed(field)
}

/// Parse an ASCII field into whatever the caller is collecting.
///
/// Every field tmux puts in a notification is ASCII, so anything that is not
/// is a line which merely begins with a percent.
fn parsed<T: std::str::FromStr>(field: &[u8]) -> Option<T> {
    std::str::from_utf8(field).ok()?.parse().ok()
}

/// Build a notification whose only argument is a pane id.
fn pane_event(
    arguments: &[u8],
    text: impl FnOnce() -> Line,
    build: impl FnOnce(PaneId) -> Event,
) -> Line {
    let (pane, _) = split_once(arguments, b' ');
    parsed(pane).map_or_else(text, |pane| Line::Event(build(pane)))
}

/// Build a notification whose only argument is a window id.
fn window_event(
    arguments: &[u8],
    text: impl FnOnce() -> Line,
    build: impl FnOnce(WindowId) -> Event,
) -> Line {
    let (window, _) = split_once(arguments, b' ');
    parsed(window).map_or_else(text, |window| Line::Event(build(window)))
}

/// Split at the first occurrence of `byte`, which is not kept.
fn split_once(bytes: &[u8], byte: u8) -> (&[u8], &[u8]) {
    bytes
        .iter()
        .position(|found| *found == byte)
        .map_or((bytes, [].as_slice()), |index| {
            (&bytes[..index], &bytes[index + 1..])
        })
}

/// Undo the escaping tmux applies to `%output`.
///
/// tmux writes a byte below `0x20` as `\ooo` and a backslash as `\\`, and
/// leaves everything else alone -- so a pane emitting Latin-1 or binary
/// produces a line that is not UTF-8. Anything else after a backslash is not
/// an escape tmux produces, so it is kept as written rather than guessed at.
fn unescape_output(source: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(source.len());
    let mut index = 0;

    while index < source.len() {
        if source[index] != b'\\' {
            bytes.push(source[index]);
            index += 1;
            continue;
        }

        match source.get(index + 1..index + 4) {
            Some(digits) if digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) => {
                let value = digits
                    .iter()
                    .fold(0_u32, |value, digit| value * 8 + u32::from(digit - b'0'));
                // Three octal digits can exceed one byte; tmux never emits
                // that, and truncating would corrupt rather than refuse.
                if let Ok(byte) = u8::try_from(value) {
                    bytes.push(byte);
                    index += 4;
                    continue;
                }
                bytes.push(source[index]);
                index += 1;
            }
            _ => {
                if source.get(index + 1) == Some(&b'\\') {
                    bytes.push(b'\\');
                    index += 2;
                } else {
                    bytes.push(source[index]);
                    index += 1;
                }
            }
        }
    }

    bytes
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
mod tests {

    use std::time::Duration;

    use tokio::sync::{mpsc, oneshot};

    use super::{
        BlockResult, ControlSender, Event, HELD_WHILE_AWAITING, Line, ReplySlot, ReplySlots,
        Request, admit_request, decode_watched_pane_id, unescape_output,
    };
    use crate::{
        Command, ControlModeErrorKind, Error, ErrorKind, PaneId, SessionId, TmuxText, WindowId,
    };

    fn reply(number: u64) -> BlockResult {
        BlockResult {
            number,
            succeeded: true,
            output: Vec::new(),
            sensitive_input: false,
        }
    }

    fn request() -> (Request, oneshot::Receiver<Result<BlockResult, Error>>) {
        let (result, answer) = oneshot::channel();
        let (commit, _commitment) = oneshot::channel();
        (
            Request {
                line: String::new(),
                deadline: None,
                commit,
                result,
            },
            answer,
        )
    }

    fn sender(commands: mpsc::Sender<Request>, timeout: Duration) -> ControlSender {
        ControlSender {
            commands,
            timeout,
            pane_off_is_safe: true,
        }
    }

    fn refused(mut answer: oneshot::Receiver<Result<BlockResult, Error>>) -> Error {
        answer
            .try_recv()
            .expect("the request is answered")
            .expect_err("the request is refused")
    }

    #[test]
    fn block_refusal_classification_withholds_sensitive_output() {
        assert!(reply(1).refusal_for("display-message").is_none());

        let secret = "sentinel-control-refusal";
        let block = BlockResult {
            number: 2,
            succeeded: false,
            output: vec![TmuxText::from(secret)],
            sensitive_input: true,
        };
        let error = block
            .refusal_for("display-message")
            .expect("an error block is a refusal");
        let diagnostic = format!("{error:?} {error}");
        assert!(matches!(error, Error::CommandFailed { .. }));
        assert!(!diagnostic.contains(secret), "{diagnostic}");
    }

    #[test]
    fn watch_only_rejects_unreadable_pane_ids_without_echoing_them() {
        for line in [
            TmuxText::from("sentinel-not-a-pane-id"),
            TmuxText::from_bytes([0xff]),
        ] {
            let error = decode_watched_pane_id(&line).expect_err("pane id is unreadable");
            let diagnostic = format!("{error:?} {error}");
            assert_eq!(error.kind(), ErrorKind::Decode);
            assert!(!diagnostic.contains("sentinel"), "{diagnostic}");
        }
    }

    #[tokio::test]
    async fn watch_only_marks_a_transport_failure_after_its_first_mute() {
        let (commands, mut requests) = mpsc::channel(4);
        let sender = ControlSender {
            commands,
            timeout: Duration::from_secs(1),
            pane_off_is_safe: true,
        };
        let watch = tokio::spawn(async move { sender.watch_only(&[]).await });

        let listing = requests.recv().await.expect("list-panes request");
        assert!(listing.line.starts_with("list-panes "));
        listing
            .result
            .send(Ok(BlockResult {
                number: 1,
                succeeded: true,
                output: vec![TmuxText::from_bytes(*b"%1"), TmuxText::from_bytes(*b"%2")],
                sensitive_input: false,
            }))
            .expect("watch is waiting for the listing");

        let first_mute = requests.recv().await.expect("first mute request");
        assert!(first_mute.line.contains("%1:off"));
        first_mute
            .result
            .send(Ok(reply(2)))
            .expect("watch is waiting for the first mute");

        let second_mute = requests.recv().await.expect("second mute request");
        assert!(second_mute.line.contains("%2:off"));
        second_mute
            .result
            .send(Err(Error::Overloaded {
                request_id: 11,
                command: Command::new("refresh-client").summary(),
                in_flight: 1,
            }))
            .expect("watch is waiting for the second mute");

        let error = watch
            .await
            .expect("watch task does not panic")
            .expect_err("the second mute fails");
        assert!(matches!(
            error,
            Error::AfterEffect { operation: "watch-only", source }
                if source.kind() == ErrorKind::Refused && source.is_transient()
        ));
    }

    #[tokio::test]
    async fn watch_only_refuses_a_failed_listing_before_muting_any_pane() {
        let (commands, mut requests) = mpsc::channel(2);
        let sender = ControlSender {
            commands,
            timeout: Duration::from_secs(1),
            pane_off_is_safe: true,
        };
        let watch = tokio::spawn(async move { sender.watch_only(&[]).await });

        let listing = requests.recv().await.expect("list-panes request");
        listing
            .result
            .send(Ok(BlockResult {
                number: 1,
                succeeded: false,
                output: vec![TmuxText::from_bytes(*b"listing refused")],
                sensitive_input: false,
            }))
            .expect("watch is waiting for the listing");

        let error = watch
            .await
            .expect("watch task does not panic")
            .expect_err("a failed listing is not pane data");
        assert_eq!(error.kind(), ErrorKind::Refused);
        assert!(!matches!(error, Error::AfterEffect { .. }));
        assert!(requests.try_recv().is_err(), "no mute was dispatched");
    }

    #[tokio::test]
    async fn mute_pane_reports_a_control_error_block() {
        let (commands, mut requests) = mpsc::channel(1);
        let sender = ControlSender {
            commands,
            timeout: Duration::from_secs(1),
            pane_off_is_safe: true,
        };
        let pane: PaneId = "%1".parse().expect("a pane id");
        let mute = tokio::spawn(async move { sender.mute_pane(&pane).await });

        let request = requests.recv().await.expect("mute request");
        request
            .result
            .send(Ok(BlockResult {
                number: 1,
                succeeded: false,
                output: vec![TmuxText::from_bytes(*b"mute refused")],
                sensitive_input: false,
            }))
            .expect("mute is waiting for its block");

        let error = mute
            .await
            .expect("mute task does not panic")
            .expect_err("an error block is not success");
        assert_eq!(error.kind(), ErrorKind::Refused);
    }

    #[test]
    fn a_refused_reply_keeps_the_next_reply_aligned() {
        let mut replies = ReplySlots::default();
        let (b_request, b_reply) = request();
        replies.push(b_request.result, None);

        replies.refuse_live();
        let refused = refused(b_reply);
        assert_eq!(
            refused.kind(),
            ErrorKind::Refused,
            "B reports the unread-event cutoff",
        );
        assert!(
            !refused.is_transient(),
            "this command crossed the write boundary before it was refused",
        );
        assert!(!replies.has_live(), "event reading may pause");

        let (c_request, mut c_reply) = request();
        let c_request = admit_request(c_request, HELD_WHILE_AWAITING - 1)
            .expect("C is admitted after the caller drains below the limit");
        replies.push(c_request.result, None);
        assert!(replies.has_live(), "C keeps reply reading unpaused");
        replies.complete(reply(2));
        assert!(
            matches!(c_reply.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
            "B's block is discarded rather than answering C",
        );

        replies.complete(reply(3));
        assert_eq!(
            c_reply
                .try_recv()
                .expect("C is answered")
                .expect("C succeeds")
                .number(),
            3,
        );
    }

    #[tokio::test]
    async fn queue_wait_counts_toward_the_command_deadline() {
        let (commands, mut requests) = mpsc::channel(1);
        let sender = sender(commands.clone(), Duration::from_millis(20));
        let (occupant, _answer) = request();
        commands
            .try_send(occupant)
            .expect("the command queue has its one slot filled");

        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            sender.send(Command::new("list-sessions")),
        )
        .await
        .expect("the sender applies its own deadline");
        let error = outcome.expect_err("the full queue exceeds the deadline");
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert!(
            matches!(
                &error,
                Error::ControlMode {
                    kind: ControlModeErrorKind::DispatchTimedOut,
                    ..
                }
            ),
            "the command did not reach the write boundary",
        );
        assert!(
            error.is_transient(),
            "the unwritten command is safe to retry"
        );

        let _occupant = requests.recv().await.expect("the first request remains");
        assert!(
            requests.try_recv().is_err(),
            "the expired request never enters the queue",
        );
    }

    #[tokio::test]
    async fn cancellation_before_actor_commit_refuses_the_request() {
        let (commands, mut requests) = mpsc::channel(1);
        let sender = sender(commands, Duration::from_secs(1));
        let sending = tokio::spawn(async move { sender.send(Command::new("list-sessions")).await });

        let request = requests.recv().await.expect("the request is queued");
        sending.abort();
        assert!(
            sending
                .await
                .expect_err("the caller was cancelled")
                .is_cancelled(),
        );
        assert!(
            request.commit().is_none(),
            "the actor cannot commit a cancelled request",
        );
    }

    #[tokio::test]
    async fn deadline_before_actor_commit_refuses_the_request() {
        let timeout = Duration::from_millis(20);
        let (commands, mut requests) = mpsc::channel(1);
        let sender = sender(commands, timeout);
        let sending = tokio::spawn(async move { sender.send(Command::new("list-sessions")).await });

        let request = requests.recv().await.expect("the request is queued");
        let error = sending
            .await
            .expect("the caller task joins")
            .expect_err("the held request reaches its deadline");
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert!(
            matches!(
                &error,
                Error::ControlMode {
                    kind: ControlModeErrorKind::DispatchTimedOut,
                    ..
                }
            ),
            "the held command did not reach the write boundary",
        );
        assert!(
            error.is_transient(),
            "the unwritten command is safe to retry"
        );
        assert!(
            request.commit().is_none(),
            "the actor cannot commit the expired request",
        );
    }

    #[tokio::test]
    async fn cancellation_after_commit_keeps_reply_alignment() {
        let (commands, mut requests) = mpsc::channel(1);
        let sender = sender(commands, Duration::from_secs(1));
        let sending = tokio::spawn(async move { sender.send(Command::new("list-sessions")).await });

        let request = requests.recv().await.expect("the request is queued");
        let request = request.commit().expect("the actor commits the request");
        sending.abort();
        assert!(
            sending
                .await
                .expect_err("the caller was cancelled")
                .is_cancelled(),
        );

        let mut replies = ReplySlots::default();
        replies.push(request.result, request.deadline);
        assert!(
            matches!(replies.slots.front(), Some(ReplySlot::Tombstone { .. })),
            "the committed command keeps its reply slot",
        );

        let (next, mut next_answer) = oneshot::channel();
        replies.push(next, None);
        replies.complete(reply(1));
        assert!(
            matches!(
                next_answer.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ),
            "the cancelled command consumes its own block",
        );
        replies.complete(reply(2));
        assert_eq!(
            next_answer
                .try_recv()
                .expect("the next caller is answered")
                .expect("the next command succeeds")
                .number(),
            2,
        );
    }

    #[test]
    fn reply_deadline_is_the_earliest_pending_deadline() {
        let now = tokio::time::Instant::now();
        let earlier = now + Duration::from_secs(1);
        let later = now + Duration::from_secs(2);
        let mut replies = ReplySlots::default();
        let (first, _first_answer) = oneshot::channel();
        let (second, _second_answer) = oneshot::channel();

        replies.push(first, later.into());
        replies.push(second, earlier.into());
        assert_eq!(replies.earliest_deadline(), Some(earlier));
        replies.complete(reply(1));
        assert_eq!(replies.earliest_deadline(), Some(earlier));
        replies.complete(reply(2));
        assert_eq!(replies.earliest_deadline(), None);

        let (first, _first_answer) = oneshot::channel();
        let (second, _second_answer) = oneshot::channel();
        replies.push(first, earlier.into());
        replies.push(second, later.into());
        replies.complete(reply(3));
        assert_eq!(replies.earliest_deadline(), Some(later));
    }

    #[test]
    fn retries_at_the_unread_limit_do_not_grow_reply_slots() {
        let mut replies = ReplySlots::default();
        let (in_flight, _answer) = request();
        replies.push(in_flight.result, None);
        replies.refuse_live();
        let slots_at_cutoff = replies.slots.len();

        for _ in 0..64 {
            let (retry, answer) = request();
            assert!(
                admit_request(retry, HELD_WHILE_AWAITING).is_none(),
                "the retry does not cross the write boundary",
            );
            let error = refused(answer);
            assert_eq!(
                error.kind(),
                ErrorKind::Refused,
                "the retry reports the unread-event cutoff",
            );
            assert!(
                !error.is_transient(),
                "the kind also covers live requests that were already written",
            );
        }

        assert_eq!(
            replies.slots.len(),
            slots_at_cutoff,
            "retries refused before writing need no reply tombstones",
        );
    }

    #[test]
    fn block_headers_correlate_by_the_number_tmux_assigns() {
        assert_eq!(
            Line::parse(b"%begin 1786582374 347 0"),
            Line::BlockStart(347)
        );
        assert_eq!(
            Line::parse(b"%end 1786582374 347 0"),
            Line::BlockEnd {
                number: 347,
                succeeded: true,
            },
        );
        assert_eq!(
            Line::parse(b"%error 1786582374 353 1"),
            Line::BlockEnd {
                number: 353,
                succeeded: false,
            },
        );

        // A header without a usable number is text. Guessing one would
        // correlate a result with the wrong command.
        assert!(matches!(Line::parse(b"%begin bad"), Line::Text(_)));
    }

    /// Shared by the notification tests, which between them name every
    /// notification tmux writes. The strings are tmux's own format strings
    /// from `control-notify.c` and `control.c` with the placeholders filled.
    fn event(line: &[u8]) -> Event {
        match Line::parse(line) {
            Line::Event(event) => event,
            other => panic!("{other:?} is not an event"),
        }
    }

    fn a_session() -> SessionId {
        "$0".parse().expect("a session id parses")
    }

    fn a_window() -> WindowId {
        "@2".parse().expect("a window id parses")
    }

    fn a_pane() -> PaneId {
        "%3".parse().expect("a pane id parses")
    }

    #[test]
    fn session_notifications_are_parsed() {
        assert_eq!(
            event(b"%session-changed $0 work"),
            Event::SessionChanged {
                session: a_session(),
            },
        );
        assert_eq!(
            event(b"%session-renamed $0 renamed"),
            Event::SessionRenamed {
                session: a_session(),
                name: TmuxText::from_bytes(*b"renamed"),
            },
        );
        assert_eq!(
            event(b"%session-window-changed $0 @2"),
            Event::SessionWindowChanged {
                session: a_session(),
                window: a_window(),
            },
        );
        assert_eq!(event(b"%sessions-changed"), Event::SessionsChanged);
    }

    #[test]
    fn window_notifications_are_parsed() {
        assert_eq!(
            event(b"%window-add @2"),
            Event::WindowAdded { window: a_window() },
        );
        assert_eq!(
            event(b"%window-close @2"),
            Event::WindowClosed { window: a_window() },
        );
        assert_eq!(
            event(b"%window-renamed @2 build"),
            Event::WindowRenamed {
                window: a_window(),
                name: TmuxText::from_bytes(*b"build"),
            },
        );
        assert_eq!(
            event(b"%window-pane-changed @2 %3"),
            Event::WindowPaneChanged {
                window: a_window(),
                pane: a_pane(),
            },
        );
        assert_eq!(
            event(b"%unlinked-window-add @2"),
            Event::UnlinkedWindowAdded { window: a_window() },
        );
        assert_eq!(
            event(b"%unlinked-window-close @2"),
            Event::UnlinkedWindowClosed { window: a_window() },
        );
        assert_eq!(
            event(b"%unlinked-window-renamed @2 build"),
            Event::UnlinkedWindowRenamed {
                window: a_window(),
                name: TmuxText::from_bytes(*b"build"),
            },
        );
    }

    /// The one notification tmux builds from a format template, so its
    /// trailing field is whatever `#{window_raw_flags}` expanded to.
    #[test]
    fn a_layout_change_is_parsed() {
        assert_eq!(
            event(b"%layout-change @2 bc62,80x24,0,0,0 bc62,80x24,0,0,0 *"),
            Event::LayoutChanged {
                window: a_window(),
                layout: TmuxText::from_bytes(*b"bc62,80x24,0,0,0"),
                visible_layout: TmuxText::from_bytes(*b"bc62,80x24,0,0,0"),
                flags: TmuxText::from_bytes(*b"*"),
            },
        );
    }

    #[test]
    fn output_and_flow_control_notifications_are_parsed() {
        assert_eq!(
            event(b"%output %3 hi"),
            Event::Output {
                pane: a_pane(),
                bytes: b"hi".to_vec(),
            },
        );
        assert_eq!(
            event(b"%extended-output %3 1500 : hi"),
            Event::ExtendedOutput {
                pane: a_pane(),
                age: Duration::from_millis(1500),
                bytes: b"hi".to_vec(),
            },
        );
        assert_eq!(event(b"%pause %3"), Event::Paused { pane: a_pane() });
        assert_eq!(event(b"%continue %3"), Event::Continued { pane: a_pane() });
        assert_eq!(
            event(b"%pane-mode-changed %3"),
            Event::PaneModeChanged { pane: a_pane() },
        );
    }

    #[test]
    fn client_buffer_and_server_notifications_are_parsed() {
        assert_eq!(
            event(b"%client-detached /dev/pts/4"),
            Event::ClientDetached {
                client: TmuxText::from_bytes(*b"/dev/pts/4"),
            },
        );
        assert_eq!(
            event(b"%client-session-changed /dev/pts/4 $0 work"),
            Event::ClientSessionChanged {
                client: TmuxText::from_bytes(*b"/dev/pts/4"),
                session: a_session(),
                name: TmuxText::from_bytes(*b"work"),
            },
        );
        assert_eq!(
            event(b"%paste-buffer-changed buffer0"),
            Event::PasteBufferChanged {
                name: TmuxText::from_bytes(*b"buffer0"),
            },
        );
        assert_eq!(
            event(b"%paste-buffer-deleted buffer0"),
            Event::PasteBufferDeleted {
                name: TmuxText::from_bytes(*b"buffer0"),
            },
        );
        assert_eq!(
            event(b"%config-error /etc/tmux.conf:3: unknown command"),
            Event::ConfigError {
                message: TmuxText::from_bytes(*b"/etc/tmux.conf:3: unknown command"),
            },
        );
        assert_eq!(
            event(b"%message hello"),
            Event::Message {
                message: TmuxText::from_bytes(*b"hello"),
            },
        );
        assert_eq!(event(b"%exit"), Event::Exit { reason: None });
        assert_eq!(
            event(b"%exit too far behind"),
            Event::Exit {
                reason: Some(TmuxText::from_bytes(*b"too far behind")),
            },
        );
    }

    /// tmux writes `-` for a field the subscription does not name, so an
    /// absent one is a real answer rather than a parse failure.
    #[test]
    fn a_subscription_change_is_parsed_with_and_without_its_optional_fields() {
        assert_eq!(
            event(b"%subscription-changed watched $0 @2 7 %3 : value"),
            Event::SubscriptionChanged {
                name: TmuxText::from_bytes(*b"watched"),
                session: a_session(),
                window: Some(a_window()),
                index: Some(7),
                pane: Some(a_pane()),
                value: TmuxText::from_bytes(*b"value"),
            },
        );
        assert_eq!(
            event(b"%subscription-changed watched $0 - - - : value"),
            Event::SubscriptionChanged {
                name: TmuxText::from_bytes(*b"watched"),
                session: a_session(),
                window: None,
                index: None,
                pane: None,
                value: TmuxText::from_bytes(*b"value"),
            },
        );
    }

    /// tmux adds notifications between releases, so an unrecognized one is
    /// kept rather than dropped.
    #[test]
    fn an_unmodelled_notification_is_kept() {
        assert_eq!(
            event(b"%invented-later @2 build"),
            Event::Other {
                name: "invented-later".to_owned(),
                rest: TmuxText::from_bytes(*b"@2 build"),
            },
        );
    }

    /// tmux queues a notification raised while a block is open, so a line
    /// inside one is command output even when it reads as a notification.
    /// `list-panes -F '#{pane_id}'` writes `%0` for every row.
    #[test]
    fn a_block_line_that_looks_like_a_notification_is_output() {
        assert_eq!(
            Line::parse_within_block(b"%0", 12),
            Line::Text(TmuxText::from_bytes(*b"%0")),
        );
        assert_eq!(
            Line::parse_within_block(b"%output %3 hi", 12),
            Line::Text(TmuxText::from_bytes(*b"%output %3 hi")),
        );

        // The block's own terminator is the one line that is still structure.
        assert_eq!(
            Line::parse_within_block(b"%end 1786582374 12 0", 12),
            Line::BlockEnd {
                number: 12,
                succeeded: true,
            },
        );
        // Another block's terminator is not this block's, so it is output.
        assert_eq!(
            Line::parse_within_block(b"%end 1786582374 13 0", 12),
            Line::Text(TmuxText::from_bytes(*b"%end 1786582374 13 0")),
        );
    }

    /// Parsing these leniently would report a pane that does not exist, which
    /// is worse than reporting a line nobody claimed. The text keeps the whole
    /// line, notification name included, so nothing is lost by not knowing it.
    #[test]
    fn a_malformed_notification_is_text_rather_than_a_guess() {
        let cases: [&[u8]; 5] = [
            b"%window-add nonsense",
            b"%pause nonsense",
            b"%extended-output %3 notanumber : hi",
            b"%session-window-changed $0 nonsense",
            b"%begin bad",
        ];

        for line in cases {
            assert_eq!(
                Line::parse(line),
                Line::Text(TmuxText::from_bytes(line)),
                "{}",
                String::from_utf8_lossy(line),
            );
        }
    }

    #[test]
    fn an_event_says_whether_a_listing_is_now_stale() {
        let stale = |line: &[u8]| match Line::parse(line) {
            Line::Event(event) => event.invalidates_listings(),
            other => panic!("{other:?} is not an event"),
        };

        // Output says nothing about the shape of the server.
        assert!(!stale(b"%output %3 hi"));
        assert!(!stale(b"%extended-output %3 10 : hi"));
        assert!(!stale(b"%pause %3"));

        assert!(stale(b"%window-add @2"));
        assert!(stale(b"%window-close @2"));
        assert!(stale(b"%sessions-changed"));
        assert!(stale(b"%window-pane-changed @2 %3"));
        // An unmodelled notification is precisely the one whose meaning is
        // unknown here, so it counts as invalidating.
        assert!(stale(b"%invented-later whatever"));
    }

    #[test]
    fn a_line_is_bytes_because_tmux_does_not_promise_text() {
        // tmux escapes only what would break the line protocol, so a pane
        // emitting Latin-1 or binary produces a line that is not UTF-8.
        // Reading these as a string would fail the whole connection.
        let line = Line::parse(b"%output %0 \xff\xc3(");
        assert_eq!(
            line,
            Line::Event(Event::Output {
                pane: "%0".parse().expect("a pane id parses"),
                bytes: vec![0xff, 0xc3, b'('],
            }),
        );

        // The same holds for a window name inside a notification. The id is
        // ASCII and parses; the name it carries is whatever tmux stored.
        assert_eq!(
            Line::parse(b"%window-renamed @2 \xff"),
            Line::Event(Event::WindowRenamed {
                window: "@2".parse().expect("a window id parses"),
                name: TmuxText::from_bytes(*b"\xff"),
            }),
        );
    }

    #[test]
    fn output_escaping_round_trips_the_bytes_tmux_sends() {
        assert_eq!(unescape_output(b"plain"), b"plain");
        // tmux escapes a byte below 0x20 as three octal digits.
        assert_eq!(unescape_output(br"a\015b"), b"a\rb");
        assert_eq!(unescape_output(br"\377"), vec![0xff]);
        // A literal backslash arrives doubled.
        assert_eq!(unescape_output(br"a\\b"), b"a\\b");
        // Anything else after a backslash is not an escape tmux produces, so
        // it is kept rather than guessed at.
        assert_eq!(unescape_output(br"a\zb"), b"a\\zb");
    }
}

#[cfg(test)]
#[path = "control/lifecycle_tests.rs"]
mod lifecycle_tests;
