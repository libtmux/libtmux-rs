//! Typed tmux object IDs, targets, and server identity.

use std::ffi::OsStr;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub use libtmux_core::{
    PaneId, PaneTarget, SessionId, SessionName, SessionNameError, SessionTarget, TmuxArg, WindowId,
    WindowTarget, escape_format,
};

/// Which tmux daemon is answering on an endpoint.
///
/// [`ServerIdentity`] answers "where", and that is not enough to answer
/// "which": tmux reuses the socket file across restarts, so a replacement
/// daemon is indistinguishable from the one it replaced by path alone. It also
/// reissues ids from the start, so the replacement's first pane is `%0` too.
///
/// Holding a handle across a possible restart is therefore a correctness
/// question rather than a liveness one. A stale read fails harmlessly; a stale
/// *mutation* lands on whatever now wears that id.
///
/// The pid alone would not do. A replacement daemon can be handed the pid of
/// the one it replaced, so the start time is what makes this a generation
/// rather than a guess.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// let guard = libtmux::test::TestServer::new().await?;
/// let server = guard.server();
///
/// // Capture the generation beside whatever ids are being held.
/// let generation = server.generation().await?;
/// let session = server.new_session("work").await?;
///
/// // Before acting on those ids later, confirm the daemon has not been
/// // replaced underneath them.
/// assert_eq!(server.generation().await?, generation);
/// let _ = session;
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ServerGeneration {
    pub(crate) pid: u32,
    pub(crate) start_time: i64,
}

impl ServerGeneration {
    /// The tmux server process id.
    #[must_use]
    pub const fn pid(self) -> u32 {
        self.pid
    }

    /// When that server started, to the whole second tmux keeps.
    ///
    /// [`Display`](fmt::Display) prints the same moment as the Unix seconds
    /// tmux reports.
    #[must_use]
    pub fn start_time(self) -> SystemTime {
        crate::snapshot::stored_time(self.start_time)
    }
}

impl fmt::Display for ServerGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "pid {} started {}", self.pid, self.start_time)
    }
}

/// The structural identity of one tmux server endpoint.
///
/// Equality and hashing use the captured absolute socket path. Debug output
/// redacts that path so diagnostics do not disclose local filesystem details.
///
/// This is what makes two handles to the same server compare equal, and it is
/// deliberately *not* enough to say the daemon behind them is the same one --
/// see [`ServerGeneration`] for that.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// let guard = libtmux::test::TestServer::new().await?;
/// let server = guard.server();
///
/// // Two handles to one endpoint share an identity, so they can key a map.
/// let second = server.clone();
/// assert_eq!(server.identity(), second.identity());
///
/// // The socket path never reaches diagnostics.
/// let rendered = format!("{:?}", server.identity());
/// assert!(rendered.contains("<redacted>"), "{rendered}");
/// assert!(!rendered.contains("/tmp/"), "{rendered}");
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct ServerIdentity {
    socket_path: PathBuf,
}

impl PartialEq for ServerIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.socket_path.as_os_str().as_bytes() == other.socket_path.as_os_str().as_bytes()
    }
}

impl Eq for ServerIdentity {}

impl Hash for ServerIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.socket_path.as_os_str().as_bytes().hash(state);
    }
}

impl ServerIdentity {
    #[cfg(any(test, feature = "unstable-fuzzing"))]
    pub(crate) fn from_socket_path(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl fmt::Debug for ServerIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerIdentity")
            .field("socket_path", &"<redacted>")
            .finish()
    }
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct WindowLinkIdentity {
    server_identity: ServerIdentity,
    session_id: SessionId,
    window_index: i32,
    window_id: WindowId,
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
impl WindowLinkIdentity {
    pub(crate) fn new(
        server_identity: ServerIdentity,
        session_id: SessionId,
        window_index: i32,
        window_id: WindowId,
    ) -> Self {
        Self {
            server_identity,
            session_id,
            window_index,
            window_id,
        }
    }

    pub(crate) const fn server_identity(&self) -> &ServerIdentity {
        &self.server_identity
    }

    pub(crate) const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub(crate) const fn window_index(&self) -> i32 {
        self.window_index
    }

    pub(crate) const fn window_id(&self) -> &WindowId {
        &self.window_id
    }
}

pub(crate) mod endpoint_resolution {
    use std::ffi::OsString;

    use super::{OsStr, OsStrExt, Path, PathBuf, ServerIdentity};

    #[derive(Clone, Copy)]
    enum FallbackSocketRoot<'a> {
        SystemTmp,
        Captured(Option<&'a Path>),
    }

    #[derive(Clone, Copy)]
    pub(crate) struct EndpointInputs<'a> {
        cwd: &'a Path,
        socket_root: Option<&'a OsStr>,
        real_uid: u32,
        inherited_tmux: Option<&'a OsStr>,
        fallback_socket_root: FallbackSocketRoot<'a>,
    }

    impl<'a> EndpointInputs<'a> {
        pub(crate) const fn new(
            cwd: &'a Path,
            socket_root: Option<&'a OsStr>,
            real_uid: u32,
            inherited_tmux: Option<&'a OsStr>,
        ) -> Self {
            Self {
                cwd,
                socket_root,
                real_uid,
                inherited_tmux,
                fallback_socket_root: FallbackSocketRoot::SystemTmp,
            }
        }

        pub(crate) const fn with_captured_fallback_socket_root(
            mut self,
            fallback_socket_root: Option<&'a Path>,
        ) -> Self {
            self.fallback_socket_root = FallbackSocketRoot::Captured(fallback_socket_root);
            self
        }
    }

    pub(crate) enum ResolvedSocketSelector {
        Path {
            path: PathBuf,
            inherited: bool,
        },
        Name {
            name: OsString,
            socket_root: PathBuf,
            configured: bool,
        },
    }

    pub(crate) struct ResolvedEndpoint {
        pub(crate) identity: ServerIdentity,
        pub(crate) selector: ResolvedSocketSelector,
    }

    #[derive(Debug, thiserror::Error)]
    pub(crate) enum IdentityError {
        #[error("tmux socket path is empty")]
        EmptySocketPath,
        #[error("tmux socket path contains a NUL byte")]
        SocketPathContainsNul,
        #[error("working directory is not absolute")]
        RelativeWorkingDirectory,
        #[error("socket path and socket name are mutually exclusive")]
        ConflictingSelectors,
        #[error("tmux socket name is not one normal path component")]
        InvalidSocketName,
        #[error("no tmux socket root could be resolved")]
        NoSocketRoot,
    }

    fn capture_endpoint(path: &OsStr, cwd: &Path) -> Result<ServerIdentity, IdentityError> {
        let bytes = path.as_bytes();
        if bytes.is_empty() {
            return Err(IdentityError::EmptySocketPath);
        }
        if bytes.contains(&b'\0') {
            return Err(IdentityError::SocketPathContainsNul);
        }

        let path = Path::new(path);
        let socket_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            if !cwd.is_absolute() {
                return Err(IdentityError::RelativeWorkingDirectory);
            }
            cwd.join(path)
        };

        Ok(ServerIdentity { socket_path })
    }

