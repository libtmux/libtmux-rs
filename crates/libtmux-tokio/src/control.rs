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
//! # async fn watch(server: &libtmux_tokio::Server, id: &libtmux_tokio::SessionId) -> Result<(), libtmux_tokio::Error> {
//! use libtmux_tokio::control::{ControlMode, Event};
//!
//! let (commands, mut events) = ControlMode::attach(server, id).await?.split();
//!
//! // Commands travel down the connection, so none of these spawn a process.
//! let listed = commands.send(libtmux_tokio::Command::new("list-windows").arg("-F").arg("#{window_id}")).await?;
//! assert!(listed.succeeded());
//!
//! // The watcher reads, and only reads. A reply arrives on the connection
//! // the events arrive on, so a loop that stops reading in order to await one
//! // is waiting on the connection it stopped reading.
//! let watcher = tokio::spawn(async move {
//!     while let Some(event) = events.next_event().await {
//!         match event? {
//!             Event::Output { pane, bytes } => println!("{pane}: {} bytes", bytes.len()),
//!             Event::Exit { .. } => break,
//!             other => println!("{other:?}"),
//!         }
//!     }
//!
//!     // Explicit shutdown also closes a connection before stream exhaustion.
//!     events.shutdown().await
//! });
//!
//! // Acting on what the watcher sees happens out here, on the other handle,
//! // which is what having two of them is for.
//! commands.send(libtmux_tokio::Command::new("list-panes").arg("-F").arg("#{pane_id}")).await?;
//!
//! let _ = watcher.await;
//! Ok(())
//! # }
//! ```
//!
//! `examples/watch.rs` is this as a program that runs, against a server it
//! starts and cleans up.

