//! Typed tmux object IDs, targets, and server identity.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::hash::{Hash, Hasher};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// An invalid scope-specific tmux object ID.
///
/// The error records the expected sigil but never retains the rejected input.
///
/// # Examples
///
/// ```
/// use libtmux_core::SessionId;
///
/// // The sigil is the whole difference between the id types, so a mistake
/// // names the one that was expected.
/// let error = "@1".parse::<SessionId>().expect_err("@ denotes a window");
/// assert_eq!(error.expected_sigil(), '$');
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct IdParseError {
    expected_sigil: char,
}

impl IdParseError {
    /// Construct an ID decoder error for the expected scope.
    ///
    /// This is shared implementation support for the driver crates.
    #[doc(hidden)]
    pub const fn new(expected_sigil: char) -> Self {
        Self { expected_sigil }
    }

    /// Return the sigil required by the requested ID scope.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::SessionId;
    ///
    /// let error = "@1".parse::<SessionId>().expect_err("@ denotes a window");
    /// assert_eq!(error.expected_sigil(), '$');
    /// ```
    #[must_use]
    pub const fn expected_sigil(self) -> char {
        self.expected_sigil
    }
}

impl fmt::Display for IdParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid tmux ID: expected {} followed by an integer from 0 through {}",
            self.expected_sigil,
            u32::MAX,
        )
    }
}

impl std::error::Error for IdParseError {}

/// Escape text so tmux uses it as written rather than expanding it.
///
/// tmux runs its format machinery over more than names: `#(command)` runs
/// `command` in a shell and `#{session_id}` becomes the id, in a session or
/// window name and in the start directory `-c` names alike. Doubling `#` is
/// what tmux's parser reads as one literal `#`, so the text arrives as
/// itself -- which also makes a directory whose name really contains `#`
/// reachable, where passing it through unescaped does not.
///
/// Every typed argument that tmux expands takes [`TmuxArg`], which applies
/// this on conversion, so a caller reaches for this directly only when
/// building raw tmux command arguments.
///
/// # Examples
///
/// ```
/// use std::ffi::OsString;
///
/// use libtmux_core::escape_format;
///
/// // Text that would otherwise run a command.
/// assert_eq!(escape_format("#(id)"), OsString::from("##(id)"));
///
/// // Ordinary text is unchanged.
/// assert_eq!(escape_format("editor"), OsString::from("editor"));
/// ```
#[must_use]
pub fn escape_format(text: impl AsRef<OsStr>) -> OsString {
    #[cfg(unix)]
    {
        let bytes = text.as_ref().as_bytes();
        let mut escaped = Vec::with_capacity(bytes.len());
        for byte in bytes {
            if *byte == b'#' {
                escaped.push(b'#');
            }
            escaped.push(*byte);
        }
        OsString::from_vec(escaped)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};

        let mut escaped = Vec::new();
        for unit in text.as_ref().encode_wide() {
            if unit == u16::from(b'#') {
                escaped.push(unit);
            }
            escaped.push(unit);
        }
        OsString::from_wide(&escaped)
    }
    #[cfg(not(any(unix, windows)))]
    {
        OsString::from(text.as_ref().to_string_lossy().replace('#', "##"))
    }
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
            #[doc(hidden)]
            #[must_use]
            pub const fn number(&self) -> u32 {
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
    /// use libtmux_core::SessionId;
    ///
    /// let id: SessionId = "$001".parse()?;
    /// assert_eq!(id.as_ref(), "$1");
    /// # Ok::<(), libtmux_core::IdParseError>(())
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
    /// use libtmux_core::WindowId;
    ///
    /// let id: WindowId = "@10".parse()?;
    /// assert_eq!(id.to_string(), "@10");
    /// # Ok::<(), libtmux_core::IdParseError>(())
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
    /// use libtmux_core::PaneId;
    ///
    /// let second: PaneId = "%2".parse()?;
    /// let tenth: PaneId = "%10".parse()?;
    /// assert!(second < tenth);
    /// # Ok::<(), libtmux_core::IdParseError>(())
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
    /// use libtmux_core::{SessionId, SessionTarget};
    ///
    /// let id: SessionId = "$1".parse()?;
    /// let target = SessionTarget::from(id.clone());
    /// assert_eq!(target, SessionTarget::Id(id));
    /// # Ok::<(), libtmux_core::IdParseError>(())
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
    /// use libtmux_core::{WindowId, WindowTarget};
    ///
    /// fn accepts_window(_: WindowTarget) {}
    /// let id: WindowId = "@1".parse()?;
    /// accepts_window(WindowTarget::from(id));
    /// # Ok::<(), libtmux_core::IdParseError>(())
    /// ```
    ///
    /// A pane target cannot cross that scope boundary:
    ///
    /// ```compile_fail
    /// use libtmux_core::{PaneId, PaneTarget, WindowTarget};
    ///
    /// fn accepts_window(_: WindowTarget) {}
    /// let id: PaneId = "%1".parse()?;
    /// accepts_window(PaneTarget::from(id));
    /// # Ok::<(), libtmux_core::IdParseError>(())
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
    /// use libtmux_core::{PaneId, PaneTarget};
    ///
    /// let id: PaneId = "%1".parse()?;
    /// let target = PaneTarget::from(id.clone());
    /// assert_eq!(target, PaneTarget::Id(id));
    /// # Ok::<(), libtmux_core::IdParseError>(())
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
/// use libtmux_core::ServerGeneration;
///
/// # let generation = ServerGeneration::from_raw(42, 0);
/// assert_eq!(generation.pid(), 42);
/// assert_eq!(generation.start_time(), std::time::UNIX_EPOCH);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ServerGeneration {
    pid: u32,
    start_time: i64,
}

