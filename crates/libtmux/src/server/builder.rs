use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::Server;
use crate::internal::core::{BuildContext, Core, CoreConfiguration, SocketSelection};
use crate::{DispatchLimits, Error, OutputLimits, ServerConfigurationErrorKind};

/// A consuming builder for one inert [`Server`] handle.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use libtmux::{DispatchLimits, OutputLimits, Server};
/// use std::time::Duration;
///
/// // Naming a socket and a budget is the whole of the configuration; everything
/// // else is per-command.
/// let server = Server::builder()
///     .socket_name("builder-example")
///     .default_timeout(Duration::from_secs(5))
///     .output_limits(OutputLimits::default().max_stdout_bytes(1024 * 1024))
///     .dispatch_limits(DispatchLimits::default().max_in_flight(4))
///     .build()?;
///
/// // Building does not start tmux, so this has not touched the machine yet.
/// let _ = server.identity();
/// # Ok(())
/// # }
/// ```
#[must_use = "a server builder has no effect until build is called"]
pub struct ServerBuilder {
    socket_name: Option<OsString>,
    socket_path: Option<PathBuf>,
    config_file: Option<PathBuf>,
    colors: Option<u16>,
    executable: OsString,
    timeout: Duration,
    output_limits: OutputLimits,
    dispatch_limits: DispatchLimits,
    #[cfg(feature = "test-support")]
    prevent_server_start: bool,
}

// Redacted like `ServerIdentity`'s, and for the same reason: a builder holds
// the socket path and the config path, and a caller who prints one while
// debugging should not put either into a log.
impl fmt::Debug for ServerBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerBuilder")
            .field(
                "socket_name",
                &self.socket_name.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "socket_path",
                &self.socket_path.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "config_file",
                &self.config_file.as_ref().map(|_| "<redacted>"),
            )
            .field("colors", &self.colors)
            .field("timeout", &self.timeout)
            .field("output_limits", &self.output_limits)
            .field("dispatch_limits", &self.dispatch_limits)
            .finish_non_exhaustive()
    }
}

impl ServerBuilder {
    pub(super) fn new() -> Self {
        Self {
            socket_name: None,
            socket_path: None,
            config_file: None,
            colors: None,
            executable: OsString::from("tmux"),
            timeout: CoreConfiguration::default_timeout(),
            output_limits: OutputLimits::default(),
            dispatch_limits: DispatchLimits::default(),
            #[cfg(feature = "test-support")]
            prevent_server_start: false,
        }
    }

    /// Bound how many bytes one command may read from tmux.
    ///
    /// tmux answers with as many bytes as it has: a pane with a long history,
    /// a buffer holding a pasted file, a `run-shell` that keeps printing.
    /// Without a ceiling the operating system decides when to stop, and it
    /// does that by killing this process.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::OutputLimits;
    ///
    /// let server = libtmux::Server::builder()
    ///     .output_limits(OutputLimits::default().max_stdout_bytes(1024 * 1024))
    ///     .build()?;
    /// # let _ = server;
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the limits"]
    pub const fn output_limits(mut self, limits: OutputLimits) -> Self {
        self.output_limits = limits;
        self
    }

    /// Bound how many commands may run at once.
    ///
    /// Each one is a tmux client process with its own pipes and reader tasks,
    /// and tmux serializes them on the far side anyway, so past a point more
    /// clients buy queueing rather than throughput. A caller that fans out
    /// wide -- an agent, a reconciler sweeping every pane -- otherwise turns
    /// its own concurrency into pressure on the machine.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::DispatchLimits;
    ///
    /// let server = libtmux::Server::builder()
    ///     .dispatch_limits(DispatchLimits::default().max_in_flight(4))
    ///     .build()?;
    /// # let _ = server;
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the limits"]
    pub const fn dispatch_limits(mut self, limits: DispatchLimits) -> Self {
        self.dispatch_limits = limits;
        self
    }

    /// Select a named tmux socket.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder().socket_name("example").build()?;
    /// assert_eq!(server.socket_name(), Some(std::ffi::OsStr::new("example")));
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the socket name"]
    pub fn socket_name(mut self, name: impl Into<OsString>) -> Self {
        self.socket_name = Some(name.into());
        self
    }

    /// Select an explicit tmux socket path.
    ///
    /// Relative paths are joined to the working directory captured by
    /// [`ServerBuilder::build`].
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/explicit-example.sock")
    ///     .build()?;
    /// assert!(server.socket_path().is_absolute());
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the socket path"]
    pub fn socket_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.socket_path = Some(path.into());
        self
    }

    /// Select a tmux configuration file.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder()
    ///     .config_file("/tmp/libtmux-config-example.conf")
    ///     .build()?;
    /// assert!(server.config_file().is_some());
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the config path"]
    pub fn config_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_file = Some(path.into());
        self
    }

    /// Select tmux's 88- or 256-color compatibility mode.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder().colors(88).build()?;
    /// assert_eq!(server.colors(), Some(88));
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the color mode"]
    pub const fn colors(mut self, colors: u16) -> Self {
        self.colors = Some(colors);
        self
    }

    /// Select the tmux executable without checking that it exists yet.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder().tmux_executable("tmux").build()?;
    /// assert_eq!(server.tmux_executable(), std::ffi::OsStr::new("tmux"));
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the executable"]
    pub fn tmux_executable(mut self, executable: impl Into<OsString>) -> Self {
        self.executable = executable.into();
        self
    }

    /// Set the per-command dispatch deadline.
    ///
    /// The deadline includes waiting for dispatch capacity and running the
    /// tmux process.
    ///
    /// A duration too large for the platform's monotonic clock is treated as
    /// unbounded while command cancellation remains available.
    ///
    /// # Examples
    ///
    /// ```
    /// let timeout = std::time::Duration::from_secs(4);
    /// let server = libtmux::Server::builder().default_timeout(timeout).build()?;
    /// assert_eq!(server.default_timeout(), timeout);
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the timeout"]
    pub const fn default_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[cfg(feature = "test-support")]
    pub(crate) const fn prevent_server_start(mut self) -> Self {
        self.prevent_server_start = true;
        self
    }

    /// Capture process context and construct an inert server handle.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidServerConfiguration`] for invalid selector,
    /// path, color, working-directory, or socket-root inputs.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder().build()?;
    /// assert!(server.socket_path().is_absolute());
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    pub fn build(self) -> Result<Server, Error> {
        let selection = match (self.socket_name, self.socket_path) {
            (Some(name), None) => SocketSelection::Name(name),
            (None, Some(path)) => SocketSelection::Path(path),
            (None, None) => SocketSelection::Automatic,
            (Some(_), Some(_)) => {
                return Err(Error::invalid_server_configuration(
                    ServerConfigurationErrorKind::ConflictingSocketSelectors,
                ));
            }
        };
        let configuration = CoreConfiguration::resolve(
            &selection,
            self.config_file,
            self.colors,
            self.executable,
            self.timeout,
            BuildContext::capture(),
        )
        .map_err(Error::invalid_server_configuration)?
        .with_limits(self.output_limits, self.dispatch_limits);
        #[cfg(feature = "test-support")]
        let configuration = if self.prevent_server_start {
            configuration.prevent_server_start()
        } else {
            configuration
        };
        Ok(Server {
            core: Arc::new(Core::new(configuration)),
        })
    }
}
