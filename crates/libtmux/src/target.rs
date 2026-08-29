//! Typed tmux object IDs, targets, and server identity.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::error::IdParseError;

/// Escape a name so tmux stores the text it was given.
///
/// tmux expands a name through its format machinery before it stores it, so
/// `#(command)` runs `command` in a shell and `#{session_id}` becomes the id.
/// Doubling `#` is what tmux's parser reads as one literal `#`, so the name
/// arrives as itself.
///
/// This is not applied for you, because a name is sometimes meant as a
/// format. Use it for the names a program did not write: an argument, a
/// request field, a configuration file. Passing that text through unescaped
/// gives whoever wrote it a shell.
///
/// # Examples
///
/// ```
/// use std::ffi::OsString;
///
/// use libtmux::escape_name;
///
/// // A name that would otherwise run a command.
/// assert_eq!(escape_name("#(id)"), OsString::from("##(id)"));
///
/// // Ordinary names are unchanged.
/// assert_eq!(escape_name("editor"), OsString::from("editor"));
/// ```
#[must_use]
pub fn escape_name(name: impl AsRef<OsStr>) -> OsString {
    let bytes = name.as_ref().as_bytes();
    let mut escaped = Vec::with_capacity(bytes.len());
    for byte in bytes {
        if *byte == b'#' {
            escaped.push(b'#');
        }
        escaped.push(*byte);
    }
    OsString::from_vec(escaped)
}

fn parse_id(value: &str, sigil: char) -> Result<(u32, Box<str>), IdParseError> {
    let error = || IdParseError::new(sigil);
    let digits = value.strip_prefix(sigil).ok_or_else(error)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(error());
    }

    let number = digits.parse::<u32>().map_err(|_| error())?;
    Ok((number, format!("{sigil}{number}").into_boxed_str()))
}

macro_rules! define_id {
    ($(#[$attribute:meta])* $name:ident, $sigil:literal) => {
        $(#[$attribute])*
        #[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name {
            number: u32,
            rendered: Box<str>,
        }

        impl $name {
            /// Return the numeric part, which is unique among IDs of one kind.
            ///
            /// Useful as a map key: it is `Copy` where the ID owns a rendered
            /// string, so grouping by it allocates nothing.
            pub(crate) const fn number(&self) -> u32 {
                self.number
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.rendered
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.debug_tuple(stringify!($name)).field(&self.rendered).finish()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.rendered)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let (number, rendered) = parse_id(value, $sigil)?;
                Ok(Self { number, rendered })
            }
        }
    };
}

define_id!(
    /// A native tmux session ID such as `$1`.
    ///
    /// Leading zeroes are accepted but canonicalized to the numeric tmux ID.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::SessionId;
    ///
    /// let id: SessionId = "$001".parse()?;
    /// assert_eq!(id.as_ref(), "$1");
    /// # Ok::<(), libtmux::IdParseError>(())
    /// ```
    SessionId,
    '$'
);

define_id!(
    /// A native tmux window ID such as `@1`.
    ///
    /// Leading zeroes are accepted but canonicalized to the numeric tmux ID.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::WindowId;
    ///
    /// let id: WindowId = "@10".parse()?;
    /// assert_eq!(id.to_string(), "@10");
    /// # Ok::<(), libtmux::IdParseError>(())
    /// ```
    WindowId,
    '@'
);

define_id!(
    /// A native tmux pane ID such as `%1`.
    ///
    /// Leading zeroes are accepted but canonicalized to the numeric tmux ID.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::PaneId;
    ///
    /// let second: PaneId = "%2".parse()?;
    /// let tenth: PaneId = "%10".parse()?;
    /// assert!(second < tenth);
    /// # Ok::<(), libtmux::IdParseError>(())
    /// ```
    PaneId,
    '%'
);

