//! Native blocking subprocess supervision for an explicit tmux socket.
//!
//! Requests share the validated operations, object IDs and reply decoders in
//! `libtmux-core`. Ordinary subprocess requests preserve the tmux 3.3a floor
//! without acquiring a control client or attaching to a session.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use libtmux_core::supervision::Budget;
use libtmux_core::{
    CommandKind, CreatedSession, CreationNonce, Error, NewSession, OrdinaryPaneObservation,
    OrdinaryPaneProbe, PaneId, PaneKey, Reply,
};

mod supervisor;

/// A cancellation signal shared by the caller and its native subprocess.
///
/// Cancelling before spawn leaves the request unsubmitted. Cancelling after
/// spawn stops the owned process group and preserves any complete creation
/// receipt collected before the interruption.
///
/// # Examples
///
/// ```
/// use libtmux_blocking::CancellationToken;
///
/// let cancellation = CancellationToken::default();
/// let caller = cancellation.clone();
/// caller.cancel();
/// assert!(cancellation.is_cancelled());
/// ```
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    /// Request cancellation for every holder of this token.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// The absolute deadline, stream byte limits and cancellation of one request.
///
/// The deadline includes dispatch and collection. Cleanup has a separate,
/// bounded reporting allowance; an unreaped child retains its admission slot.
///
/// # Examples
///
/// ```
/// use libtmux_blocking::RequestOptions;
///
/// let request = RequestOptions::default();
/// request.cancellation.cancel();
/// assert!(request.cancellation.is_cancelled());
/// ```
#[derive(Clone)]
pub struct RequestOptions {
    /// The deadline and maximum number of bytes collected from each stream.
    pub budget: Budget,
    /// The signal that interrupts collection and starts process cleanup.
    pub cancellation: CancellationToken,
}

impl std::fmt::Debug for RequestOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RequestOptions")
            .field("deadline", &self.budget.deadline)
            .field("bytes_per_stream", &self.budget.bytes_per_stream)
            .field("cancellation", &self.cancellation)
            .finish()
    }
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            budget: Budget::starting_at(Instant::now()),
            cancellation: CancellationToken::default(),
        }
    }
}

/// A native blocking client bound to one absolute Unix socket path.
///
/// Clones share subprocess admission. No async runtime is constructed or
/// required. Creation, observation and capture use ordinary subprocesses;
/// these methods never acquire a control client implicitly.
///
/// # Examples
///
/// ```
/// use libtmux_blocking::Server;
///
/// let server = Server::at("/tmp/libtmux-rs-dev/example/socket")?;
/// # let _ = server;
/// # Ok::<_, std::io::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct Server {
    socket: PathBuf,
    executable: OsString,
    children: supervisor::Children,
}

