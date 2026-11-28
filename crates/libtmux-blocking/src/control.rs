//! A native persistent control client with bounded replies and notices.
//!
//! The `control-mode` feature is off by default. Attachment requires tmux
//! 3.7 or newer, a pinned socket and executable, and a fresh identity and
//! framing challenge completed on the attached connection.

use std::os::unix::process::CommandExt as _;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

use libtmux_core::control::{
    Completion, ControlIdentity, ControlProbe, RequestKind, Tracker, fence_command,
};
use libtmux_core::{Command, CreationNonce, SessionId};

use crate::{RequestOptions, Server, fresh_nonce, supervisor};

mod error;
mod owner;
mod pin;

pub use error::ControlError;
pub use libtmux_core::control::{
    ControlBlock, ControlCommandRefusal, ControlFault, ControlLimits, ControlNotice, ControlReply,
    Event,
};

const INTERVAL: Duration = Duration::from_millis(5);
const QUEUED: u8 = 0;
const WRITING: u8 = 1;
const CANCELLED_BEFORE: u8 = 2;
const DRAINING: u8 = 3;
const FINISHED: u8 = 4;

/// A verified persistent client and its sole bounded notice receiver.
///
/// Requests are serialized through private drain fences. A cancelled caller
/// keeps its place until the fence is read or the connection is closed.
///
/// # Examples
///
/// ```no_run
/// use libtmux_blocking::{Server, control::ControlMode};
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let server = Server::at("/tmp/libtmux-rs-dev/example/socket")?;
/// let control = ControlMode::attach(&server, "$0".parse()?)?;
/// control.shutdown()?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct ControlMode {
    sender: ControlSender,
    events: ControlEvents,
}

impl ControlMode {
    /// Attach to one exact session after qualifying its pinned connection.
    ///
    /// # Errors
    ///
    /// Refuses unsupported versions, failed probes, changed endpoints,
    /// incompatible protocol behaviour and incomplete attachment or cleanup.
    pub fn attach(server: &Server, session: SessionId) -> Result<Self, ControlError> {
        Self::attach_with_options(
            server,
            session,
            ControlLimits::default(),
            RequestOptions::default(),
        )
    }

    /// Attach with explicit memory, admission and cancellation-drain limits.
    ///
    /// # Errors
    ///
    /// Returns the same qualification and cleanup failures as [`Self::attach`].
    pub fn attach_with_limits(
        server: &Server,
        session: SessionId,
        limits: ControlLimits,
    ) -> Result<Self, ControlError> {
        Self::attach_with_options(server, session, limits, RequestOptions::default())
    }

