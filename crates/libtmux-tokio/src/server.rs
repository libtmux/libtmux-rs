mod operations;

use std::ffi::OsString;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::{DispatchLimits, Error, OutputLimits, TokioRuntime};

/// A shared client handle configured to use native Tokio I/O.
///
/// All operations and returned handles use the shared `libtmux` object model.
/// This wrapper selects Tokio at construction; it does not run a blocking
/// client on a thread pool.
///
/// ```no_run
/// # async fn example() -> Result<(), libtmux_tokio::Error> {
/// let server = libtmux_tokio::Server::new()?;
/// for session in server.sessions().await? { println!("{}", session.id()); }
/// server.shutdown().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Server(pub(crate) libtmux::Server);

impl Server {
    /// Construct a server using the captured default endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if the working directory or socket root cannot be captured.
    pub fn new() -> Result<Self, Error> {
        Self::builder().build()
    }

    /// Configure an inert server that uses Tokio for every operation.
    pub fn builder() -> ServerBuilder {
        ServerBuilder(libtmux::Server::builder().runtime(Arc::new(TokioRuntime)))
    }

    /// Construct a server from the process's `TMUX` value.
    ///
    /// # Errors
    ///
    /// Returns an error for an absent or malformed `TMUX` value.
    pub fn from_env() -> Result<Self, Error> {
        Self::from_env_value(std::env::var_os("TMUX"))
    }

    /// Construct a server from an explicit `TMUX` value.
    ///
    /// # Errors
    ///
    /// Returns an error for an absent or malformed value or an unusable socket path.
    pub fn from_env_value(value: Option<impl Into<OsString>>) -> Result<Self, Error> {
        libtmux::Server::from_env_value_with_runtime(value, Arc::new(TokioRuntime)).map(Self)
    }

    /// Route this server's operations through an existing control connection.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection belongs to another server or capability probing fails.
    #[cfg(feature = "control-mode")]
    pub async fn over_control_mode(
        &self,
        sender: &crate::control::ControlSender,
    ) -> Result<Self, Error> {
        self.0.over_control_mode(sender).await.map(Self)
    }

    /// Consume this constructor wrapper and return the shared handle.
    #[must_use]
    pub fn into_inner(self) -> libtmux::Server {
        self.0
    }
}

impl Deref for Server {
    type Target = libtmux::Server;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl AsRef<libtmux::Server> for Server {
    fn as_ref(&self) -> &libtmux::Server {
        &self.0
    }
}

/// Configures the shared server while retaining the Tokio driver.
///
/// ```
/// let server = libtmux_tokio::Server::builder().socket_name("example").build()?;
/// assert_eq!(server.socket_name(), Some(std::ffi::OsStr::new("example")));
/// # Ok::<(), libtmux_tokio::Error>(())
/// ```
#[derive(Debug)]
#[must_use = "call build to construct the server"]
pub struct ServerBuilder(libtmux::ServerBuilder);

impl ServerBuilder {
    /// Select a named tmux socket.
    pub fn socket_name(self, name: impl Into<OsString>) -> Self {
        Self(self.0.socket_name(name))
    }
    /// Select an explicit socket path.
    pub fn socket_path(self, path: impl Into<PathBuf>) -> Self {
        Self(self.0.socket_path(path))
    }
    /// Select a tmux configuration file.
    pub fn config_file(self, path: impl Into<PathBuf>) -> Self {
        Self(self.0.config_file(path))
    }
    /// Request the 256-color compatibility mode.
    pub fn colors(self, colors: u16) -> Self {
        Self(self.0.colors(colors))
    }
    /// Select the tmux executable.
    pub fn tmux_executable(self, executable: impl Into<OsString>) -> Self {
        Self(self.0.tmux_executable(executable))
    }
    /// Set the deadline for command dispatches.
    pub fn default_timeout(self, timeout: Duration) -> Self {
        Self(self.0.default_timeout(timeout))
    }
    /// Bound each command's captured output.
    pub fn output_limits(self, limits: OutputLimits) -> Self {
        Self(self.0.output_limits(limits))
    }
    /// Bound concurrent command dispatches.
    pub fn dispatch_limits(self, limits: DispatchLimits) -> Self {
        Self(self.0.dispatch_limits(limits))
    }
    /// Bound persistent control-mode connections.
    #[cfg(feature = "control-mode")]
    pub fn control_client_limits(self, limits: crate::ControlClientLimits) -> Self {
        Self(self.0.control_client_limits(limits))
    }
    /// Capture process context and construct the server without starting tmux.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid endpoint, path, color, or process context.
    pub fn build(self) -> Result<Server, Error> {
        self.0.build().map(Server)
    }
}