use std::future::{Future as _, poll_fn};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_core::Stream;
use tokio::sync::{Semaphore, mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::limits::ControlLimits;
use crate::version::since::CONTROL_PANE_OFF;
use crate::{Command, Error, IdParseError, PaneId, Server, SessionId, TmuxText, WindowId};

mod actor;
#[cfg(feature = "unstable-fuzzing")]
mod fuzz;
pub(crate) mod policy;

use actor::{Request, deadline_elapsed};

pub use libtmux_core::control::{ControlNotice, Event};

/// The outcome of one command sent over control mode.
#[derive(Clone, Eq, PartialEq)]
pub struct BlockResult {
    number: u64,
    succeeded: bool,
    output: Vec<TmuxText>,
    sensitive_input: bool,
    blocks: Vec<libtmux_core::control::ControlBlock>,
    hook_failures: usize,
}

impl BlockResult {
    fn from_shared(reply: libtmux_core::control::ControlReply) -> Self {
        let mut output = Vec::new();
        for block in reply.blocks() {
            if let Some(bytes) = block.output().strip_suffix(b"\n") {
                output.extend(bytes.split(|byte| *byte == b'\n').map(TmuxText::from_bytes));
            }
        }
        Self {
            number: reply
                .blocks()
                .last()
                .map_or(0, |block| block.header().number),
            succeeded: reply.succeeded(),
            sensitive_input: false,
            output,
            hook_failures: reply.hook_failures(),
            blocks: reply.into_blocks(),
        }
    }

    /// The exact input blocks, excluding hook blocks and the private drain fence.
    #[must_use]
    pub fn blocks(&self) -> &[libtmux_core::control::ControlBlock] {
        &self.blocks
    }

    /// The number of failed hook blocks, with their payloads withheld.
    #[must_use]
    pub const fn hook_failures(&self) -> usize {
        self.hook_failures
    }
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

        let bytes = self
            .blocks
            .iter()
            .find(|block| !block.succeeded())
            .map_or(&[][..], |block| block.output());
        let classified = Error::refused(
            operation,
            None,
            String::from_utf8_lossy(bytes).into_owned(),
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

impl std::fmt::Debug for BlockResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BlockResult")
            .field("number", &self.number)
            .field("succeeded", &self.succeeded)
            .field("line_count", &self.output.len())
            .field("block_count", &self.blocks.len())
            .field("hook_failures", &self.hook_failures)
            .finish_non_exhaustive()
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
    /// Refuses a daemon ranking below 3.7 final (`next-3.7`, 3.7 release candidates).
    /// Capacity, metadata, socket pinning and the opening handshake share one
    /// deadline from attach entry. Process, pipe and framing failures also
    /// refuse the attachment.
    ///
    /// # Cancel safety
    ///
    /// Nothing is left held: a dropped attach kills and reaps the tmux client
    /// it started.
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
    ///
    /// # Cancel safety
    ///
    /// Nothing is left held, as for [`Self::attach`].
    pub async fn attach_with_limits(
        server: &Server,
        session: &SessionId,
        limits: ControlLimits,
    ) -> Result<Self, Error> {
        let timeout = server.default_timeout();
        let deadline = Some(
            Instant::now()
                .checked_add(timeout)
                .ok_or_else(Error::control_mode_timeout)?,
        );
        if limits.max_pending_requests == 0 || limits.max_pending_requests > Semaphore::MAX_PERMITS
        {
            return Err(Error::ControlProtocol {
                fault: libtmux_core::control::ControlFault::Busy {
                    limit: limits.max_pending_requests,
                },
            });
        }
        let child = server.spawn_control(session, deadline).await?;
        let identity = child.identity().clone();
        let generation = identity.generation();
        let native_server = server.native_source().bound_to(generation)?;
        let version = child.version().clone();
        let pane_off_is_safe = version.has_behavior(&CONTROL_PANE_OFF);
        let actor::OpenedConnection {
            commands,
            admission,
            events,
            stop,
            connection,
        } = actor::open(child, session.clone(), limits, deadline).await?;

        let sender = ControlSender {
            commands,
            admission,
            timeout,
            pane_off_is_safe,
            version,
            max_line_bytes: limits.max_line_bytes,
            max_reply_blocks: limits.max_reply_blocks,
            generation,
            native_server,
            identity: server.identity().clone(),
        };
        let events = ControlEvents {
            events,
            stop,
            connection: Some(connection),
        };

        let challenge = libtmux_core::control::ControlProbe::new(
            session.clone(),
            crate::internal::operation::nonce()?,
        );
        let challenge_line = challenge
            .command()
            .control_mode_line()
            .ok_or_else(Error::control_mode_unrepresentable)?;
        let qualification = sender
            .dispatch_until(
                challenge_line,
                true,
                libtmux_core::control::RequestKind::Ordinary,
                Some((challenge, identity)),
                deadline,
                None,
                None,
            )
            .await;
        if let Err(error) = qualification {
            drop(sender);
            return Err(events.shutdown_after_error(error).await);
        }
        if !events.events.is_qualified() {
            drop(sender);
            return Err(events
                .shutdown_after_error(Error::ControlProtocol {
                    fault: libtmux_core::control::ControlFault::InvalidChallenge,
                })
                .await);
        }

        // Without this, tmux 3.8+ hands a control client the classic
        // `window_layout` string instead of JSON, disagreeing with a plain
        // client's snapshot; a release below 3.8 ignores the unknown flag.
        let layouts = Command::new("refresh-client")
            .arg("-f")
            .arg("new-layouts")
            .control_mode_line()
            .ok_or_else(Error::control_mode_unrepresentable)?;
        if let Err(error) = sender
            .dispatch_until(
                layouts,
                false,
                libtmux_core::control::RequestKind::Ordinary,
                None,
                deadline,
                None,
                None,
            )
            .await
            .and_then(|reply| reply.require_success("refresh-client"))
        {
            drop(sender);
            return Err(events.shutdown_after_error(error).await);
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
    /// # Cancel safety
    ///
    /// As for [`ControlSender::send`]: a command dropped while queued is never
    /// written, and one already committed may run.
    pub async fn send(&self, command: Command) -> Result<BlockResult, Error> {
        self.sender.send(command).await
    }

    /// Return the next notification or terminal error, then `None`.
    ///
    /// See [`ControlEvents::next_event`] for termination.
    ///
    /// # Cancel safety
    ///
    /// Nothing happened: a dropped call consumes no event.
    pub async fn next_event(&mut self) -> Option<Result<Event, Error>> {
        self.events.next_event().await
    }

    /// Close the connection and report how it ended.
    ///
    /// # Errors
    ///
    /// Returns a connection error that was not already delivered by
    /// [`Self::next_event`], or [`Error::ShutdownPending`] if cleanup exceeds
    /// its allowance.
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
/// use libtmux_tokio::control::Subscription;
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
    Window(
        /// The window ID used to scope the watched format.
        WindowId,
    ),
    /// Every window in the attached session.
    AllWindows,
    /// One pane.
    Pane(
        /// The pane ID used to scope the watched format.
        PaneId,
    ),
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
/// The name is an unescaped event label; separators would change its meaning.
fn check_subscription_name(name: &str) -> Result<(), Error> {
    if !policy::valid_subscription_name(name.as_bytes()) {
        return Err(Error::control_mode_invalid_subscription());
    }
    Ok(())
}

/// Sends commands down a control-mode connection.
///
/// Cheap to clone, and every method that sends takes `&self`, so several tasks
/// can issue commands while another watches events.
#[derive(Clone, Debug)]
pub struct ControlSender {
    commands: mpsc::Sender<Request>,
    admission: Arc<Semaphore>,
    timeout: Duration,
    max_line_bytes: usize,
    max_reply_blocks: usize,
    generation: crate::ServerGeneration,
    native_server: Server,
    /// Whether this tmux can take a pane out of the stream with `off`.
    ///
    /// Read once at attach rather than per call: the server cannot change
    /// release under a connection.
    pane_off_is_safe: bool,
    /// The daemon version checked when this connection attached.
    ///
    /// The private nonce challenge verified this report on the pinned connection.
    version: crate::TmuxVersion,
    /// Which server this connection reaches.
    ///
    /// A sender says nothing about where it points, so routing one onto a
    /// handle for a different server would silently talk to this one.
    /// [`crate::Server::over_control_mode`] compares it and refuses.
    identity: crate::ServerIdentity,
}

impl ControlSender {
    pub(crate) const fn generation(&self) -> crate::ServerGeneration {
        self.generation
    }
    pub(crate) const fn native_server(&self) -> &Server {
        &self.native_server
    }
    #[cfg(feature = "plan")]
    pub(crate) async fn create_session(
        &self,
        request: libtmux_core::NewSession,
    ) -> Result<libtmux_core::CreatedSession, Error> {
        let deadline = Some(
            Instant::now()
                .checked_add(self.timeout)
                .ok_or_else(Error::control_mode_dispatch_timeout)
                .map_err(crate::internal::operation::OperationContext::before_dispatch)?,
        );
        let command = Command::new_session(&request, libtmux_core::NewSession::RECEIPT_FORMAT);
        let Some(line) = command.control_mode_line_bounded(self.line_limit()) else {
            return self
                .native_server()
                .new_session_until(request, deadline)
                .await;
        };
        let dispatch = async {
            crate::command::require_features(&command, &self.version)?;
            policy::require_shared(&command.logical_words().collect::<Vec<_>>())?;
            let creation =
                actor::Creation::new(request.clone(), self.identity.clone(), self.generation);
            self.dispatch_until(
                line,
                true,
                libtmux_core::control::RequestKind::Ordinary,
                None,
                deadline,
                None,
                Some(creation),
            )
            .await
        };
        let reply = dispatch
            .await
            .map_err(crate::internal::operation::OperationContext::before_dispatch)?;
        created_from_reply(&request, &reply, &self.identity, self.generation)
    }
    pub(crate) fn line_limit(&self) -> usize {
        self.max_line_bytes.saturating_sub(1)
    }
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
    /// A private nonce fence drains all input and hook blocks belonging to
    /// this request. Client-blocking waits and raw grid or buffer output are
    /// refused before admission; use an ordinary native client for those.
    /// A cancelled complete write is drained within a separate bound or the
    /// connection closes. A partial input write always closes the connection.
    ///
    /// # Errors
    ///
    /// Returns an error when the command cannot be written as a control-mode
    /// line, the connection has closed, or its deadline elapses while queued,
    /// being written, or awaiting a response.
    ///
    /// # Cancel safety
    ///
    /// A command dropped while queued is never written. Once the connection has
    /// committed it, tmux may run it, and its reply is still read and
    /// discarded, so later replies stay aligned.
    pub async fn send(&self, command: Command) -> Result<BlockResult, Error> {
        self.send_ordered(command, libtmux_core::control::RequestKind::Ordinary)
            .await
    }

    /// Return the server this connection reaches.
    pub(crate) const fn identity(&self) -> &crate::ServerIdentity {
        &self.identity
    }

    /// Return the daemon version checked when this connection attached.
    pub(crate) const fn version(&self) -> &crate::TmuxVersion {
        &self.version
    }

    /// Send a command whose completed block marks one point in event order.
    async fn send_ordered(
        &self,
        command: Command,
        kind: libtmux_core::control::RequestKind,
    ) -> Result<BlockResult, Error> {
        let deadline = Some(
            Instant::now()
                .checked_add(self.timeout)
                .ok_or_else(Error::control_mode_dispatch_timeout)?,
        );
        let line = command
            .control_mode_line_bounded(self.line_limit())
            .ok_or_else(Error::control_mode_unrepresentable)?;
        crate::command::require_features(&command, &self.version)?;
        policy::require_shared(&command.logical_words().collect::<Vec<_>>())?;
        let sensitive_input = command.summary().sensitive_argument_count() > 0;
        self.dispatch_until(line, sensitive_input, kind, None, deadline, None, None)
            .await
    }

    /// Send one already-rendered control-mode line holding `commands`
    /// commands.
    ///
    /// The typed API routes through here: a request built for dispatch carries
    /// its own rendering, so re-deriving one from the argv is neither needed
    /// nor correct. tmux answers each command of a chain with its own block,
    /// and they come back as one result.
    pub(crate) async fn send_line(
        &self,
        line: String,
        sensitive_input: bool,
        _commands: usize,
        limits: crate::OutputLimits,
    ) -> Result<BlockResult, Error> {
        let deadline = Some(
            Instant::now()
                .checked_add(self.timeout)
                .ok_or_else(Error::control_mode_dispatch_timeout)?,
        );
        let reply_limit = limits
            .max_stdout_bytes
            .saturating_add(limits.max_stderr_bytes)
            .saturating_add(self.max_reply_blocks.saturating_mul(160))
            .saturating_add(256);
        self.dispatch_until(
            line,
            sensitive_input,
            libtmux_core::control::RequestKind::Ordinary,
            None,
            deadline,
            Some(reply_limit),
            None,
        )
        .await
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one request carries admission, drain and typed receipt context"
    )]
    async fn dispatch_until(
        &self,
        line: String,
        sensitive_input: bool,
        kind: libtmux_core::control::RequestKind,
        challenge: Option<(
            libtmux_core::control::ControlProbe,
            libtmux_core::control::ControlIdentity,
        )>,
        deadline: Option<Instant>,
        reply_limit: Option<usize>,
        creation: Option<actor::Creation>,
    ) -> Result<BlockResult, Error> {
        if line.len() > self.line_limit() {
            return Err(Error::control_mode_frame_too_large(
                "command",
                self.max_line_bytes,
            ));
        }
        let (result, mut answer) = oneshot::channel();
        let (commit, mut commitment) = oneshot::channel();
        let evidence = creation.as_ref().map(actor::Creation::evidence);
        let finish = |answer: Result<Result<BlockResult, Error>, oneshot::error::RecvError>| {
            let mut block = answer
                .map_err(|_| Error::control_mode_closed())
                .and_then(std::convert::identity)
                .map_err(|source| {
                    if source.operation_effect().is_none() {
                        match &evidence {
                            Some(evidence) => evidence.interrupted(source),
                            None => source,
                        }
                    } else {
                        source
                    }
                })?;
            block.sensitive_input = sensitive_input;
            Ok(block)
        };

        if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
            return Err(Error::control_mode_dispatch_timeout());
        }
        let admission = tokio::select! {
            biased;
            permit = Arc::clone(&self.admission).acquire_owned() => {
                permit.map_err(|_| Error::control_mode_closed())?
            }
            () = deadline_elapsed(deadline) => {
                return Err(Error::control_mode_dispatch_timeout());
            }
        };
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
            kind,
            permit: admission,
            challenge,
            reply_limit,
            creation,
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
    pub async fn unmute_pane(&self, pane: &PaneId) -> Result<(), Error> {
        self.recover_pane(pane).await
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
    pub async fn resume_pane(&self, pane: &PaneId) -> Result<(), Error> {
        self.recover_pane(pane).await
    }

    /// Undo whichever of `off` or `pause` a pane is under, in one dispatch.
    ///
    /// `refresh-client -A` accepts more than one `pane:state` pair, so `on`
    /// and `continue` travel together. tmux's `control_set_pane_on` and
    /// `control_continue_pane` are each a no-op when their own flag is not
    /// set, so sending both is correct whether the pane was taken out of the
    /// stream, paused, both, or neither.
    async fn recover_pane(&self, pane: &PaneId) -> Result<(), Error> {
        self.send_ordered(
            Command::new("refresh-client")
                .arg("-A")
                .arg(format!("{pane}:on"))
                .arg("-A")
                .arg(format!("{pane}:continue")),
            libtmux_core::control::RequestKind::Continue(pane.clone()),
        )
        .await?
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
    /// subscription, the format can expand arbitrary text, or the name is not
    /// an ASCII token containing only letters, digits, `.`, `_` and `-`.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux_tokio::control::{ControlMode, Event, Subscription};
    ///
    /// let guard = libtmux_tokio::test::TestServer::new().await?;
    /// # let version = guard.server().capabilities().await?.tmux_version().clone();
    /// # let eligible = version.has_behavior(&libtmux_tokio::since::CONTROL_ACQUISITION);
    /// # if !eligible {
    /// #     guard.shutdown().await?;
    /// #     return Ok(());
    /// # }
    /// let session = guard.session("watched").await?;
    /// let (commands, mut events) = ControlMode::attach(guard.server(), session.id())
    ///     .await?
    ///     .split();
    ///
    /// commands
    ///     .subscribe("windows", &Subscription::Session, "#{session_windows}")
    ///     .await?;
    ///
    /// // The first report arrives without anything having changed, which is
    /// // what makes a subscription usable for reading the value as well.
    /// while let Some(event) = events.next_event().await {
    ///     if let Event::SubscriptionChanged { name, value, .. } = event? {
    ///         assert_eq!(name.as_str()?, "windows");
    ///         assert_eq!(value.as_str()?, "1");
    ///         break;
    ///     }
    /// }
    ///
    /// commands.unsubscribe("windows").await?;
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
    /// request, or the name is not an ASCII token containing only letters,
    /// digits, `.`, `_` and `-`.
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
    /// # Cancel safety
    ///
    /// The effect can be partial: panes are muted one at a time, so a dropped
    /// call can leave some muted and others not. Muting is idempotent, so
    /// calling again finishes the job.
    pub async fn watch_only(&self, panes: &[PaneId]) -> Result<(), Error> {
        let listed = self
            .send(
                Command::new("list-panes")
                    .arg("-s")
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

    /// Report whether tmux still lists this pane, anywhere on the server.
    ///
    /// Used to tell a pane's death from an unrelated listing change: an event
    /// that may mean a pane appeared can equally mean one left, and tmux
    /// publishes no notification that names which.
    pub(crate) async fn pane_exists(&self, pane: &PaneId) -> Result<bool, Error> {
        let listed = self
            .send(
                Command::new("list-panes")
                    .arg("-a")
                    .arg("-F")
                    .arg("#{pane_id}"),
            )
            .await?
            .require_success("list-panes")?;

        for line in listed.output() {
            if decode_watched_pane_id(line)? == *pane {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn set_pane_stream(&self, pane: &PaneId, state: &str) -> Result<(), Error> {
        self.send_ordered(
            Command::new("refresh-client")
                .arg("-A")
                .arg(format!("{pane}:{state}")),
            if state == "pause" {
                libtmux_core::control::RequestKind::Pause(pane.clone())
            } else {
                libtmux_core::control::RequestKind::Ordinary
            },
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

#[cfg(feature = "plan")]
fn created_from_reply(
    request: &libtmux_core::NewSession,
    reply: &BlockResult,
    server: &crate::ServerIdentity,
    generation: crate::ServerGeneration,
) -> Result<libtmux_core::CreatedSession, Error> {
    let unconfirmed = || {
        Error::from(libtmux_core::Error::UnconfirmedCreation {
            status: None,
            late: false,
        })
    };
    let first = reply.blocks().first().ok_or_else(unconfirmed)?;
    let receipt = request
        .decode(server.socket_path(), first.output())
        .map_err(|_| unconfirmed())?;
    if receipt.generation() != generation {
        return Err(unconfirmed());
    }
    if !reply.succeeded() {
        return Err(libtmux_core::Error::CreationFollowup {
            receipt: Some(Box::new(receipt)),
            status: None,
            late: false,
        }
        .into());
    }
    Ok(receipt)
}

fn decode_watched_pane_id(line: &TmuxText) -> Result<PaneId, Error> {
    let invalid = |detail| Error::UnreadableFormatValue {
        format: "#{pane_id}",
        detail,
    };
    let id = line.as_str().map_err(|_| invalid(IdParseError::new('%')))?;
    id.parse().map_err(invalid)
}

fn retain_cleanup(mut primary: Error, terminal: Error) -> Error {
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
    } else if let Error::OperationAndCleanup { cleanup, .. } = terminal {
        return Error::OperationAndCleanup {
            primary: Box::new(primary),
            cleanup,
        };
    }
    primary
}

/// Receives tmux notifications and terminal connection errors.
///
/// This is a [`Stream<Item = Result<Event, Error>>`](Stream). Notifications
/// arrive in order. A connection failure follows all buffered notifications
/// as one `Err`; subsequent polls return `None`. A normal tmux `%exit` arrives
/// as [`Event::Exit`] and is followed by `None` after cleanup succeeds. EOF
/// without `%exit` is [`crate::ControlModeErrorKind::Closed`].
/// [`Server::shutdown`] may discard notifications still waiting for delivery
/// so an unread stream cannot prevent executor shutdown.
///
/// Exhaustion waits for connection cleanup. [`Self::shutdown`] closes early
/// and returns any terminal error the stream has not already delivered.
///
/// Notices are bounded by count and wire bytes. Reading from tmux continues
/// while the subscriber is idle; discarded notices produce an explicit gap.
/// [`Self::next_notice`] returns structured gaps and [`Self::next_event`]
/// reports them as nonterminal [`Error::ControlGap`] values. Drop this handle
/// to discard notices while command senders continue working.
#[derive(Debug)]
pub struct ControlEvents {
    events: actor::EventReceiver,
    /// Requests closure; dropping it leaves remaining senders working.
    stop: watch::Sender<()>,
    connection: Option<tokio::task::JoinHandle<Result<(), Error>>>,
}

impl ControlEvents {
    pub(crate) async fn shutdown_after_error(self, primary: Error) -> Error {
        match self.shutdown().await {
            Err(Error::ShutdownPending) => Error::OperationCleanupPending {
                source: Box::new(primary),
            },
            Err(terminal) => retain_cleanup(primary, terminal),
            Ok(()) => primary,
        }
    }

    /// Return the next notification or terminal error, then `None`.
    ///
    /// Once `None` is returned, subsequent calls also return `None`. See
    /// [`ControlEvents`] for the EOF and cleanup contract.
    ///
    /// # Errors
    ///
    /// Yields one terminal error for transport failure, unexpected EOF, a
    /// frame budget or command deadline being exceeded, executor shutdown,
    /// or failed connection cleanup. A failed connection task closes the stream.
    ///
    /// # Cancel safety
    ///
    /// Nothing happened: a dropped call consumes neither an event nor the
    /// terminal error.
    pub async fn next_event(&mut self) -> Option<Result<Event, Error>> {
        poll_fn(|context| Pin::new(&mut *self).poll_next(context)).await
    }

    /// End the connection and report how it went.
    ///
    /// Stops the connection even while command senders remain alive. Unread
    /// notifications are discarded so cleanup cannot wait on a full buffer.
    /// If the stream already delivered its terminal error, this succeeds;
    /// that error is not delivered twice.
    ///
    /// # Errors
    ///
    /// Returns a connection or cleanup error not already delivered by the
    /// stream. After the one-second allowance, [`Error::ShutdownPending`]
    /// leaves cleanup with the connection task. Keep its Tokio runtime alive.
    pub async fn shutdown(mut self) -> Result<(), Error> {
        let _ = self.stop.send(());
        // Draining releases a connection that is parked handing over an event,
        // so it reaches its own shutdown rather than waiting for a reader that
        // is not coming back.
        self.events.close();

        let Some(connection) = self.connection.take() else {
            return Ok(());
        };
        crate::internal::operation::finish_shutdown(async {
            connection.await.map_err(|_| Error::control_mode_closed())?
        })
        .await
    }
}

impl ControlEvents {
    /// Return the next notice, including an explicit subscriber gap.
    ///
    /// A gap preserves command progress and requires the subscriber to resynchronise.
    ///
    /// # Errors
    ///
    /// Returns the terminal failures described by [`Self::next_event`].
    ///
    /// # Cancel safety
    ///
    /// Dropping a pending call consumes no notice or terminal error.
    pub async fn next_notice(&mut self) -> Option<Result<ControlNotice, Error>> {
        poll_fn(|context| self.poll_notice(context)).await
    }

    fn poll_notice(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<ControlNotice, Error>>> {
        if let Some(notice) = std::task::ready!(self.events.poll_recv(context)) {
            return Poll::Ready(Some(Ok(notice)));
        }
        let Some(connection) = self.connection.as_mut() else {
            return Poll::Ready(None);
        };
        let outcome = std::task::ready!(Pin::new(connection).poll(context));
        self.connection = None;
        Poll::Ready(match outcome {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(Err(error)),
            Err(_) => Some(Err(Error::control_mode_closed())),
        })
    }
}

impl Stream for ControlEvents {
    type Item = Result<Event, Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.poll_notice(context).map(|notice| {
            notice.map(|notice| {
                notice.and_then(|notice| match notice {
                    ControlNotice::Event(event) => Ok(event),
                    ControlNotice::Gap {
                        dropped,
                        included_pause,
                    } => Err(Error::ControlGap {
                        dropped,
                        included_pause,
                    }),
                    _ => Err(Error::control_mode_closed()),
                })
            })
        })
    }
}

const NARROW_IDLE: u8 = 0;
const NARROW_RUNNING: u8 = 1;
const NARROW_DIRTY: u8 = 2;
const PANE_NOTICE_BATCH: usize = 64;

/// An observed terminal condition for one pane output stream.
///
/// # Examples
///
/// ```
/// use libtmux_tokio::control::PaneOutputEnd;
/// assert_eq!(PaneOutputEnd::PaneGone, PaneOutputEnd::PaneGone);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PaneOutputEnd {
    /// A checked listing no longer contains the watched pane.
    PaneGone,
    /// The control connection ended; this does not establish whether the pane lives.
    ConnectionEnded,
}

/// A fallible stream of the bytes one pane produces.
///
/// Output chunks retain their exact bytes. A subscriber gap is an
/// [`Error::ControlGap`], which permits further reads after resynchronisation.
/// A pane ending, server exit or connection failure is reported once before
/// the stream ends. Grid capture uses an independent generation-bound request;
/// it does not establish continuity or an ordering point in this event stream.
#[derive(Debug)]
pub struct PaneOutput {
    pane: PaneId,
    events: ControlEvents,
    closed: bool,
    ending: Option<PaneOutputEnd>,
    sender: ControlSender,
    narrowing: Arc<AtomicU8>,
    narrow_handle: Option<tokio::task::JoinHandle<Result<bool, Error>>>,
    capture: Option<(Server, libtmux_core::PaneKey)>,
}

impl PaneOutput {
    pub(crate) fn new(pane: PaneId, events: ControlEvents, sender: ControlSender) -> Self {
        Self {
            pane,
            events,
            closed: false,
            ending: None,
            sender,
            narrowing: Arc::new(AtomicU8::new(NARROW_IDLE)),
            narrow_handle: None,
            capture: None,
        }
    }

    pub(crate) fn with_capture(mut self, server: Server, key: libtmux_core::PaneKey) -> Self {
        self.capture = Some((server, key));
        self
    }

    fn narrow(&mut self) {
        #[allow(deprecated)]
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
        self.narrow_handle = Some(tokio::spawn(async move {
            loop {
                sender.watch_only(std::slice::from_ref(&pane)).await?;
                match narrowing.compare_exchange(
                    NARROW_RUNNING,
                    NARROW_IDLE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(_) => narrowing.store(NARROW_RUNNING, Ordering::Release),
                }
            }
            sender.pane_exists(&pane).await
        }));
    }

    /// The pane this stream watches.
    #[must_use]
    pub const fn pane(&self) -> &PaneId {
        &self.pane
    }

    /// The sender controlling this stream's private connection.
    #[must_use]
    pub const fn sender(&self) -> &ControlSender {
        &self.sender
    }

    /// Capture this pane's grid through an independent generation-bound request.
    ///
    /// A bounded batch of buffered chunks is passed to `on_output` first and
    /// stays consumed. The captured grid may overlap them or later output;
    /// it is not a continuity boundary or an offset in this stream.
    ///
    /// # Errors
    ///
    /// Returns a subscriber gap or stream failure while consuming buffered
    /// chunks, or a native capture failure. Refuses an unbound capture key.
    ///
    /// # Cancel safety
    ///
    /// Chunks already passed to `on_output` remain consumed.
    pub async fn snapshot(
        &mut self,
        mut on_output: impl FnMut(&[u8]),
    ) -> Result<Vec<TmuxText>, Error> {
        let (chunks, byte_limit) = self.events.events.snapshot_budget();
        let mut collected = 0usize;
        for _ in 0..chunks {
            if collected >= byte_limit {
                break;
            }
            match poll_fn(|context| Poll::Ready(Pin::new(&mut *self).poll_next(context))).await {
                Poll::Ready(Some(Ok(bytes))) => {
                    collected = collected.saturating_add(bytes.len());
                    on_output(&bytes);
                }
                Poll::Ready(Some(Err(error))) => return Err(error),
                Poll::Ready(None) | Poll::Pending => break,
            }
        }
        let Some((server, key)) = &self.capture else {
            return Err(Error::control_mode_unrepresentable());
        };
        let bytes = server.capture(key).await?;
        Ok(bytes
            .strip_suffix(b"\n")
            .unwrap_or(&bytes)
            .split(|byte| *byte == b'\n')
            .map(TmuxText::from_bytes)
            .collect())
    }

    /// Return one exact output chunk, a gap or terminal failure, then `None`.
    ///
    /// A gap is nonterminal; callers must resynchronise before claiming
    /// continuity.
    ///
    /// # Cancel safety
    ///
    /// Dropping a pending call consumes no chunk or error.
    pub async fn next_chunk(&mut self) -> Option<Result<Vec<u8>, Error>> {
        poll_fn(|context| Pin::new(&mut *self).poll_next(context)).await
    }

    /// Close the private connection and report any undelivered cleanup failure.
    ///
    /// # Errors
    ///
    /// Returns a connection or cleanup failure not already delivered, including
    /// [`Error::ShutdownPending`] when the connection task is still reaping.
    pub async fn shutdown(self) -> Result<(), Error> {
        drop(self.sender);
        self.events.shutdown().await
    }

    fn actor_reports(&self, error: &Error) -> bool {
        match error {
            Error::ControlModeFrameTooLarge {
                frame: "line" | "reply" | "blocks",
                ..
            }
            | Error::ControlProtocol {
                fault: libtmux_core::control::ControlFault::Disconnected,
            }
            | Error::ControlMode {
                kind: crate::ControlModeErrorKind::Closed,
                ..
            } => true,
            Error::ControlProtocol { .. }
            | Error::ExecutorShutdown { .. }
            | Error::ControlMode {
                kind: crate::ControlModeErrorKind::Transport | crate::ControlModeErrorKind::TimedOut,
                ..
            } => self.sender.is_closed(),
            _ => false,
        }
    }

    fn end_after_notices(&mut self, cause: PaneOutputEnd) {
        if self.ending.is_none() {
            self.ending = Some(cause);
            let _ = self.events.stop.send(());
        }
    }

    fn terminal(&mut self, error: Option<Error>) -> Poll<Option<Result<Vec<u8>, Error>>> {
        self.closed = true;
        let cause = self.ending.unwrap_or(PaneOutputEnd::ConnectionEnded);
        Poll::Ready(Some(Err(match error {
            None
            | Some(
                Error::ControlProtocol {
                    fault: libtmux_core::control::ControlFault::Disconnected,
                }
                | Error::ControlMode {
                    kind: crate::ControlModeErrorKind::Closed,
                    ..
                },
            ) => Error::PaneOutputEnded { cause },
            Some(error) => error,
        })))
    }
}

impl Stream for PaneOutput {
    type Item = Result<Vec<u8>, Error>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.closed {
            return Poll::Ready(None);
        }
        if let Some(mut handle) = this.narrow_handle.take() {
            match Pin::new(&mut handle).poll(context) {
                Poll::Pending => this.narrow_handle = Some(handle),
                Poll::Ready(Ok(Ok(true))) => {}
                Poll::Ready(Ok(Ok(false))) => this.end_after_notices(PaneOutputEnd::PaneGone),
                // A watch RPC receives a copy of an actor failure. Drain the
                // original through the event receiver, after any queued bytes.
                Poll::Ready(Ok(Err(error))) if this.actor_reports(&error) => {
                    this.end_after_notices(PaneOutputEnd::ConnectionEnded);
                }
                Poll::Ready(outcome) => {
                    this.closed = true;
                    let _ = this.events.stop.send(());
                    return Poll::Ready(Some(Err(match outcome {
                        Ok(Err(error)) => error,
                        _ => Error::control_mode_closed(),
                    })));
                }
            }
        }
        // Unrelated notices must not keep one stream poll from returning.
        for _ in 0..PANE_NOTICE_BATCH {
            match std::task::ready!(this.events.poll_notice(context)) {
                Some(Ok(ControlNotice::Event(
                    Event::Output { pane, bytes } | Event::ExtendedOutput { pane, bytes, .. },
                ))) if pane == this.pane => return Poll::Ready(Some(Ok(bytes))),
                Some(Ok(ControlNotice::Event(Event::Paused { pane }))) if pane == this.pane => {
                    return Poll::Ready(Some(Err(Error::ControlGap {
                        dropped: 0,
                        included_pause: true,
                    })));
                }
                Some(Ok(ControlNotice::Gap {
                    dropped,
                    included_pause,
                })) => {
                    return Poll::Ready(Some(Err(Error::ControlGap {
                        dropped,
                        included_pause,
                    })));
                }
                Some(Ok(ControlNotice::Event(Event::Exit { .. }))) => {
                    this.end_after_notices(PaneOutputEnd::ConnectionEnded);
                }
                Some(Ok(ControlNotice::Event(event))) => {
                    if this.ending.is_none()
                        && (event.may_have_added_a_pane()
                            || matches!(
                                event,
                                Event::WindowClosed { .. } | Event::UnlinkedWindowClosed { .. }
                            ))
                    {
                        this.narrow();
                    }
                }
                Some(Err(error)) => return this.terminal(Some(error)),
                Some(Ok(_)) => {}
                None => return this.terminal(None),
            }
        }
        context.waker().wake_by_ref();
        Poll::Pending
    }
}

/// Parse one control-mode protocol line, for fuzzing only.
///
/// The parser is the crate's most exposed surface: it reads bytes from a
/// process that keeps running, and every other decoder sits behind a tmux
/// command that ended. Nothing here is a supported API -- it exists so a
/// fuzzer can reach the shared bounded tracker -- and it is
/// gated behind a feature no release turns on.
#[cfg(feature = "unstable-fuzzing")]
#[doc(hidden)]
pub fn __fuzz_parse_control_line(line: &[u8]) {
    __fuzz_control_blocks(line);
}

#[cfg(feature = "unstable-fuzzing")]
#[doc(hidden)]
pub use fuzz::__fuzz_control_blocks;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "control/lifecycle_tests.rs"]
mod lifecycle_tests;