    /// Attach under one absolute deadline and cancellation signal.
    ///
    /// # Errors
    ///
    /// Qualification failures precede caller command submission. A spawned
    /// control child remains owned until process-group cleanup and reaping.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "attachment consumes one session and request policy for the owned connection"
    )]
    pub fn attach_with_options(
        server: &Server,
        session: SessionId,
        limits: ControlLimits,
        options: RequestOptions,
    ) -> Result<Self, ControlError> {
        supervisor::check_before_spawn(&options)?;
        let permit = server.children.reserve_persistent()?;
        let initial = probe(server, session.clone(), &options)?;
        let endpoint = pin::PinnedEndpoint::new(&server.socket, &server.executable)
            .map_err(|error| ControlError::native(error, false))?;
        let pinned = Server {
            socket: endpoint.socket(),
            executable: endpoint.executable().as_os_str().to_owned(),
            children: server.children.clone(),
        };
        qualify_client(&pinned, &options)?;
        let identity = probe(&pinned, session.clone(), &options)?;
        if identity != initial {
            return Err(ControlError::protocol(
                ControlFault::InvalidChallenge,
                false,
            ));
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                tracker: Tracker::new(session.clone(), limits),
                ready: false,
                terminal: None,
                cleanup: None,
            }),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
            watching: AtomicBool::new(true),
            handles: AtomicUsize::new(2),
            next_token: AtomicU64::new(1),
            available: Arc::new(AtomicUsize::new(limits.max_pending_requests)),
            limits,
            identity,
        });
        let (tx, rx) = mpsc::sync_channel(limits.max_pending_requests);
        let mut command = pinned.command([
            "-N",
            "-C",
            "attach-session",
            "-E",
            "-t",
            &format!("{session}:"),
        ]);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0);
        let owned = Arc::clone(&shared);
        let opening = options.clone();
        std::thread::Builder::new()
            .name("libtmux blocking control".into())
            .spawn(move || owner::run(command, endpoint, permit, &owned, &rx, &opening))
            .map_err(|error| ControlError::native(error, false))?;
        let mode = Self {
            sender: ControlSender {
                shared: Arc::clone(&shared),
                tx,
            },
            events: ControlEvents {
                shared,
                finished: false,
            },
        };
        mode.sender
            .shared
            .await_opening(&options)
            .map_err(ControlError::before_submission)?;
        Ok(mode)
    }

    /// Separate command submission from the sole notice receiver.
    #[must_use]
    pub fn split(self) -> (ControlSender, ControlEvents) {
        (self.sender, self.events)
    }

    /// Send one logical command and collect its complete input reply blocks.
    ///
    /// # Errors
    ///
    /// Returns encoding, admission, cancellation, framing or transport errors.
    pub fn send(&self, command: Command) -> Result<ControlReply, ControlError> {
        self.sender.send(command)
    }

    /// Receive the next notice, gap or terminal connection failure.
    ///
    /// # Errors
    ///
    /// A failed connection returns its retained cause after queued notices.
    pub fn next_event(&mut self) -> Result<Option<ControlNotice>, ControlError> {
        self.events.next_event()
    }

    /// Stop the owned client and wait a bounded time for cleanup evidence.
    ///
    /// # Errors
    ///
    /// Reports signal, reap or bounded cleanup-reporting failures.
    pub fn shutdown(&self) -> Result<(), ControlError> {
        self.sender.shutdown()
    }
}

/// A clonable command handle sharing bounded admission and one wire owner.
///
/// # Examples
///
/// ```no_run
/// use libtmux_core::Command;
/// use libtmux_blocking::control::ControlSender;
/// # fn example(sender: &ControlSender) -> Result<(), Box<dyn std::error::Error>> {
/// let reply = sender.send(Command::new("list-sessions").arg("-F").arg("#{session_id}"))?;
/// # let _ = reply;
/// # Ok(())
/// # }
/// ```
pub struct ControlSender {
    shared: Arc<Shared>,
    tx: mpsc::SyncSender<Request>,
}

impl ControlSender {
    /// Dispatch under the default absolute request budget.
    ///
    /// # Errors
    ///
    /// Returns the same classified failures as [`Self::send_with`].
    pub fn send(&self, command: Command) -> Result<ControlReply, ControlError> {
        self.send_with(command, RequestOptions::default())
    }