impl ServerGeneration {
    /// Restore a generation decoded from a tmux identity reply.
    ///
    /// Drivers must validate that the timestamp is representable before
    /// constructing an observed generation. This is shared implementation
    /// support for the driver crates.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_raw(pid: u32, start_time: i64) -> Self {
        Self { pid, start_time }
    }

    /// Return the whole-second Unix timestamp used by a tmux generation guard.
    ///
    /// This is shared implementation support for the driver crates.
    #[doc(hidden)]
    #[must_use]
    pub const fn unix_start_time(self) -> i64 {
        self.start_time
    }

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
        let offset = Duration::from_secs(self.start_time.unsigned_abs());
        let time = if self.start_time < 0 {
            UNIX_EPOCH.checked_sub(offset)
        } else {
            UNIX_EPOCH.checked_add(offset)
        };
        time.unwrap_or(UNIX_EPOCH)
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
/// use libtmux_core::ServerIdentity;
///
/// # let identity = ServerIdentity::from_socket_path("/private/socket".into());
/// assert_eq!(identity, identity.clone());
/// assert!(!format!("{identity:?}").contains("private"));
/// ```
#[derive(Clone)]
pub struct ServerIdentity {
    socket_path: PathBuf,
}

impl PartialEq for ServerIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.socket_path.as_os_str().as_encoded_bytes()
            == other.socket_path.as_os_str().as_encoded_bytes()
    }
}

impl Eq for ServerIdentity {}

impl Hash for ServerIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.socket_path.as_os_str().as_encoded_bytes().hash(state);
    }
}

impl ServerIdentity {
    /// Capture a socket path already resolved by a driver.
    ///
    /// The driver must provide its captured absolute path. This constructor
    /// preserves its spelling without filesystem access or normalisation.
    #[doc(hidden)]
    #[must_use]
    pub fn from_socket_path(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// Borrow the captured socket path for driver dispatch.
    ///
    /// This is shared implementation support for the driver crates.
    #[doc(hidden)]
    #[must_use]
    pub fn socket_path(&self) -> &Path {
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
/// use libtmux_core::SessionName;
///
/// assert!(SessionName::new("build").is_ok());
/// assert!(SessionName::new("a:b").is_err());
/// assert!(SessionName::new("c.d").is_err());
/// assert!(SessionName::new("").is_err());
///
/// // Everything else tmux accepts is kept, including what looks awkward.
/// assert!(SessionName::new("has,comma").is_ok());
/// assert!(SessionName::new("has space").is_ok());
/// # Ok::<(), libtmux_core::SessionNameError>(())
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
/// use libtmux_core::{SessionName, SessionNameError};
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SessionNameError {
    /// The name is empty, which tmux reads as "the current session".
    Empty,

    /// The name holds a character tmux splits targets on.
    Separator {
        /// The separator found.
        separator: char,
    },
}

impl fmt::Display for SessionNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str(
                "a session name cannot be empty: tmux reads that as the current session",
            ),
            Self::Separator { separator } => write!(
                formatter,
                "a session name cannot hold {separator:?}: tmux splits a target there, so the session \
                 would not be addressable by name",
            ),
        }
    }
}

