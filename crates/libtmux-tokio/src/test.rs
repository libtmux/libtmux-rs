//! Isolated real-tmux fixtures whose clients use native Tokio I/O.

use std::ffi::OsString;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

pub use libtmux::test::*;

use crate::{DispatchLimits, OutputLimits, Server, TokioRuntime};

/// Owns an isolated foreground tmux daemon and a Tokio client.
///
/// Explicit shutdown reports cleanup failures. Dropping the guard forces
/// cleanup on a best-effort basis.
#[derive(Debug)]
pub struct TestServer {
    inner: libtmux::test::TestServer,
    server: Server,
}

impl TestServer {
    /// Start an isolated daemon with default fixture limits.
    ///
    /// # Errors
    ///
    /// Returns an error when setup, startup, or rollback fails.
    pub async fn new() -> Result<Self, TestServerError> {
        Self::builder().start().await
    }

    /// Configure an isolated daemon whose clients use Tokio.
    pub fn builder() -> TestServerBuilder {
        TestServerBuilder(libtmux::test::TestServer::builder().runtime(Arc::new(TokioRuntime)))
    }

    /// Return the client for this daemon.
    #[must_use]
    pub fn server(&self) -> &Server {
        &self.server
    }

    /// Observe whether this fixture's daemon is still running.
    pub fn daemon_state(&mut self) -> DaemonState {
        self.inner.daemon_state()
    }

    /// Stop the client and daemon, then remove their files.
    ///
    /// # Errors
    ///
    /// Returns an error if executor shutdown or daemon cleanup fails.
    ///
    /// # Cancel safety
    ///
    /// Dropping this future forces cleanup; failures then go unreported.
    pub async fn shutdown(self) -> Result<(), TestServerError> {
        self.inner.shutdown().await
    }
}

impl Deref for TestServer {
    type Target = libtmux::test::TestServer;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Configures an isolated daemon and its Tokio client.
#[derive(Debug)]
#[must_use = "call start to launch the fixture"]
pub struct TestServerBuilder(libtmux::test::TestServerBuilder);

impl TestServerBuilder {
    /// Bound the bytes captured by each command.
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

    /// Select the executable for the daemon and its clients.
    pub fn tmux_executable(self, executable: impl Into<OsString>) -> Self {
        Self(self.0.tmux_executable(executable))
    }

    /// Set the startup and graceful-shutdown observation timeout.
    pub fn lifecycle_timeout(self, timeout: Duration) -> Self {
        Self(self.0.lifecycle_timeout(timeout))
    }

    /// Start the isolated daemon.
    ///
    /// # Errors
    ///
    /// Returns an error when setup, startup, or rollback fails.
    ///
    /// # Cancel safety
    ///
    /// Dropping this future forces cleanup; failures then go unreported.
    pub async fn start(self) -> Result<TestServer, TestServerError> {
        let inner = self.0.start().await?;
        let server = Server(inner.server().clone());
        Ok(TestServer { inner, server })
    }
}