    /// Dispatch without allowing a cancelled reply to answer another caller.
    ///
    /// Raw commands return their output once the whole reply is drained. If an
    /// after-hook stalls, an interrupted caller can lose output from a command
    /// that already ran. Use [`Server::new_session_with`](crate::Server::new_session_with)
    /// to retain a verified creation receipt across an interruption.
    ///
    /// # Errors
    ///
    /// Refusal before the owner commits to writing is `NotSubmitted`.
    /// Cancellation or a lost drain after that point is `Unknown`; the
    /// connection drains the tombstone under its original limits or closes.
    /// Scripts, unqualified waits and arbitrary output are refused with
    /// [`ControlError::policy_refusal`] before admission.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "dispatch consumes one logical command and its request policy"
    )]
    pub fn send_with(
        &self,
        command: Command,
        options: RequestOptions,
    ) -> Result<ControlReply, ControlError> {
        self.send_kind(&command, &options, RequestKind::Ordinary)
    }

    fn send_kind(
        &self,
        command: &Command,
        options: &RequestOptions,
        kind: RequestKind,
    ) -> Result<ControlReply, ControlError> {
        supervisor::check_before_spawn(options)?;
        if let Some((capability, needs)) = command.version_requirement() {
            self.shared
                .identity
                .version()
                .require(capability, needs)
                .map_err(libtmux_core::Error::from)?;
        }
        for (_, first, fixed) in command.refused_intervals() {
            if self.shared.identity.version().has_behavior(first)
                && !self.shared.identity.version().has_behavior(fixed)
            {
                return Err(libtmux_core::Error::UnsupportedVersion.into());
            }
        }
        let nonce = fresh_nonce(options)?;
        let bytes = wire(command, nonce, self.shared.limits.max_line_bytes)?;
        let words: Vec<_> = command.logical_words().collect();
        libtmux_core::control::require_shared_control(&words)?;
        if self.shared.stop.load(Ordering::Acquire) {
            return Err(ControlError::protocol(ControlFault::Closed, false));
        }
        if let Some(error) = &self.shared.lock().terminal {
            return Err(error.clone().before_submission());
        }
        let slot = supervisor::reserve(
            &self.shared.available,
            self.shared.limits.max_pending_requests,
        )?;
        let phase = Arc::new(AtomicU8::new(QUEUED));
        let (tx, rx) = mpsc::sync_channel(1);
        let request = Request {
            token: self.shared.next_token.fetch_add(1, Ordering::Relaxed),
            nonce,
            kind,
            bytes,
            phase: Arc::clone(&phase),
            options: options.clone(),
            tx,
            _slot: slot,
        };
        self.tx.try_send(request).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => ControlError::protocol(
                ControlFault::Busy {
                    limit: self.shared.limits.max_pending_requests,
                },
                false,
            ),
            mpsc::TrySendError::Disconnected(_) => {
                ControlError::protocol(ControlFault::Closed, false)
            }
        })?;
        loop {
            match rx.recv_timeout(INTERVAL) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let error = self.shared.lock().terminal.clone().unwrap_or_else(|| {
                        ControlError::protocol(ControlFault::Disconnected, true)
                    });
                    return Err(
                        if matches!(phase.load(Ordering::Acquire), QUEUED | CANCELLED_BEFORE) {
                            error.before_submission()
                        } else {
                            error
                        },
                    );
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if options.cancellation.is_cancelled() || Instant::now() >= options.budget.deadline {
                let submitted = cancel(&phase);
                let fault = if options.cancellation.is_cancelled() {
                    ControlFault::Cancelled
                } else {
                    ControlFault::Deadline
                };
                return Err(ControlError::protocol(fault, submitted));
            }
        }
    }

    /// Stop the shared client and wait for bounded cleanup reporting.
    ///
    /// # Errors
    ///
    /// Retains process-group signal, reap and pending-cleanup evidence.
    pub fn shutdown(&self) -> Result<(), ControlError> {
        self.shared.shutdown()
    }

    /// Pause output for an exact pane on this pinned connection.
    ///
    /// # Errors
    ///
    /// Returns the same admission, cancellation and transport errors as [`Self::send`].
    pub fn pause_pane(&self, pane: libtmux_core::PaneId) -> Result<ControlReply, ControlError> {
        self.send_kind(
            &Command::new("refresh-client")
                .arg("-A")
                .arg(format!("{pane}:pause")),
            &RequestOptions::default(),
            RequestKind::Pause(pane),
        )
    }

    /// Continue output and publish the acknowledged pane boundary.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::pause_pane`].
    pub fn continue_pane(&self, pane: libtmux_core::PaneId) -> Result<ControlReply, ControlError> {
        self.send_kind(
            &Command::new("refresh-client")
                .arg("-A")
                .arg(format!("{pane}:continue")),
            &RequestOptions::default(),
            RequestKind::Continue(pane),
        )
    }
}

impl Clone for ControlSender {
    fn clone(&self) -> Self {
        self.shared.handles.fetch_add(1, Ordering::Relaxed);
        Self {
            shared: Arc::clone(&self.shared),
            tx: self.tx.clone(),
        }
    }
}

