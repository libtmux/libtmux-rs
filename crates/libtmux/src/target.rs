//! Typed tmux targets and endpoint discovery for the live client.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub use libtmux_core::{
    PaneId, PaneTarget, ServerGeneration, ServerIdentity, SessionId, SessionName, SessionNameError,
    SessionTarget, TmuxArg, WindowId, WindowTarget, escape_format,
};

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
            /// `TMUX` named this path.
            inherited: bool,
            /// tmux reads that `TMUX` to the same path, so it is not passed
            /// as `-S`.
            left_to_tmux: bool,
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

        Ok(ServerIdentity::from_socket_path(socket_path))
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

    /// Whether tmux, which stops reading `TMUX` at the first comma, would
    /// name `path` itself. A path that holds a comma is one only this crate
    /// can address, so it is passed as `-S` rather than left to tmux.
    fn tmux_reads_the_same_path(value: &OsStr, path: &OsStr) -> bool {
        value.as_bytes().split(|byte| *byte == b',').next() == Some(path.as_bytes())
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
            if !captured.as_os_str().is_empty() {
                if let Ok(resolved) = captured.canonicalize() {
                    return Ok(resolved);
                }
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
                    path: identity.socket_path().to_path_buf(),
                    inherited: false,
                    left_to_tmux: false,
                },
                identity,
            });
        }

        let (name, configured) = if let Some(name) = socket_name {
            if !valid_socket_name(name) {
                return Err(IdentityError::InvalidSocketName);
            }
            (name, true)
        } else if let Some((left_to_tmux, identity)) = inputs.inherited_tmux.and_then(|value| {
            let path = inherited_socket_path(value)?;
            let identity = capture_endpoint(path, inputs.cwd).ok()?;
            Some((tmux_reads_the_same_path(value, path), identity))
        }) {
            return Ok(ResolvedEndpoint {
                selector: ResolvedSocketSelector::Path {
                    path: identity.socket_path().to_path_buf(),
                    inherited: true,
                    left_to_tmux,
                },
                identity,
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
            identity: ServerIdentity::from_socket_path(socket_path),
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