impl Server {
    /// Bind the client to an absolute socket without connecting to it.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error for a relative socket path.
    pub fn at(socket: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let socket = socket.as_ref();
        if !socket.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "expected an absolute tmux socket path",
            ));
        }
        Ok(Self {
            socket: socket.to_owned(),
            executable: "tmux".into(),
            children: supervisor::Children::default(),
        })
    }

    /// Choose the tmux client executable for subsequent requests.
    #[must_use]
    pub fn with_executable(mut self, executable: impl Into<OsString>) -> Self {
        self.executable = executable.into();
        self
    }

    /// Create a detached session and retain its generated object identities.
    ///
    /// # Errors
    ///
    /// A dispatched failure preserves its submission, creation and cleanup
    /// evidence in [`Error`]. Entropy and startup failures precede submission.
    pub fn new_session(&self, request: NewSession) -> Result<CreatedSession, Error> {
        self.new_session_with(request, RequestOptions::default())
    }

    /// Create a detached session under an explicit deadline and cancellation.
    ///
    /// # Errors
    ///
    /// Returns the same classified failures as [`Self::new_session`], including
    /// cancellation before spawn and complete receipts collected before interruption.
    pub fn new_session_with(
        &self,
        request: NewSession,
        options: RequestOptions,
    ) -> Result<CreatedSession, Error> {
        let nonce = fresh_nonce(&options)?;
        let (output, late) =
            self.output(request.ordinary_arguments(nonce), options, Some(nonce))?;
        Reply::new(
            output.status.code(),
            output.stdout,
            output.stderr.len(),
            late,
        )
        .ordinary_creation(request, &self.socket, nonce)
    }

    /// Observe an exact pane ID and bind it to its daemon generation.
    ///
    /// # Errors
    ///
    /// Returns an observation failure when the exact pane, supported daemon
    /// version or complete metadata reply cannot be established.
    pub fn observe_pane(&self, pane: PaneId) -> Result<PaneKey, Error> {
        self.observe_pane_with(pane, RequestOptions::default())
    }

    /// Observe an exact pane under an explicit deadline and cancellation.
    ///
    /// # Errors
    ///
    /// Returns the same classified failures as [`Self::observe_pane`].
    pub fn observe_pane_with(
        &self,
        pane: PaneId,
        options: RequestOptions,
    ) -> Result<PaneKey, Error> {
        self.probe_pane(pane, options)
            .map(|observed| observed.key().clone())
    }

    /// Capture bytes only while the pane's socket and generation still match.
    ///
    /// # Errors
    ///
    /// Rejects a key from another socket or a stale daemon, and discards
    /// incomplete, failed or late observation bytes.
    pub fn capture(&self, pane: &PaneKey) -> Result<Vec<u8>, Error> {
        self.capture_with(pane, RequestOptions::default())
    }

    /// Capture a generation-checked pane under explicit request limits.
    ///
    /// # Errors
    ///
    /// Returns the same classified failures as [`Self::capture`].
    pub fn capture_with(&self, pane: &PaneKey, options: RequestOptions) -> Result<Vec<u8>, Error> {
        if pane.server_identity().socket_path() != self.socket {
            return Err(Error::WrongEndpoint);
        }
        let observed = self.probe_pane(pane.id(), options.clone())?;
        let (output, late) = self
            .output(
                observed.capture_arguments(&self.socket, pane)?,
                options,
                None,
            )
            .map_err(|error| error.during_read(CommandKind::CapturePane))?;
        observed.decode_capture(output.status.code(), output.stdout, &output.stderr, late)
    }

    fn probe_pane(
        &self,
        pane: PaneId,
        options: RequestOptions,
    ) -> Result<OrdinaryPaneObservation, Error> {
        let probe = OrdinaryPaneProbe::new(pane, fresh_nonce(&options)?);
        let (output, late) = self
            .output(probe.arguments(), options, None)
            .map_err(|error| error.during_read(CommandKind::ListPanes))?;
        probe.decode(
            &self.socket,
            output.status.code(),
            &output.stdout,
            &output.stderr,
            late,
        )
    }

    fn output(
        &self,
        args: impl IntoIterator<Item = impl AsRef<OsStr>>,
        options: RequestOptions,
        creation_nonce: Option<CreationNonce>,
    ) -> Result<(Output, bool), Error> {
        let mut command = std::process::Command::new(&self.executable);
        command
            .arg("-S")
            .arg(&self.socket)
            .args(args)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("LIBTMUX_SOCKET")
            .env_remove("LIBTMUX_SOCKET_PATH");
        self.children.dispatch_ordinary(
            command,
            options,
            creation_nonce.map(|nonce| (self.socket.clone(), nonce)),
        )
    }
}

fn fresh_nonce(options: &RequestOptions) -> Result<CreationNonce, Error> {
    supervisor::check_before_spawn(options)?;
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| Error::Spawn(std::io::Error::other(error.to_string())))?;
    Ok(CreationNonce::from_bytes(bytes))
}

#[cfg(test)]
#[path = "../tests/common/native.rs"]
mod native_contracts;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_socket_cannot_select_an_ambient_server() {
        let error = Server::at("relative/socket").expect_err("relative socket is refused");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn cancelled_creation_does_not_start_the_client() {
        let server = Server::at("/tmp/libtmux-rs-test/unqualified/socket")
            .expect("absolute socket")
            .with_executable("/libtmux-blocking-missing-executable");
        let options = RequestOptions::default();
        options.cancellation.cancel();
        let error = server
            .new_session_with(NewSession::new("refused"), options)
            .expect_err("request was cancelled before spawn");
        assert!(matches!(error, Error::CancelledBeforeSpawn));
        assert_eq!(error.effect(), libtmux_core::Effect::NotSubmitted);
    }
}