impl Drop for ControlSender {
    fn drop(&mut self) {
        if self.shared.handles.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.shared.stop.store(true, Ordering::Release);
        }
    }
}

/// The sole consumer of bounded notifications and explicit loss boundaries.
///
/// Slow or dropped subscribers never stop reply collection.
///
/// # Examples
///
/// ```no_run
/// use libtmux_blocking::control::ControlEvents;
/// # fn example(events: &mut ControlEvents) -> Result<(), Box<dyn std::error::Error>> {
/// let next = events.next_event()?;
/// # let _ = next;
/// # Ok(())
/// # }
/// ```
pub struct ControlEvents {
    shared: Arc<Shared>,
    finished: bool,
}

impl std::fmt::Debug for ControlSender {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlSender")
            .field("closed", &self.shared.stop.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for ControlEvents {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlEvents")
            .field("closed", &self.shared.stop.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl ControlEvents {
    /// Wait for the next notice, gap, clean shutdown or terminal error.
    ///
    /// # Errors
    ///
    /// Returns the terminal connection or cleanup error once, after queued
    /// notices. Subsequent calls return `Ok(None)`.
    pub fn next_event(&mut self) -> Result<Option<ControlNotice>, ControlError> {
        if self.finished {
            return Ok(None);
        }
        loop {
            let mut state = self.shared.lock();
            if let Some(notice) = state.tracker.next_notice() {
                return Ok(Some(notice));
            }
            if let Some(result) = &state.cleanup {
                self.finished = true;
                return result.clone().map(|()| None);
            }
            drop(state);
            self.shared.wait(INTERVAL);
        }
    }

    /// Stop the shared connection and wait for bounded cleanup evidence.
    ///
    /// # Errors
    ///
    /// Returns signal, reap or pending-cleanup errors.
    pub fn shutdown(&self) -> Result<(), ControlError> {
        self.shared.shutdown()
    }
}

impl Drop for ControlEvents {
    fn drop(&mut self) {
        self.shared.watching.store(false, Ordering::Release);
        self.shared.lock().tracker.discard_notices();
        if self.shared.handles.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.shared.stop.store(true, Ordering::Release);
        }
    }
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    stop: AtomicBool,
    watching: AtomicBool,
    handles: AtomicUsize,
    next_token: AtomicU64,
    available: Arc<AtomicUsize>,
    limits: ControlLimits,
    identity: ControlIdentity,
}

struct State {
    tracker: Tracker,
    ready: bool,
    terminal: Option<ControlError>,
    cleanup: Option<Result<(), ControlError>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn wait(&self, duration: Duration) {
        let _guard = self
            .changed
            .wait_timeout(self.lock(), duration)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    fn await_opening(&self, options: &RequestOptions) -> Result<(), ControlError> {
        loop {
            let state = self.lock();
            if state.ready {
                return Ok(());
            }
            if let Some(error) = &state.terminal {
                let error = error.clone();
                drop(state);
                let reported = self.shutdown().err().unwrap_or(error);
                return Err(reported);
            }
            drop(state);
            if options.cancellation.is_cancelled() || Instant::now() >= options.budget.deadline {
                let fault = if options.cancellation.is_cancelled() {
                    ControlFault::Cancelled
                } else {
                    ControlFault::Deadline
                };
                self.stop.store(true, Ordering::Release);
                return Err(self
                    .shutdown()
                    .err()
                    .unwrap_or_else(|| ControlError::protocol(fault, false)));
            }
            self.wait(INTERVAL);
        }
    }

    fn shutdown(&self) -> Result<(), ControlError> {
        self.stop.store(true, Ordering::Release);
        let deadline = Instant::now() + libtmux_core::supervision::CLEANUP_ALLOWANCE;
        loop {
            let state = self.lock();
            if let Some(result) = &state.cleanup {
                return result.clone();
            }
            let primary = state.terminal.clone();
            drop(state);
            if Instant::now() >= deadline {
                return Err(primary
                    .unwrap_or_else(|| ControlError::protocol(ControlFault::Closed, false))
                    .pending());
            }
            self.wait(INTERVAL);
        }
    }
}

struct Request {
    token: u64,
    nonce: CreationNonce,
    kind: RequestKind,
    bytes: Vec<u8>,
    phase: Arc<AtomicU8>,
    options: RequestOptions,
    tx: mpsc::SyncSender<Result<ControlReply, ControlError>>,
    _slot: supervisor::Permit,
}

fn cancel(phase: &AtomicU8) -> bool {
    match phase.compare_exchange(
        QUEUED,
        CANCELLED_BEFORE,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) | Err(CANCELLED_BEFORE) => false,
        Err(_) => {
            let _ = phase.compare_exchange(WRITING, DRAINING, Ordering::AcqRel, Ordering::Acquire);
            true
        }
    }
}

fn wire(command: &Command, nonce: CreationNonce, limit: usize) -> Result<Vec<u8>, ControlError> {
    let mut minimum = 0usize;
    for word in command.logical_words() {
        let bytes = word.as_encoded_bytes();
        if word.to_str().is_none() || bytes.iter().any(|byte| matches!(byte, 0 | b'\r' | b'\n')) {
            return Err(ControlError::protocol(ControlFault::InvalidCommand, false));
        }
        minimum = minimum.saturating_add(bytes.len().saturating_add(1));
        if minimum > limit {
            return Err(ControlError::protocol(
                ControlFault::CommandLimit { limit },
                false,
            ));
        }
    }
    let mut line = command
        .control_mode_line_bounded(limit.saturating_sub(1))
        .ok_or_else(|| ControlError::protocol(ControlFault::CommandLimit { limit }, false))?;
    let fence = fence_command(nonce)
        .control_mode_line_bounded(limit.saturating_sub(1))
        .ok_or_else(|| ControlError::protocol(ControlFault::CommandLimit { limit }, false))?;
    line.push('\n');
    line.push_str(&fence);
    line.push('\n');
    Ok(line.into_bytes())
}

fn run_probe(
    options: &RequestOptions,
    mut dispatch: impl FnMut() -> Result<(std::process::Output, bool), libtmux_core::Error>,
) -> Result<(std::process::Output, bool), libtmux_core::Error> {
    loop {
        match dispatch() {
            // A fork can hold the executable open for writing until exec.
            // ETXTBSY confirms the probe never ran.
            Err(libtmux_core::Error::Spawn(source))
                if source.kind() == std::io::ErrorKind::ExecutableFileBusy =>
            {
                supervisor::check_before_spawn(options)?;
                std::thread::sleep(
                    options
                        .budget
                        .deadline
                        .saturating_duration_since(Instant::now())
                        .min(INTERVAL),
                );
            }
            result => return result.map_err(crate::before_submission),
        }
    }
}

fn qualify_client(server: &Server, options: &RequestOptions) -> Result<(), ControlError> {
    let (output, late) = run_probe(options, || server.output(["-V"], options.clone(), None))?;
    if late || !output.status.success() {
        return Err(ControlError::protocol(ControlFault::AttachRejected, false));
    }
    let client =
        libtmux_core::TmuxVersion::parse_executable_output(&server.executable, &output.stdout)
            .map_err(libtmux_core::Error::from)?;
    client
        .ensure_supported()
        .map_err(libtmux_core::Error::from)?;
    Ok(())
}

fn probe(
    server: &Server,
    session: SessionId,
    options: &RequestOptions,
) -> Result<ControlIdentity, ControlError> {
    let probe = ControlProbe::new(session, fresh_nonce(options)?);
    let (output, late) = run_probe(options, || {
        server
            .children
            .dispatch_probe(server.command(probe.arguments()), options.clone())
    })?;
    Ok(probe.decode_ordinary(output.status.code(), &output.stdout, &output.stderr, late)?)
}

#[cfg(test)]
mod tests;
