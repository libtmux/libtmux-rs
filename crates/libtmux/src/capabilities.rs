//! Executable diagnostics and daemon-bound feature decisions.

use crate::{ReleaseVersion, TmuxVersion};

/// What libtmux does below a feature's required release.
///
/// # Examples
///
/// ```
/// use libtmux::{CapabilityDefinition, CapabilityHandling};
/// let feature = CapabilityDefinition::find("control-pane-off")
///     .ok_or("control-pane-off is not in the registry")?;
/// assert_eq!(feature.handling, CapabilityHandling::Fallback);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CapabilityHandling {
    /// Refuse the operation before dispatch.
    Refuse,
    /// Use the alternative described by the feature's summary.
    Fallback,
}

impl CapabilityHandling {
    /// Return the manifest spelling of this decision.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refuse => "refuse",
            Self::Fallback => "fallback",
        }
    }
}

/// How a daemon version can perform a named feature.
///
/// # Examples
///
/// ```
/// use libtmux::{CapabilityDefinition, CapabilitySupport, TmuxVersion};
/// let version = TmuxVersion::parse_output(b"tmux 3.3a\n")?;
/// let feature = CapabilityDefinition::find("buffer-delete-named")
///     .ok_or("buffer-delete-named is not in the registry")?;
/// assert_eq!(feature.support(&version), CapabilitySupport::Unavailable);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CapabilitySupport {
    /// The daemon carries the required behaviour.
    Available,
    /// libtmux uses the alternative described by the feature's summary.
    Fallback,
    /// libtmux refuses the feature on this version.
    Unavailable,
}

impl CapabilitySupport {
    /// Return the machine-readable spelling of this decision.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Fallback => "fallback",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Immutable metadata for one version-gated libtmux feature.
///
/// The registry also supplies native command admission and the published
/// capability manifest. It describes libtmux's behaviour, including fallbacks.
///
/// # Examples
///
/// ```
/// let feature = libtmux::CapabilityDefinition::find("buffer-delete-named")
///     .ok_or("buffer-delete-named is not in the registry")?;
/// assert_eq!(feature.since.to_string(), "3.4");
/// assert!(!feature.apis.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityDefinition {
    /// Stable kebab-case feature identity.
    pub id: &'static str,
    /// Human-readable name.
    pub name: &'static str,
    /// Behaviour and any fallback or refusal limits.
    pub summary: &'static str,
    /// First release whose behaviour libtmux accepts for this feature.
    pub since: ReleaseVersion,
    /// What libtmux does below `since`.
    pub handling: CapabilityHandling,
    /// Public error reported when the feature is refused.
    pub error: &'static str,
    /// Public API names in the generated reference.
    pub apis: &'static [&'static str],
    /// Native command names and the flags the feature depends on.
    pub commands: &'static [(&'static str, &'static [&'static str])],
}

impl CapabilityDefinition {
    /// Return every definition in registry order, without contacting tmux.
    #[must_use]
    pub const fn all() -> &'static [&'static Self] {
        crate::internal::support::capabilities::CAPABILITIES
    }

    /// Find a known feature; an unknown identity returns `None`.
    #[must_use]
    pub fn find(id: &str) -> Option<&'static Self> {
        Self::all().iter().copied().find(|entry| entry.id == id)
    }

    /// Apply native admission's release rule to a version.
    ///
    /// This does not contact a daemon. Use [`crate::Server::daemon_capabilities`]
    /// to query an existing daemon and enforce the supported-daemon minimum.
    #[must_use]
    pub fn support(&self, version: &TmuxVersion) -> CapabilitySupport {
        if version.has_behavior(&self.since) {
            CapabilitySupport::Available
        } else {
            match self.handling {
                CapabilityHandling::Refuse => CapabilitySupport::Unavailable,
                CapabilityHandling::Fallback => CapabilitySupport::Fallback,
            }
        }
    }
}

/// Immutable metadata for a defect in a bounded release interval.
///
/// # Examples
///
/// ```
/// use libtmux::{TmuxVersion, WorkaroundDefinition};
/// let workaround = WorkaroundDefinition::find("break-pane-misnames-windows")
///     .ok_or("break-pane-misnames-windows is not in the registry")?;
/// assert!(workaround.is_active(&TmuxVersion::parse_output(b"tmux 3.7\n")?));
/// assert!(!workaround.is_active(&TmuxVersion::parse_output(b"tmux 3.7a\n")?));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkaroundDefinition {
    /// Stable kebab-case workaround identity.
    pub id: &'static str,
    /// Human-readable name.
    pub name: &'static str,
    /// Defect and libtmux's handling of it.
    pub summary: &'static str,
    /// First affected release, inclusive.
    pub broken_in: ReleaseVersion,
    /// First fixed release, exclusive.
    pub fixed_in: ReleaseVersion,
    /// Public API names in the generated reference.
    pub apis: &'static [&'static str],
    /// Native command names and affected flags.
    pub commands: &'static [(&'static str, &'static [&'static str])],
}

