//! Immutable capabilities shared by handles for one configured tmux executable.

use crate::TmuxVersion;

/// Immutable capability state detected for one configured tmux executable.
///
/// Successful detection is shared by every clone of the owning
/// [`crate::Server`].
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// let guard = libtmux::test::TestServer::new().await?;
///
/// // Detected once and cached, so asking twice does not run tmux twice.
/// let capabilities = guard.server().capabilities().await?;
/// assert!(!capabilities.tmux_version().raw().is_empty());
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineCapabilities {
    tmux_version: TmuxVersion,
}

impl EngineCapabilities {
    pub(crate) fn from_tmux_version(tmux_version: TmuxVersion) -> Self {
        Self { tmux_version }
    }

    /// Return the configured tmux executable's detected version.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::new()?;
    /// let capabilities = server.capabilities().await?;
    /// assert!(!capabilities.tmux_version().raw().is_empty());
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub const fn tmux_version(&self) -> &TmuxVersion {
        &self.tmux_version
    }
}

#[cfg(test)]
mod tests {

    use static_assertions::assert_impl_all;

    use super::EngineCapabilities;
    use crate::TmuxVersion;

    assert_impl_all!(EngineCapabilities: Clone, std::fmt::Debug, Eq, Send, Sync);

    fn version(output: &[u8]) -> TmuxVersion {
        TmuxVersion::parse_output(output).expect("fixture is a valid tmux version output")
    }

    #[test]
    fn stores_detected_version_without_transformation() {
        let version = version(b"tmux 3.7b\n");
        let capabilities = EngineCapabilities::from_tmux_version(version.clone());

        assert_eq!(capabilities.tmux_version(), &version);
        assert_eq!(capabilities.tmux_version().raw(), "3.7b");
    }

    #[test]
    fn preserves_development_version_without_inventing_a_release() {
        let capabilities = EngineCapabilities::from_tmux_version(version(b"tmux master\n"));

        assert_eq!(capabilities.tmux_version().raw(), "master");
        assert_eq!(capabilities.tmux_version().release(), None);
        assert!(capabilities.tmux_version().is_development());
    }

    #[test]
    fn equality_tracks_current_capability_state() {
        let final_release = EngineCapabilities::from_tmux_version(version(b"tmux 3.7\n"));
        let patch_release = EngineCapabilities::from_tmux_version(version(b"tmux 3.7b\n"));

        assert_eq!(final_release, final_release.clone());
        assert_ne!(final_release, patch_release);
    }
}