macro_rules! define_target {
    ($(#[$attribute:meta])* $name:ident, $id:ident) => {
        $(#[$attribute])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[non_exhaustive]
        pub enum $name {
            /// Target the native tmux object ID.
            Id($id),
        }

        impl From<$id> for $name {
            fn from(id: $id) -> Self {
                Self::Id(id)
            }
        }
    };
}

define_target!(
    /// A target accepted by session-scoped operations.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::{SessionId, SessionTarget};
    ///
    /// let id: SessionId = "$1".parse()?;
    /// let target = SessionTarget::from(id.clone());
    /// assert_eq!(target, SessionTarget::Id(id));
    /// # Ok::<(), libtmux::IdParseError>(())
    /// ```
    SessionTarget,
    SessionId
);

define_target!(
    /// A target accepted by window-scoped operations.
    ///
    /// A window target can be passed to a window-scoped function:
    ///
    /// ```
    /// use libtmux::{WindowId, WindowTarget};
    ///
    /// fn accepts_window(_: WindowTarget) {}
    /// let id: WindowId = "@1".parse()?;
    /// accepts_window(WindowTarget::from(id));
    /// # Ok::<(), libtmux::IdParseError>(())
    /// ```
    ///
    /// A pane target cannot cross that scope boundary:
    ///
    /// ```compile_fail
    /// use libtmux::{PaneId, PaneTarget, WindowTarget};
    ///
    /// fn accepts_window(_: WindowTarget) {}
    /// let id: PaneId = "%1".parse()?;
    /// accepts_window(PaneTarget::from(id));
    /// # Ok::<(), libtmux::IdParseError>(())
    /// ```
    WindowTarget,
    WindowId
);

define_target!(
    /// A target accepted by pane-scoped operations.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::{PaneId, PaneTarget};
    ///
    /// let id: PaneId = "%1".parse()?;
    /// let target = PaneTarget::from(id.clone());
    /// assert_eq!(target, PaneTarget::Id(id));
    /// # Ok::<(), libtmux::IdParseError>(())
    /// ```
    PaneTarget,
    PaneId
);

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

    /// When that server started, as tmux reports it.
    #[must_use]
    pub const fn start_time(self) -> i64 {
        self.start_time
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
    #[cfg(test)]
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

/// A session name tmux can address.
///
/// tmux will happily create a session whose name holds `:` or `.`, and then
/// cannot find it again: those are its target separators, so it splits the
/// name before looking anything up.
///
/// ```console
/// $ tmux new-session -d -s 'a:b'      # accepted
/// $ tmux has-session -t 'a:b'
/// can't find window: b
/// $ tmux kill-session -t 'a:b'
/// can't find window: b
/// ```
///
/// The session exists and cannot be killed by its own name. An empty name is
/// rejected for the same reason: an empty target means "the current one".
///
/// # Examples
///
/// ```
/// use libtmux::SessionName;
///
/// assert!(SessionName::new("build").is_ok());
/// assert!(SessionName::new("a:b").is_err());
/// assert!(SessionName::new("c.d").is_err());
/// assert!(SessionName::new("").is_err());
///
/// // Everything else tmux accepts is kept, including what looks awkward.
/// assert!(SessionName::new("has,comma").is_ok());
/// assert!(SessionName::new("has space").is_ok());
/// # Ok::<(), libtmux::SessionNameError>(())
/// ```
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionName(String);

impl SessionName {
    /// Check a name tmux will be able to address.
    ///
    /// # Errors
    ///
    /// Returns [`SessionNameError`] for an empty name, or one holding a tmux
    /// target separator.
    pub fn new(name: impl Into<String>) -> Result<Self, SessionNameError> {
        let name = name.into();
        if name.is_empty() {
            return Err(SessionNameError::Empty);
        }
        if let Some(separator) = name
            .chars()
            .find(|character| matches!(character, ':' | '.'))
        {
            return Err(SessionNameError::Separator { separator });
        }

        Ok(Self(name))
    }

    /// Borrow the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for SessionName {
    type Err = SessionNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl From<SessionName> for OsString {
    fn from(name: SessionName) -> Self {
        Self::from(name.0)
    }
}

/// Why a session name is one tmux could not address.
///
/// # Examples
///
/// ```
/// use libtmux::{SessionName, SessionNameError};
///
/// // tmux splits a target on `:` and `.`, so a name holding either would not be
/// // addressable by name afterwards.
/// assert!(matches!(
///     SessionName::new("build:release"),
///     Err(SessionNameError::Separator { separator: ':' }),
/// ));
/// assert!(matches!(SessionName::new(""), Err(SessionNameError::Empty)));
/// assert!(SessionName::new("build-release").is_ok());
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SessionNameError {
    /// The name is empty, which tmux reads as "the current session".
    #[error("a session name cannot be empty: tmux reads that as the current session")]
    Empty,

    /// The name holds a character tmux splits targets on.
    #[error(
        "a session name cannot hold {separator:?}: tmux splits a target there, so the session \
         would not be addressable by name"
    )]
    Separator {
        /// The separator found.
        separator: char,
    },
}