impl WorkaroundDefinition {
    /// Return every workaround in registry order, without contacting tmux.
    #[must_use]
    pub const fn all() -> &'static [&'static Self] {
        crate::internal::support::capabilities::WORKAROUNDS
    }

    /// Find a known workaround; an unknown identity returns `None`.
    #[must_use]
    pub fn find(id: &str) -> Option<&'static Self> {
        Self::all().iter().copied().find(|entry| entry.id == id)
    }

    /// Apply the same exact release interval as native admission.
    #[must_use]
    pub fn is_active(&self, version: &TmuxVersion) -> bool {
        crate::internal::feature_policy::in_interval(version, self.broken_in, self.fixed_in)
    }
}

/// A named feature's decision for the daemon observed by a report.
///
/// # Examples
///
/// ```no_run
/// # async fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
/// if let Some(report) = server.daemon_capabilities().await? {
///     for feature in report.features() {
///         println!("{}: {}", feature.definition.id, feature.support.as_str());
///     }
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityStatus {
    /// The canonical registry definition.
    pub definition: &'static CapabilityDefinition,
    /// The decision for the observed daemon version.
    pub support: CapabilitySupport,
}

/// A workaround's applicability to the daemon observed by a report.
///
/// # Examples
///
/// ```no_run
/// # async fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
/// if let Some(report) = server.daemon_capabilities().await? {
///     for workaround in report.workarounds().filter(|entry| entry.active) {
///         println!("{}", workaround.definition.summary);
///     }
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkaroundStatus {
    /// The canonical registry definition.
    pub definition: &'static WorkaroundDefinition,
    /// Whether the observed daemon falls in the affected interval.
    pub active: bool,
}

/// Immutable feature decisions for an observed, supported tmux daemon.
///
/// A report is advisory: the daemon may be replaced after discovery. Later
/// operations still enforce their own admission and version fence.
/// Feature decisions concern release rules; APIs retain their connection and
/// resource preconditions.
///
/// # Examples
///
/// ```no_run
/// # async fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
/// if let Some(report) = server.daemon_capabilities().await? {
///     println!("tmux {}", report.tmux_version().raw());
///     if let Some(feature) = report.feature("buffer-delete-named") {
///         println!("{}", feature.support.as_str());
///     }
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonCapabilities {
    tmux_version: TmuxVersion,
}

impl DaemonCapabilities {
    pub(crate) fn from_tmux_version(tmux_version: TmuxVersion) -> Self {
        Self { tmux_version }
    }

    /// Return the observed daemon version, without an executable fallback.
    #[must_use]
    pub const fn tmux_version(&self) -> &TmuxVersion {
        &self.tmux_version
    }

    /// Return each feature decision in canonical registry order.
    pub fn features(&self) -> impl ExactSizeIterator<Item = CapabilityStatus> + '_ {
        CapabilityDefinition::all()
            .iter()
            .map(|definition| CapabilityStatus {
                definition,
                support: definition.support(&self.tmux_version),
            })
    }

    /// Find a feature decision; unknown identities return `None`.
    #[must_use]
    pub fn feature(&self, id: &str) -> Option<CapabilityStatus> {
        self.features().find(|entry| entry.definition.id == id)
    }

    /// Return each workaround's applicability in canonical registry order.
    pub fn workarounds(&self) -> impl ExactSizeIterator<Item = WorkaroundStatus> + '_ {
        WorkaroundDefinition::all()
            .iter()
            .map(|definition| WorkaroundStatus {
                definition,
                active: definition.is_active(&self.tmux_version),
            })
    }

    /// Find a workaround decision; unknown identities return `None`.
    #[must_use]
    pub fn workaround(&self, id: &str) -> Option<WorkaroundStatus> {
        self.workarounds().find(|entry| entry.definition.id == id)
    }
}

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
    /// let server = libtmux::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/tmux-version-example-absent.sock")
    ///     .build()?;
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