impl std::error::Error for SessionNameError {}

/// Text bound for a tmux argument that tmux expands as a format.
///
/// tmux runs its format machinery over a session or window name, a pane
/// title, and a `-c` start directory before it uses them, so `#(command)` in
/// caller text runs a shell. Every such argument in this crate takes this
/// type, and every conversion into it escapes: text a program did not write
/// arrives as itself, and asking for expansion is a visible call to
/// [`TmuxArg::format`].
///
/// # Examples
///
/// ```
/// use libtmux_core::TmuxArg;
///
/// // The ordinary path. `From` escapes, so tmux stores what was asked.
/// let literal = TmuxArg::from("release#1");
/// assert_eq!(literal.as_os_str(), "release##1");
///
/// // Expansion is opt-in and reads as such.
/// let expanded = TmuxArg::format("#{pane_current_path}");
/// assert_eq!(expanded.as_os_str(), "#{pane_current_path}");
/// ```
#[derive(Clone)]
pub struct TmuxArg {
    encoded: OsString,
    literal: Option<OsString>,
}

impl TmuxArg {
    /// Send `text` literally, escaping what tmux would otherwise expand.
    #[must_use]
    pub fn literal(text: impl AsRef<OsStr>) -> Self {
        let literal = text.as_ref().to_os_string();
        Self {
            encoded: escape_format(&literal),
            literal: Some(literal),
        }
    }

    /// Send `template` for tmux to expand as a format.
    ///
    /// Only for a template the program itself wrote. A template built from an
    /// argument, a request field, or a configuration file hands whoever wrote
    /// it a shell.
    #[must_use]
    pub fn format(template: impl Into<OsString>) -> Self {
        Self {
            encoded: template.into(),
            literal: None,
        }
    }

    /// Borrow the bytes as they will reach tmux, escaped or not.
    #[must_use]
    pub fn as_os_str(&self) -> &OsStr {
        &self.encoded
    }

    /// Borrow the original literal before tmux format escaping.
    ///
    /// Returns `None` for an argument built with [`Self::format`]. Literal
    /// provenance is retained for portable operation serialisation; equality,
    /// ordering and hashing still compare the encoded argument.
    #[must_use]
    pub fn literal_value(&self) -> Option<&OsStr> {
        self.literal.as_deref()
    }

    /// Consume the argument without escaping its prepared bytes again.
    ///
    /// This is shared implementation support for the driver crates.
    #[doc(hidden)]
    #[must_use]
    pub fn into_os_string(self) -> OsString {
        self.encoded
    }
}

impl PartialEq for TmuxArg {
    fn eq(&self, other: &Self) -> bool {
        self.encoded == other.encoded
    }
}

impl Eq for TmuxArg {}

impl PartialOrd for TmuxArg {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TmuxArg {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.encoded.cmp(&other.encoded)
    }
}

impl Hash for TmuxArg {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.encoded.hash(state);
    }
}

impl fmt::Debug for TmuxArg {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("TmuxArg")
            .field(&self.encoded)
            .finish()
    }
}

macro_rules! tmux_arg_from {
    ($($type:ty),* $(,)?) => {
        $(
            impl From<$type> for TmuxArg {
                fn from(text: $type) -> Self {
                    Self::literal(text)
                }
            }
        )*
    };
}

// One impl per source type rather than a blanket `impl<T: AsRef<OsStr>>`,
// which would collide with the reflexive `From<TmuxArg>` in core and take
// `TmuxArg::format`'s result back through the escaper.
tmux_arg_from!(
    &str, String, &String, OsString, &OsString, &OsStr, PathBuf, &PathBuf, &Path
);

#[cfg(unix)]
impl From<&crate::TmuxText> for TmuxArg {
    fn from(text: &crate::TmuxText) -> Self {
        Self::literal(OsStr::from_bytes(text.as_bytes()))
    }
}

#[cfg(test)]
mod tests;