    fn inherited_socket_path(value: &OsStr) -> Option<&OsStr> {
        let mut fields = value.as_bytes().rsplitn(3, |byte| *byte == b',');
        fields.next()?;
        fields.next()?;
        let path = fields.next()?;
        if path.is_empty() {
            return None;
        }
        Some(OsStr::from_bytes(path))
    }

    fn resolved_socket_root(
        candidate: Option<&OsStr>,
        cwd: &Path,
        fallback: FallbackSocketRoot<'_>,
    ) -> Result<PathBuf, IdentityError> {
        if let Some(candidate) = candidate.filter(|value| !value.as_bytes().is_empty()) {
            let candidate = Path::new(candidate);
            let captured = if candidate.is_absolute() {
                candidate.to_path_buf()
            } else if cwd.is_absolute() {
                cwd.join(candidate)
            } else {
                PathBuf::new()
            };
            if !captured.as_os_str().is_empty()
                && let Ok(resolved) = captured.canonicalize()
            {
                return Ok(resolved);
            }
        }

        match fallback {
            FallbackSocketRoot::SystemTmp => Path::new("/tmp")
                .canonicalize()
                .map_err(|_| IdentityError::NoSocketRoot),
            FallbackSocketRoot::Captured(Some(path)) => Ok(path.to_path_buf()),
            FallbackSocketRoot::Captured(None) => Err(IdentityError::NoSocketRoot),
        }
    }

    fn valid_socket_name(name: &OsStr) -> bool {
        let bytes = name.as_bytes();
        !bytes.is_empty()
            && !bytes.contains(&b'\0')
            && !bytes.contains(&b'/')
            && bytes != b"."
            && bytes != b".."
    }

    pub(crate) fn resolve_server_endpoint(
        explicit_path: Option<&OsStr>,
        socket_name: Option<&OsStr>,
        inputs: EndpointInputs<'_>,
    ) -> Result<ResolvedEndpoint, IdentityError> {
        if explicit_path.is_some() && socket_name.is_some() {
            return Err(IdentityError::ConflictingSelectors);
        }
        if let Some(path) = explicit_path {
            let identity = capture_endpoint(path, inputs.cwd)?;
            return Ok(ResolvedEndpoint {
                selector: ResolvedSocketSelector::Path {
                    path: identity.socket_path.clone(),
                    inherited: false,
                },
                identity,
            });
        }

        let (name, configured) = if let Some(name) = socket_name {
            if !valid_socket_name(name) {
                return Err(IdentityError::InvalidSocketName);
            }
            (name, true)
        } else if let Some(path) = inputs
            .inherited_tmux
            .and_then(inherited_socket_path)
            .and_then(|path| capture_endpoint(path, inputs.cwd).ok())
        {
            return Ok(ResolvedEndpoint {
                selector: ResolvedSocketSelector::Path {
                    path: path.socket_path.clone(),
                    inherited: true,
                },
                identity: path,
            });
        } else {
            (OsStr::new("default"), false)
        };

        let socket_root =
            resolved_socket_root(inputs.socket_root, inputs.cwd, inputs.fallback_socket_root)?;
        let mut socket_path = socket_root.clone();
        socket_path.push(format!("tmux-{}", inputs.real_uid));
        socket_path.push(name);
        Ok(ResolvedEndpoint {
            identity: ServerIdentity { socket_path },
            selector: ResolvedSocketSelector::Name {
                name: name.to_os_string(),
                socket_root,
                configured,
            },
        })
    }

    #[cfg(test)]
    pub(crate) fn resolve_server_identity(
        explicit_path: Option<&OsStr>,
        socket_name: Option<&OsStr>,
        inputs: EndpointInputs<'_>,
    ) -> Result<ServerIdentity, IdentityError> {
        resolve_server_endpoint(explicit_path, socket_name, inputs)
            .map(|endpoint| endpoint.identity)
    }
}

#[cfg(test)]
mod tests;
