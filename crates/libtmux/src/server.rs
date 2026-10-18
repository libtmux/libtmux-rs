use std::ffi::{OsStr, OsString};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "control-mode")]
use crate::SessionId;
use crate::formats::{TmuxText, TransportDialect, split_quoted_rows};
use crate::internal::core::Core;
use crate::internal::listing;
#[cfg(feature = "control-mode")]
use crate::internal::process::PersistentChild;
use crate::internal::scoped;
use crate::pane::Pane;
use crate::session::Session;
use crate::{
    Command, CommandChain, CommandResult, EngineCapabilities, Error, ListingDecodeError,
    ReleaseSuffix, ReleaseVersion, ServerConfigurationErrorKind, ServerGeneration, ServerIdentity,
    TmuxArg,
};

mod builder;
mod channels;
mod discovery;
mod interactive;
pub use interactive::MenuItem;
mod keys;
#[cfg(feature = "unstable-fuzzing")]
#[doc(hidden)]
pub use keys::__fuzz_parse_key_bindings;
pub use keys::KeyBinding;
mod settings;
pub use builder::ServerBuilder;
pub use discovery::{SessionTree, WindowTree};
#[cfg(feature = "query")]
pub use discovery::{SessionTreeFields, WindowTreeFields};

/// The first tmux release that has a server access list.
use crate::version::since::SERVER_ACCESS as SERVER_ACCESS_SINCE;

/// How much a user on the server's access list may do.
///
/// An enum rather than two flags because tmux's `-r` and `-w` are exclusive:
/// passing both is a contradiction the type makes unrepresentable.
///
/// # Examples
///
/// ```no_run
/// #  fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
/// use libtmux::AccessMode;
///
/// // Server access control is tmux 3.3 and later. `ReadOnly` lets another user
/// // watch without being able to send keys.
/// server.grant_access("observer", AccessMode::ReadOnly)?;
/// server.grant_access("pair", AccessMode::Write)?;
///
/// let rules = server.access_rules()?;
/// assert_eq!(rules.len(), 2);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessMode {
    /// May attach and watch, but not act.
    ReadOnly,
    /// May attach and act.
    Write,
}

/// Whether an [`AccessRule`] names an operating-system user or group.
///
/// tmux originally listed only users. A later release let the server owner
/// add a whole group to the access list, and marks each listed entry `U` or
/// `G` so a caller can tell them apart. A release before that mark existed
/// prints a bare `name (R)`/`name (W)` line with no marker at all -- every row
/// such a release can print names a user, so decoding one reports
/// [`Principal::User`].
///
/// # Examples
///
/// ```no_run
/// #  fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
/// use libtmux::Principal;
///
/// for rule in server.access_rules()? {
///     match rule.principal() {
///         Principal::User => println!("{} (user)", rule.name()),
///         Principal::Group => println!("{} (group)", rule.name()),
///     }
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Principal {
    /// The entry names an operating-system user.
    User,
    /// The entry names an operating-system group.
    Group,
}

/// One entry of the server's access list.
///
/// # Examples
///
/// ```no_run
/// #  fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
/// use libtmux::AccessMode;
///
/// for rule in server.access_rules()? {
///     if rule.mode() == AccessMode::Write {
///         println!("{} can type", rule.name());
///     }
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessRule {
    name: String,
    principal: Principal,
    mode: AccessMode,
}

impl AccessRule {
    /// The user or group this entry names.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether [`Self::name`] is an operating-system user or group.
    #[must_use]
    pub const fn principal(&self) -> Principal {
        self.principal
    }

    /// What that principal may do.
    #[must_use]
    pub const fn mode(&self) -> AccessMode {
        self.mode
    }
}

/// Decode one `server-access -l` line into an [`AccessRule`].
///
/// tmux prints either the legacy `name (R)`/`name (W)` (no principal marker,
/// every row a user) or the compound `name (U,R)`/`name (G,W)` grammar a
/// release with group ACLs uses for every row, owner included. Deciding which
/// grammar applies from the line itself, rather than from the detected tmux
/// version, is what lets this read a listing from either release without a
/// version predicate.
///
/// # Errors
///
/// Returns [`Error::UnreadableAccessRule`] when the trailing marker matches
/// neither grammar, naming the marker rather than silently dropping the row.
fn parse_access_rule(line: &str) -> Result<AccessRule, Error> {
    let (name, marker) = line
        .rsplit_once(' ')
        .ok_or_else(|| Error::unreadable_access_rule(line))?;
    let inner = marker
        .strip_prefix('(')
        .and_then(|marker| marker.strip_suffix(')'))
        .ok_or_else(|| Error::unreadable_access_rule(marker))?;
    let (principal, mode) = match inner.split_once(',') {
        Some(("U", mode)) => (Principal::User, mode),
        Some(("G", mode)) => (Principal::Group, mode),
        Some(_) => return Err(Error::unreadable_access_rule(marker)),
        // No comma: the legacy grammar, which lists only users.
        None => (Principal::User, inner),
    };
    let mode = match mode {
        "R" => AccessMode::ReadOnly,
        "W" => AccessMode::Write,
        _ => return Err(Error::unreadable_access_rule(marker)),
    };
    Ok(AccessRule {
        name: name.to_owned(),
        principal,
        mode,
    })
}

/// The first tmux release that remembers prompt history.
use crate::version::since::PROMPT_HISTORY as PROMPT_HISTORY_SINCE;

/// Which prompt tmux is remembering answers for.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use libtmux::PromptKind;
///
/// let guard = libtmux::test::TestServer::new()?;
/// let version = guard.server().capabilities()?.tmux_version().clone();
///
/// // tmux keeps a separate history per prompt kind, so a command typed at the
/// // `:` prompt is not offered when searching.
/// if version.has_behavior(&libtmux::since::PROMPT_HISTORY) {
///     assert!(guard.server().prompt_history(PromptKind::Command)?.is_empty());
///     assert!(guard.server().prompt_history(PromptKind::Search)?.is_empty());
/// }
///
/// guard.shutdown()?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptKind {
    /// The `:` command prompt.
    Command,
    /// The search prompt in copy mode.
    Search,
    /// A prompt asking for a target.
    Target,
    /// A prompt asking for a window target.
    WindowTarget,
}

impl PromptKind {
    /// The name tmux knows this prompt by.
    const fn name(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Search => "search",
            Self::Target => "target",
            Self::WindowTarget => "window-target",
        }
    }
}

/// A cloneable handle to one captured tmux server endpoint.
///
/// Equality and hashing use only the captured [`ServerIdentity`]. Clones share
/// capability detection, request IDs, and executor shutdown state. Dropping a handle
/// does not stop the tmux daemon. Call [`Server::shutdown`] to wait for active client
/// cleanup.
///
/// # What is on here
///
/// A tmux server does a great many things, so this type has a great many
/// methods. They fall into a few groups:
///
/// **Connecting.** [`new`] takes the default socket, [`builder`] configures
/// one, and [`from_env`] reads the server this process is already inside.
/// [`is_alive`] and [`check_alive`] answer whether anything is listening.
///
/// **Finding one thing.** [`session`] and [`client`] take names;
/// [`session_by_id`], [`window_by_id`], and [`pane_by_id`] take tmux IDs.
/// Each reports `Ok(None)` when tmux does not have it.
///
/// **Listing everything.** [`sessions`], [`windows`], [`panes`], and
/// [`clients`]. Each keeps the reason it failed, so an outage does not read
/// as an empty server. [`hierarchy`] gathers the whole tree in
/// three tmux commands rather than one per object.
///
/// **Changing things.** [`new_session`], [`kill`], and [`with_session`],
/// which cleans up after itself whether the body succeeded or not.
///
/// **Options and hooks.** [`typed_option`] and [`set_typed_option`] for this
/// server, [`typed_global_option`] and [`set_typed_global_option`] for the
/// session defaults, [`set_option`] to send text tmux's option table does not
/// check, and [`set_hook`] and [`unset_hook`]. [`OptionValue`] says how the
/// reads and writes fit together.
///
/// **Everything else tmux keeps.** Paste buffers ([`buffer`], [`set_buffer`],
/// [`buffer_names`], [`delete_buffer`]), key bindings ([`bind_key`],
/// [`unbind_key`], [`key_bindings`]), format expansion ([`format`]),
/// configuration ([`source_file`]), shell commands ([`run_shell`],
/// [`spawn_shell`]), and wait-for channels ([`lock_channel`],
/// [`unlock_channel`], [`signal_channel`]).
///
/// **Things that need a terminal.** [`display_popup`], [`display_menu`],
/// [`command_prompt`], [`choose`], [`find_window`], and [`display_panes`] all
/// draw on an attached client, and fail without one.
///
/// Anything tmux can do that is not here is reachable through [`cmd`], which
/// runs an arbitrary command and hands back its result.
///
/// [`new`]: Server::new
/// [`builder`]: Server::builder
/// [`from_env`]: Server::from_env
/// [`is_alive`]: Server::is_alive
/// [`check_alive`]: Server::check_alive
/// [`session`]: Server::session
/// [`client`]: Server::client
/// [`session_by_id`]: Server::session_by_id
/// [`window_by_id`]: Server::window_by_id
/// [`pane_by_id`]: Server::pane_by_id
/// [`sessions`]: Server::sessions
/// [`windows`]: Server::windows
/// [`panes`]: Server::panes
/// [`clients`]: Server::clients
/// [`hierarchy`]: Server::hierarchy
/// [`new_session`]: Server::new_session
/// [`kill`]: Server::kill
/// [`with_session`]: Server::with_session
/// [`typed_option`]: Server::typed_option
/// [`set_typed_option`]: Server::set_typed_option
/// [`typed_global_option`]: Server::typed_global_option
/// [`set_typed_global_option`]: Server::set_typed_global_option
/// [`set_option`]: Server::set_option
/// [`OptionValue`]: crate::OptionValue
/// [`set_hook`]: Server::set_hook
/// [`unset_hook`]: Server::unset_hook
/// [`buffer`]: Server::buffer
/// [`set_buffer`]: Server::set_buffer
/// [`buffer_names`]: Server::buffer_names
/// [`delete_buffer`]: Server::delete_buffer
/// [`bind_key`]: Server::bind_key
/// [`unbind_key`]: Server::unbind_key
/// [`key_bindings`]: Server::key_bindings
/// [`format`]: Server::format
/// [`source_file`]: Server::source_file
/// [`run_shell`]: Server::run_shell
/// [`spawn_shell`]: Server::spawn_shell
/// [`lock_channel`]: Server::lock_channel
/// [`unlock_channel`]: Server::unlock_channel
/// [`signal_channel`]: Server::signal_channel
/// [`display_popup`]: Server::display_popup
/// [`display_menu`]: Server::display_menu
/// [`command_prompt`]: Server::command_prompt
/// [`choose`]: Server::choose
/// [`find_window`]: Server::find_window
/// [`display_panes`]: Server::display_panes
/// [`cmd`]: Server::cmd
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use libtmux::test::TestServer;
///
/// // In your own code this is `libtmux::Server::new()?`; the fixture keeps
/// // the example off whichever tmux you are actually using.
/// let guard = TestServer::new()?;
/// let server = guard.server();
///
/// server.new_session("work")?;
/// assert_eq!(server.sessions()?.len(), 1);
///
/// guard.shutdown()?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Server {
    core: Arc<Core>,
}

/// What came of waiting on a `wait-for` channel.
///
/// Running out of time is an outcome rather than an error, because a caller
/// retries "nothing signalled it" and "tmux could not be reached" differently,
/// and an error kind would make them look alike.
///
/// # Examples
///
/// ```
/// use libtmux::ChannelWait;
///
/// fn keep_waiting(outcome: ChannelWait) -> bool {
///     matches!(outcome, ChannelWait::TimedOut)
/// }
///
/// assert!(keep_waiting(ChannelWait::TimedOut));
/// assert!(!keep_waiting(ChannelWait::Signalled));
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ChannelWait {
    /// Something signalled the channel, or a signal was already waiting.
    Signalled,
    /// The time ran out with the channel unsignalled.
    TimedOut,
}

impl Server {
    #[cfg(feature = "control-mode")]
    pub(crate) fn control_request_ids(&self) -> Arc<std::sync::atomic::AtomicU64> {
        self.core.control_request_ids()
    }

    /// Build a handle onto a connection an object already holds.
    ///
    /// Not feature-gated: a `Client` resolving what it is attached to needs a
    /// server to look ids up through, and that has nothing to do with control
    /// mode.
    pub(crate) const fn from_core(core: Arc<Core>) -> Self {
        Self { core }
    }

    /// Clone this handle with a cancellation token for its operations.
    ///
    /// The clone shares command admission, request IDs, capability detection,
    /// channel waits, and shutdown with this handle. Cancelling its token does
    /// not cancel calls made through handles carrying a different token.
    #[must_use]
    pub fn with_cancellation_token(&self, token: crate::CancellationToken) -> Self {
        Self {
            core: self.core.for_cancellation(token),
        }
    }

    #[cfg(feature = "control-mode")]
    pub(crate) fn cancellation_token(&self) -> &crate::CancellationToken {
        self.core.cancellation_token()
    }

    #[cfg(feature = "control-mode")]
    pub(crate) fn spawn_control(&self, session: &SessionId) -> Result<PersistentChild, Error> {
        self.core.spawn_control(session)
    }

    /// Report whether this process itself opened the control client with
    /// this pid, and it is still running.
    ///
    /// [`crate::Client::is_own`] is the usual form, and reads better: ask a handle
    /// from [`Self::clients`] rather than doing pid arithmetic. This one is
    /// for a caller that already has a pid and no handle -- reading
    /// `list-clients` itself to get sessions and pids in one command, which a
    /// per-client lookup would turn into one command each.
    ///
    /// [`crate::Pane::stream_output`], [`crate::Client::pid`] and friends can
    /// all open or report a control-mode connection; tmux counts any of them
    /// as an attached client the same as a human's terminal. A caller telling
    /// its own observation apart from an attached human reads every
    /// [`crate::Client::pid`] it cares about through this rather than
    /// tracking one connection it happened to keep, because more than one may
    /// be open at once.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let guard = libtmux::test::TestServer::new()?;
    /// let server = guard.server();
    ///
    /// // Nothing has opened a control client yet.
    /// assert!(!server.owns_control_client(std::process::id()));
    ///
    /// guard.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "control-mode")]
    #[must_use]
    pub fn owns_control_client(&self, pid: u32) -> bool {
        self.core.owns_control_client(pid)
    }

    /// Construct a server from the captured default endpoint context.
    ///
    /// # Errors
    ///
    /// Returns an error when the working directory or socket root cannot be
    /// captured.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::new()?;
    /// assert!(server.socket_path().is_absolute());
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    pub fn new() -> Result<Self, Error> {
        Self::builder().build()
    }

    /// Start a consuming server builder.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/builder-example.sock")
    ///     .build()?;
    /// assert_eq!(server.socket_path(), std::path::Path::new("/tmp/libtmux-rs-test/builder-example.sock"));
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    pub fn builder() -> ServerBuilder {
        ServerBuilder::new()
    }

    /// Return the captured structural endpoint identity.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/identity-example.sock")
    ///     .build()?;
    /// assert_eq!(server.identity(), server.identity());
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use]
    pub fn identity(&self) -> &ServerIdentity {
        self.core.configuration().identity()
    }

    /// Return the captured absolute socket path used for identity and dispatch.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/socket-example.sock")
    ///     .build()?;
    /// assert!(server.socket_path().is_absolute());
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        self.identity().socket_path()
    }

    /// Return the configured named socket selector, if any.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder().socket_name("example").build()?;
    /// assert_eq!(server.socket_name(), Some(std::ffi::OsStr::new("example")));
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use]
    pub fn socket_name(&self) -> Option<&OsStr> {
        self.core.configuration().socket_name()
    }

    /// Return the captured config path, if configured.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder()
    ///     .config_file("/tmp/libtmux-example.conf")
    ///     .build()?;
    /// assert_eq!(server.config_file(), Some(std::path::Path::new("/tmp/libtmux-example.conf")));
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use]
    pub fn config_file(&self) -> Option<&Path> {
        self.core.configuration().config_file()
    }

    /// Return the configured tmux color mode.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder().colors(256).build()?;
    /// assert_eq!(server.colors(), Some(256));
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use]
    pub fn colors(&self) -> Option<u16> {
        self.core.configuration().colors()
    }

    /// Return the captured tmux executable value.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder().tmux_executable("tmux").build()?;
    /// assert_eq!(server.tmux_executable(), std::ffi::OsStr::new("tmux"));
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use]
    pub fn tmux_executable(&self) -> &OsStr {
        self.core.configuration().executable()
    }

    /// Resolve the configured tmux executable from the captured launch context.
    ///
    /// Bare names use the `PATH` captured by [`ServerBuilder::build`]. Relative
    /// paths use its captured working directory. The returned path preserves a
    /// configured wrapper or symlink rather than canonicalizing it.
    ///
    /// Returns `None` when the configured executable is not on the captured
    /// `PATH`, so a caller can tell "tmux is missing" from "tmux failed".
    ///
    /// # Examples
    ///
    /// ```
    /// // Any absolute path is resolved only if it is a runnable file, so this
    /// // example names one POSIX guarantees rather than tmux's own location,
    /// // which varies by platform.
    /// let server = libtmux::Server::builder()
    ///     .tmux_executable("/bin/sh")
    ///     .build()?;
    /// assert_eq!(
    ///     server.resolved_tmux_executable(),
    ///     Some(std::path::PathBuf::from("/bin/sh")),
    /// );
    ///
    /// let missing = libtmux::Server::builder()
    ///     .tmux_executable("tmux-that-is-not-installed")
    ///     .build()?;
    /// assert_eq!(missing.resolved_tmux_executable(), None);
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use]
    pub fn resolved_tmux_executable(&self) -> Option<PathBuf> {
        self.core.configuration().resolved_executable()
    }

    /// Return the captured per-command timeout.
    ///
    /// # Examples
    ///
    /// ```
    /// let timeout = std::time::Duration::from_secs(7);
    /// let server = libtmux::Server::builder().default_timeout(timeout).build()?;
    /// assert_eq!(server.default_timeout(), timeout);
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use]
    pub fn default_timeout(&self) -> Duration {
        self.core.configuration().timeout()
    }

    /// Detect and share capabilities for the configured tmux executable.
    ///
    /// Failed initialization is not cached and may be retried.
    ///
    /// # Errors
    ///
    /// Returns an error when the executable cannot be run, its version probe
    /// fails, its output is invalid, or the detected version is unsupported.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let server = libtmux::Server::new()?;
    /// assert!(!server.capabilities()?.tmux_version().raw().is_empty());
    /// server.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn capabilities(&self) -> Result<&EngineCapabilities, Error> {
        self.core.capabilities()
    }

    /// Execute one raw logical tmux command.
    ///
    /// A non-zero process exit status is returned in [`CommandResult`]. This
    /// raw boundary does not probe or enforce the supported-version floor;
    /// callers opt into that check with [`Server::capabilities`].
    ///
    /// # Errors
    ///
    /// Returns an error when the process cannot be started, captured, waited for, or is
    /// cancelled by timeout or shutdown.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let server = libtmux::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/command-example-absent.sock")
    ///     .build()?;
    /// let result = server.cmd(libtmux::Command::new("list-sessions"))?;
    /// assert!(result.exit_code().is_some());
    /// server.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn cmd(&self, command: Command) -> Result<CommandResult, Error> {
        self.core.execute(command)
    }

    /// Validate every planned layout before scripts or tmux mutations.
    ///
    /// Each item pairs layout bytes with the required pane count. Classic
    /// layouts are checked for checksums, nonempty trees, unsigned fields and
    /// depth up to 256 nested groups. tmux owns geometry correction and pruning.
    /// JSON layouts require tmux 3.8; tmux validates their contents.
    /// Version-sensitive layouts query the daemon; only a cold endpoint uses
    /// the selected client version. Unique name abbreviations are accepted.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLayout`] for invalid syntax, ambiguous names or
    /// insufficient pane cells, [`Error::UnsupportedCapability`] for layouts
    /// unavailable on the daemon, or the version probe's transport/decode error.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// #  fn validate(server: &libtmux::Server) -> Result<(), libtmux::Error> {
    /// use std::ffi::OsStr;
    /// server.validate_layouts([(OsStr::new("even-h"), 2)])?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn validate_layouts<'a>(
        &self,
        layouts: impl IntoIterator<Item = (&'a OsStr, usize)>,
    ) -> Result<(), Error> {
        let pending = crate::layout::prepare(layouts)?;
        if !pending.is_empty() {
            crate::layout::resolve(&pending, &crate::layout::version(self)?)?;
        }
        Ok(())
    }

    /// Dispatch several commands as one `tmux a \; b` invocation.
    ///
    /// One process, one exit status, one merged stdout. tmux runs the chain up
    /// to the first failure and drops the remainder, so this trades the ability
    /// to tell *which* command failed for a single round trip. The merged
    /// result is identical whichever member failed: use it when the commands
    /// succeed or fail as a unit, and [`Server::cmd`] when you need to know
    /// where a failure happened.
    ///
    /// # Errors
    ///
    /// Returns an error when the process cannot be started, captured, waited for, or is
    /// cancelled by timeout or shutdown. A command tmux refuses is reported through the
    /// returned [`CommandResult`], not as an error.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use libtmux::{Command, CommandChain};
    ///
    /// let server = libtmux::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/chain-example-absent.sock")
    ///     .build()?;
    /// let chain = CommandChain::new(Command::new("list-sessions"))
    ///     .then(Command::new("list-panes"));
    /// let result = server.chain(chain)?;
    /// assert!(result.exit_code().is_some());
    /// server.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn chain(&self, chain: CommandChain) -> Result<CommandResult, Error> {
        self.core.execute_chain(chain)
    }

    /// Stop accepting clients and wait for every active client process.
    ///
    /// Shutdown is idempotent and affects every clone of this server. It never stops
    /// the tmux daemon itself. The call waits for client processes to be reaped. With
    /// control mode enabled, it also closes persistent control connections.
    ///
    /// # Errors
    ///
    /// Propagates failures reported by the shared executor shutdown boundary.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let server = libtmux::Server::new()?;
    /// server.shutdown()?;
    /// server.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn shutdown(&self) -> Result<(), Error> {
        self.core.shutdown()
    }

    /// Build a server from the `TMUX` variable tmux exports into every pane.
    ///
    /// tmux sets `TMUX` to `<socket_path>,<server_pid>,<session_id>`. Only the
    /// socket path is used: the pid and session id are frozen when the pane
    /// spawns, and the session id goes stale as soon as the pane's window is
    /// moved to another session.
    ///
    /// Being frozen is what makes the pid worth something to a caller who
    /// wants to know the daemon has not been replaced since. A server that
    /// restarts on the same socket answers here just as well, and reissues
    /// ids from the start, so [`Server::generation`] and
    /// [`Server::require_generation`] are what tell the two apart. This does
    /// not check on its own initiative, because that would cost a round trip
    /// on a call documented as running no tmux command.
    ///
    /// This runs no tmux command and does not check that the server is alive;
    /// use [`Server::is_alive`] for that.
    ///
    /// # Errors
    ///
    /// Returns an error when `TMUX` is unset, empty, or not shaped like that
    /// triple, and when the socket path it names is unusable.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::ffi::OsString;
    /// let outside = libtmux::Server::from_env_value(None::<OsString>);
    /// assert!(outside.is_err(), "a process outside tmux has no TMUX value");
    ///
    /// let inside = libtmux::Server::from_env_value(Some("/tmp/tmux-1000/default,7,$0"))?;
    /// assert_eq!(inside.socket_path(), std::path::Path::new("/tmp/tmux-1000/default"));
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    pub fn from_env() -> Result<Self, Error> {
        Self::from_env_value(std::env::var_os("TMUX"))
    }

    /// Build a server from an explicit `TMUX` value.
    ///
    /// This resolves on behalf of another pane, or in a test, without touching
    /// the process environment.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is absent, empty, or not shaped like
    /// tmux's triple, and when the socket path it names is unusable.
    pub fn from_env_value(value: Option<impl Into<OsString>>) -> Result<Self, Error> {
        use std::os::unix::ffi::OsStrExt as _;

        let value: OsString = value.map(Into::into).ok_or_else(|| {
            Error::invalid_server_configuration(ServerConfigurationErrorKind::NotInsideTmux)
        })?;

        // The socket path is everything before the first comma. tmux writes a
        // path, and a path may itself contain commas only before that split
        // point is reached, so splitting on the first comma is what tmux's own
        // consumers do.
        let bytes = value.as_bytes();
        let socket = bytes
            .iter()
            .position(|byte| *byte == b',')
            .map(|index| &bytes[..index])
            .filter(|socket| !socket.is_empty())
            .ok_or_else(|| {
                // The variable exists and does not say what tmux says, which
                // is a different problem from not being inside tmux at all.
                Error::invalid_server_configuration(
                    ServerConfigurationErrorKind::MalformedTmuxVariable,
                )
            })?;

        Self::builder()
            .socket_path(PathBuf::from(OsString::from_vec(socket.to_vec())))
            .build()
    }

    /// List the sessions that have at least one client attached.
    ///
    /// # Errors
    ///
    /// Returns an error when the session listing fails.
    pub fn attached_sessions(&self) -> Result<Vec<Session>, Error> {
        let mut sessions = self.sessions()?;
        sessions.retain(Session::is_attached);

        Ok(sessions)
    }

    /// Create a detached session and return it.
    ///
    /// The session is always detached: attaching would take over the calling
    /// process's terminal, which a library must not do on its own initiative.
    ///
    /// tmux prints the new session through the same format machinery as a
    /// listing, so this costs one round trip rather than a create followed by
    /// a lookup.
    ///
    /// tmux expands the name as a format before it checks it, so `#(command)`
    /// in one runs a shell command. See [the crate documentation][crate#a-name-reaches-tmux-as-a-format]
    /// before passing text a caller supplied.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command, which includes a name
    /// that already exists, and when its output cannot be decoded.
    pub fn new_session(&self, options: impl Into<NewSessionOptions>) -> Result<Session, Error> {
        let options = options.into();
        let info = listing::create_session(&self.core, |format| options.into_command(format))?;

        Ok(Session::new(Arc::clone(&self.core), info))
    }

    /// Lock every client on the server.
    ///
    /// Runs each client's `lock-command`, which is `lock -np` unless the
    /// server was told otherwise. Succeeds when nobody is attached, having
    /// locked nobody. [`crate::Session::lock`] narrows this to one session and
    /// [`crate::Client::lock`] to one client.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub fn lock_all(&self) -> Result<(), Error> {
        listing::mutate(&self.core, "lock-server", Command::new("lock-server"))
    }

    /// Stop the tmux daemon at this endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be run. A daemon that was already
    /// stopped is not an error.
    pub fn kill(&self) -> Result<(), Error> {
        let result = self.cmd(Command::new("kill-server"))?;
        if result.success() {
            return Ok(());
        }

        // tmux exits nonzero when no server was running, which is the state
        // the caller asked for.
        if self.is_alive() {
            return Err(Error::CommandFailed {
                command: "kill-server",
                exit_code: result.exit_code(),
                stderr: result.stderr_lossy().into_owned(),
            });
        }
        Ok(())
    }

    /// Store data in a tmux paste buffer.
    ///
    /// Passing `None` for the name lets tmux choose one, matching
    /// `set-buffer` without `-b`. The data is marked sensitive.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the buffer name or data.
    pub fn set_buffer(&self, name: Option<&str>, data: impl Into<OsString>) -> Result<(), Error> {
        let mut command = Command::new("set-buffer");
        if let Some(name) = name {
            command = command.arg("-b").arg(OsString::from(name));
        }
        command = command.arg("--");

        listing::mutate(&self.core, "set-buffer", command.sensitive_arg(data.into()))
    }

    /// Fill a paste buffer from a file, letting tmux read it.
    ///
    /// [`Self::set_buffer`] carries the data as a command argument, which the
    /// kernel caps: on Linux a single argument stops at `MAX_ARG_STRLEN`,
    /// 128 KiB, and a larger one fails with "argument list too long" before
    /// tmux sees it. tmux opens the file itself here, so size is the file
    /// system's problem rather than the command line's.
    ///
    /// `path` is tmux's to resolve, and tmux reads it as the user running the
    /// server, which is not necessarily this process.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot read the path.
    pub fn load_buffer(&self, name: Option<&str>, path: impl AsRef<Path>) -> Result<(), Error> {
        let mut command = Command::new("load-buffer");
        if let Some(name) = name {
            command = command.arg("-b").arg(OsString::from(name));
        }

        listing::mutate(
            &self.core,
            "load-buffer",
            command
                .arg("--")
                .arg(path.as_ref().as_os_str().to_os_string()),
        )
    }

    /// Write a paste buffer to a file, letting tmux do the writing.
    ///
    /// The counterpart to [`Self::load_buffer`], and the same reasoning: the
    /// bytes never cross a command line, so a buffer larger than an argument
    /// can hold still round-trips. tmux writes as the user running the
    /// server.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is missing or tmux cannot write the
    /// path.
    pub fn save_buffer(&self, name: Option<&str>, path: impl AsRef<Path>) -> Result<(), Error> {
        let mut command = Command::new("save-buffer");
        if let Some(name) = name {
            command = command.arg("-b").arg(OsString::from(name));
        }

        listing::mutate(
            &self.core,
            "save-buffer",
            command
                .arg("--")
                .arg(path.as_ref().as_os_str().to_os_string()),
        )
    }

    /// Read a paste buffer's exact bytes.
    ///
    /// Returns `None` when no buffer has that name. Buffer contents are
    /// arbitrary bytes, so this is not a string. The name is bytes too, so
    /// one [`Self::buffer_names`] lists passes back as it is.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached.
    pub fn buffer(&self, name: impl AsRef<[u8]>) -> Result<Option<Vec<u8>>, Error> {
        let result = self.cmd(
            Command::new("show-buffer")
                .arg("-b")
                .arg(OsString::from_vec(name.as_ref().to_vec())),
        )?;

        // tmux exits nonzero for a name it does not have, which is absence
        // rather than failure.
        if result.success() {
            Ok(Some(result.stdout().to_vec()))
        } else {
            Ok(None)
        }
    }

    /// List the paste buffer names.
    ///
    /// A name is [`TmuxText`] because whoever made the buffer chose it: tmux
    /// before 3.7 stores any bytes, a newline included. Each name is read
    /// framed, so one holding a newline or `=` is one name, and
    /// [`Self::buffer`] and [`Self::delete_buffer`] take it back unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the listing, and
    /// [`Error::DecodeListing`] when its output does not decode.
    pub fn buffer_names(&self) -> Result<Vec<TmuxText>, Error> {
        let dialect = TransportDialect::for_version(self.capabilities()?.tmux_version());
        let result = self.cmd(
            Command::new("list-buffers")
                .arg("-F")
                .arg("#{q:buffer_name}="),
        )?;
        if !result.success() {
            return Err(Error::CommandFailed {
                command: "list-buffers",
                exit_code: result.exit_code(),
                stderr: result.stderr_lossy().into_owned(),
            });
        }

        let rows =
            split_quoted_rows(result.stdout(), ["buffer_name"], dialect).map_err(|detail| {
                Error::DecodeListing {
                    list_command: "list-buffers",
                    detail: ListingDecodeError::new(detail),
                }
            })?;
        Ok(rows
            .into_iter()
            .map(|[name]| TmuxText::from(name))
            .collect())
    }

    /// Delete one paste buffer.
    ///
    /// The name is bytes, as [`Self::buffer_names`] lists it.
    ///
    /// # Errors
    ///
    /// Returns an error when no buffer has that name.
    pub fn delete_buffer(&self, name: impl AsRef<[u8]>) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "delete-buffer",
            Command::new("delete-buffer")
                .arg("-b")
                .arg(OsString::from_vec(name.as_ref().to_vec())),
        )
    }

    /// Bind a key in one key table.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the key or the command.
    pub fn bind_key(
        &self,
        table: &str,
        key: &str,
        command: impl Into<OsString>,
    ) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "bind-key",
            Command::new("bind-key")
                .arg("-T")
                .arg(OsString::from(table))
                .arg("--")
                .arg(OsString::from(key))
                .arg(command.into()),
        )
    }

    /// Remove a key binding from one key table.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the key.
    pub fn unbind_key(&self, table: &str, key: &str) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "unbind-key",
            Command::new("unbind-key")
                .arg("-T")
                .arg(OsString::from(table))
                .arg("--")
                .arg(OsString::from(key)),
        )
    }

    /// List key bindings as tmux prints them.
    ///
    /// Each line is a complete `bind-key` command in tmux's own quoting, which
    /// this crate deliberately does not re-parse: the same value is rendered
    /// bare, double quoted, or single quoted depending on content, and a table
    /// name is printed bare, spaces and all. [`Self::typed_key_bindings`]
    /// reads the same bindings as fields on tmux 3.7 and later.
    ///
    /// On tmux 3.7 through 3.7c, a `table` holding exactly one binding lists
    /// empty: those releases send a one-line listing to the message log.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the listing.
    pub fn key_bindings(&self, table: Option<&str>) -> Result<Vec<String>, Error> {
        let mut command = Command::new("list-keys");
        if let Some(table) = table {
            command = command.arg("-T").arg(OsString::from(table));
        }

        let result = self.cmd(command)?;
        if !result.success() {
            return Err(Error::CommandFailed {
                command: "list-keys",
                exit_code: result.exit_code(),
                stderr: result.stderr_lossy().into_owned(),
            });
        }

        Ok(result
            .stdout_lossy()
            .lines()
            .map(ToOwned::to_owned)
            .collect())
    }

    /// Expand a tmux format string and return the result.
    ///
    /// This is `display-message -p`, whose target is a pane: the format is
    /// evaluated against it, so `#{pane_current_command}` and the session and
    /// window fields around it all resolve from there.
    ///
    /// The result is [`TmuxText`] because a format can interpolate names that
    /// are not valid UTF-8.
    ///
    /// A literal `#(command)` starts `command` in a shell. Recursive expansion,
    /// such as `#{E:status-left}`, can also expose a command stored in the
    /// expanded value. The job is asynchronous, so this call may return before
    /// it produces output. Escaping the outer template with `##` does not
    /// escape text introduced by a recursive expansion. Use only validated,
    /// simple `#{field}` lookups with untrusted input.
    ///
    /// A `%` in the template is a time conversion, not literal text. tmux
    /// expands this command through `strftime` before it expands `#{}` -- it
    /// calls `format_expand_time` here and plain `format_expand` for every
    /// listing -- so `%Y` becomes the year. Write `%%` for a literal `%`.
    ///
    /// A `%` followed by something `strftime` does not define is worse than
    /// surprising: it is not portable. glibc keeps both characters, so the
    /// template survives; Apple's libc emits the conversion character alone
    /// and deletes the `%`. So a template separating fields with `%` reads
    /// back correctly on Linux and comes back unframed on macOS. Separate
    /// fields with a byte that is not `%`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the pane belongs to another
    /// server, or an error when tmux rejects the format or the pane is gone.
    pub fn format(&self, pane: Option<&Pane>, format: &str) -> Result<TmuxText, Error> {
        let mut command = Command::new("display-message").arg("-p");
        if let Some(pane) = pane {
            self.core
                .require_same_server(pane.server_identity(), "display-message")?;
            command = command.arg("-t").arg(pane.id().to_string());
        }

        let result = self.cmd(command.arg("--").arg(OsString::from(format)))?;
        if !result.success() {
            return Err(Error::from_refused_result("display-message", &result, None));
        }

        // `-p` terminates its output with a newline that is framing rather
        // than part of the expansion.
        let stdout = result.stdout();
        let value = stdout.strip_suffix(b"\n").unwrap_or(stdout);

        Ok(TmuxText::from(value.to_vec()))
    }

    /// Read this server's release and refuse a capability it is too old for.
    pub(crate) fn require(
        &self,
        capability: &'static str,
        needs: ReleaseVersion,
    ) -> Result<(), Error> {
        self.capabilities()?
            .tmux_version()
            .require(capability, needs)
    }

    /// Refuse when the running release is inside a range that gets it wrong.
    ///
    /// `require` cannot express this: it asks for a floor, and a floor would
    /// refuse the older releases that work.
    fn refuse_if_defective(
        &self,
        capability: &'static str,
        broken_in: ReleaseVersion,
        fixed_in: ReleaseVersion,
    ) -> Result<(), Error> {
        let found = self.capabilities()?.tmux_version();
        // A development build carries no numbered release to place in the
        // range, so it is taken at its word rather than refused, as in
        // `require`.
        if found
            .behavior_release()
            .is_some_and(|release| release >= broken_in && release < fixed_in)
        {
            return Err(Error::CapabilityDefective {
                capability,
                found: found.clone(),
                broken_in,
                fixed_in,
            });
        }

        Ok(())
    }

    /// The entries tmux remembers for one kind of prompt.
    ///
    /// tmux keeps a separate history per prompt kind, so the kind is required
    /// rather than defaulted: asking without one returns every kind at once,
    /// which is a different question.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] below tmux 3.3, and an error
    /// when tmux cannot be reached or refuses the listing.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use libtmux::PromptKind;
    ///
    /// let guard = libtmux::test::TestServer::new()?;
    /// let server = guard.server();
    /// let version = server.capabilities()?.tmux_version().clone();
    ///
    /// // A fresh server has answered no prompts.
    /// if version.has_behavior(&libtmux::since::PROMPT_HISTORY) {
    ///     assert!(server.prompt_history(PromptKind::Command)?.is_empty());
    /// }
    ///
    /// guard.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn prompt_history(&self, kind: PromptKind) -> Result<Vec<TmuxText>, Error> {
        self.require("prompt history", PROMPT_HISTORY_SINCE)?;

        let result = self.cmd(
            Command::new("show-prompt-history")
                .arg("-T")
                .arg(kind.name()),
        )?;
        if !result.success() {
            return Err(Error::from_refused_result(
                "show-prompt-history",
                &result,
                None,
            ));
        }

        // tmux heads each kind with `History for <kind>:` and lists the
        // entries under it, so the header is framing rather than an entry.
        Ok(result
            .stdout_lossy()
            .lines()
            .skip_while(|line| line.starts_with("History for "))
            .filter(|line| !line.is_empty())
            .map(|line| TmuxText::from(line.as_bytes().to_vec()))
            .collect())
    }

    /// Forget the entries tmux remembers for prompts.
    ///
    /// Clears every kind, which is what tmux's own command does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] below tmux 3.3, and an error
    /// when tmux cannot be reached or refuses the command.
    pub fn clear_prompt_history(&self) -> Result<(), Error> {
        self.require("prompt history", PROMPT_HISTORY_SINCE)?;

        listing::mutate(
            &self.core,
            "clear-prompt-history",
            Command::new("clear-prompt-history"),
        )
    }

    /// The users on this server's access list.
    ///
    /// The owner is always present and always writable, and tmux refuses to
    /// change their entry, so a caller cannot lock itself out.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] below tmux 3.3, and an error
    /// when tmux cannot be reached or refuses the listing.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let guard = libtmux::test::TestServer::new()?;
    /// let version = guard.server().capabilities()?.tmux_version().clone();
    ///
    /// // Whoever started the server owns it and may act.
    /// if version.has_behavior(&libtmux::since::SERVER_ACCESS) {
    ///     let rules = guard.server().access_rules()?;
    ///     assert_eq!(rules.len(), 1);
    ///     assert_eq!(rules[0].mode(), libtmux::AccessMode::Write);
    /// }
    ///
    /// guard.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn access_rules(&self) -> Result<Vec<AccessRule>, Error> {
        self.require("the server access list", SERVER_ACCESS_SINCE)?;

        let result = self.cmd(Command::new("server-access").arg("-l"))?;
        if !result.success() {
            return Err(Error::from_refused_result("server-access", &result, None));
        }

        result
            .stdout_lossy()
            .lines()
            .map(parse_access_rule)
            .collect()
    }

    /// Let a user attach to this server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] below tmux 3.3, and an error
    /// when tmux refuses -- which it does for the user who owns the server.
    pub fn grant_access(&self, user: &str, mode: AccessMode) -> Result<(), Error> {
        self.require("the server access list", SERVER_ACCESS_SINCE)?;

        listing::mutate(
            &self.core,
            "server-access",
            Command::new("server-access")
                .arg("-a")
                .arg(match mode {
                    AccessMode::ReadOnly => "-r",
                    AccessMode::Write => "-w",
                })
                .arg("--")
                .arg(OsString::from(user)),
        )
    }

    /// Stop a user attaching to this server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] below tmux 3.3, and an error
    /// when tmux refuses -- which it does for the user who owns the server.
    pub fn revoke_access(&self, user: &str) -> Result<(), Error> {
        self.require("the server access list", SERVER_ACCESS_SINCE)?;

        listing::mutate(
            &self.core,
            "server-access",
            Command::new("server-access")
                .arg("-d")
                .arg("--")
                .arg(OsString::from(user)),
        )
    }

    /// Load a tmux configuration file into this server.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot read the file or a command in it
    /// fails.
    pub fn source_file(&self, path: impl Into<PathBuf>) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "source-file",
            Command::new("source-file")
                .arg("--")
                .arg(path.into().into_os_string()),
        )
    }

    /// Create a session, run an operation with it, then kill it.
    ///
    /// [`crate::ScopeError`] retains creation, operation and cleanup failures
    /// separately. If operation and cleanup both fail, both original errors
    /// are returned. Cleanup errors carry [`Error::AfterEffect`] because
    /// creation succeeded.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ScopeError`] when creation, the operation, or cleanup
    /// fails. The operation's error needs no conversion into [`Error`].
    ///
    /// # Cancellation
    ///
    /// Cancellation before creation starts leaves tmux untouched. After creation
    /// starts, the call waits for the resource ID and kills the session before
    /// reporting cancellation. Once the callback starts, cancellation is cooperative:
    /// its tmux operations observe the token. Cleanup runs before return and during
    /// panic unwinding, using an uncancelled handle.
    pub fn with_session<T, E>(
        &self,
        options: impl Into<NewSessionOptions>,
        operation: impl FnOnce(&Session) -> Result<T, E>,
    ) -> Result<T, crate::ScopeError<T, E>> {
        let mut server = self.clone();
        server.core = self.core.for_cleanup();
        let callback_core = Arc::clone(&self.core);
        let options = options.into();
        scoped::run(
            self.core.cancellation_token(),
            "with-session",
            move || {
                server
                    .new_session(options)
                    .map(|resource| resource.with_core(callback_core))
            },
            Session::kill_for_cleanup,
            operation,
        )
    }

    /// Run a shell command through tmux and collect its output.
    ///
    /// The command runs in tmux's own environment, not the caller's. Output
    /// lines are [`TmuxText`] because a shell can emit any bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    ///
    /// Also refuses outright on tmux 3.3, 3.3a, and 3.4, with
    /// [`Error::CapabilityDefective`]. Those releases send `run-shell` output
    /// into a pane's copy-mode buffer instead of to the client, and still exit
    /// zero, so the command appears to have produced nothing. Reporting an
    /// empty listing there would be a wrong answer the caller could not
    /// detect. 3.2a and 3.5 onwards are unaffected.
    ///
    /// # Cancellation
    ///
    /// The effect may have happened, and may still be happening. The shell command runs
    /// under the tmux server, not this dispatch, so cancelling the call stops waiting
    /// for it without stopping it, and its output is lost.
    pub fn run_shell(&self, command: impl Into<OsString>) -> Result<Vec<TmuxText>, Error> {
        self.refuse_if_defective(
            "run-shell output",
            ReleaseVersion::new(3, 3, ReleaseSuffix::FINAL),
            ReleaseVersion::new(3, 5, ReleaseSuffix::FINAL),
        )?;

        let result = self.cmd(
            Command::new("run-shell")
                .arg("--")
                .sensitive_arg(command.into()),
        )?;
        if !result.success() {
            return Err(Error::from_refused_result("run-shell", &result, None));
        }

        let stdout = result.stdout();
        let stdout = stdout.strip_suffix(b"\n").unwrap_or(stdout);
        if stdout.is_empty() {
            return Ok(Vec::new());
        }

        Ok(stdout
            .split(|byte| *byte == b'\n')
            .map(|line| TmuxText::from(line.to_vec()))
            .collect())
    }

    /// Run a shell command in the background and return immediately.
    ///
    /// Success means tmux accepted the command, not that it finished or
    /// succeeded. Nothing is captured.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses to enqueue the command.
    pub fn spawn_shell(&self, command: impl Into<OsString>) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "run-shell",
            Command::new("run-shell")
                .arg("-b")
                .arg("--")
                .sensitive_arg(command.into()),
        )
    }

    /// Start a tmux server without creating a session.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be run.
    pub fn start(&self) -> Result<(), Error> {
        listing::mutate(&self.core, "start-server", Command::new("start-server"))
    }

    /// Which tmux daemon is answering on this endpoint.
    ///
    /// A socket path does not identify a daemon. tmux reuses the socket file
    /// across restarts -- it survives `kill-server`, and a replacement daemon
    /// binds the same inode -- so an endpoint that looks unchanged can be a
    /// different server holding different objects under the same ids. A pane
    /// handle for `%0` taken before a restart addresses the *replacement's*
    /// `%0` afterwards, which for a mutation is the wrong object rather than a
    /// missing one.
    ///
    /// Capture this before work that must not be misapplied, and check it with
    /// [`Self::require_generation`] before acting on a handle that has been
    /// held across time.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached, or answers with something
    /// that is not a pid and a start time this platform's `SystemTime` holds.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let guard = libtmux::test::TestServer::new()?;
    /// let server = guard.server();
    /// server.new_session("work")?;
    ///
    /// let generation = server.generation()?;
    /// // Unchanged while the daemon is the same one.
    /// server.require_generation(generation)?;
    ///
    /// guard.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn generation(&self) -> Result<ServerGeneration, Error> {
        // Both are server-scoped and available in every list profile since
        // 3.2a. The start time is what defeats pid reuse: a replacement daemon
        // can be handed the pid of the one it replaced.
        let answer = self.format(None, "#{pid} #{start_time}")?;
        let text = answer.to_string_lossy();
        let mut parts = text.split_whitespace();
        let pid = parts.next().and_then(|pid| pid.parse::<u32>().ok());
        let start_time = parts
            .next()
            .and_then(|start| start.parse::<i64>().ok())
            .filter(|start| crate::snapshot::unix_time(*start).is_some());

        let (Some(pid), Some(start_time)) = (pid, start_time) else {
            return Err(Error::UnreadableFormatValue {
                format: "#{pid} #{start_time}",
                detail: crate::IdParseError::new('#'),
            });
        };

        Ok(ServerGeneration { pid, start_time })
    }

    /// Fail unless the daemon answering is still the one that was captured.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerGenerationChanged`] when a different daemon now
    /// holds this endpoint, or a transport error when tmux cannot be reached.
    pub fn require_generation(&self, expected: ServerGeneration) -> Result<(), Error> {
        let found = self.generation()?;
        if found == expected {
            return Ok(());
        }

        Err(Error::ServerGenerationChanged { expected, found })
    }

    /// Report whether a tmux daemon is answering at this endpoint.
    ///
    /// Every failure becomes `false`, including a missing executable. Use
    /// [`Server::check_alive`] when the difference matters.
    #[must_use = "inspect whether the server is reachable"]
    pub fn is_alive(&self) -> bool {
        self.check_alive().is_ok()
    }

    /// Require a tmux daemon to be answering at this endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerGone`] when no daemon is answering, or another
    /// dispatch error when tmux cannot be asked.
    pub fn check_alive(&self) -> Result<(), Error> {
        let result = self.cmd(Command::new("list-sessions"))?;
        if result.success() {
            return Ok(());
        }

        Err(Error::from_refused_result("list-sessions", &result, None))
    }

    #[cfg(test)]
    pub(crate) fn from_executor_for_test(
        executor: Arc<dyn crate::internal::executor::Executor>,
    ) -> Self {
        Self {
            core: Arc::new(Core::from_executor_for_test(executor)),
        }
    }

    /// Return a handle whose commands run over an open control connection.
    ///
    /// Every typed call on the returned server -- `sessions`, `send_keys`,
    /// `capture`, the option and hook accessors -- is written to `sender` as
    /// one control-mode line and answered from its `%begin`/`%end` block,
    /// rather than spawning `tmux`. Handles reached through it inherit the
    /// route, because they carry the same connection.
    ///
    /// The original server is unchanged and still dispatches processes. Use it
    /// for the two things a connection is wrong for: a command that answers at
    /// once and then parks the client's queue, such as `wait-for` or a
    /// foreground `run-shell`, and a command whose arguments are not valid
    /// UTF-8, which a text protocol cannot carry.
    ///
    /// Commands wait for the sender's own [`reply_timeout`], not this server's
    /// [`default_timeout`]. They are not the same budget: one bounds a round
    /// trip on an open connection, the other bounds forking tmux.
    ///
    /// # Draining events
    ///
    /// The connection stops reading tmux once a caller is far enough behind on
    /// [`ControlEvents`], and refuses commands past that. Either keep reading
    /// events, drop the watching half, or narrow what tmux reports with
    /// [`ControlSender::watch_only`].
    ///
    /// # Errors
    ///
    /// Returns an error when the sender reaches a different server, or when
    /// the tmux version cannot be detected. Detection runs here, once, through
    /// this server's process transport, because `tmux -V` is a client flag and
    /// has no control-mode spelling; the returned handle never probes.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use libtmux::control::ControlMode;
    /// use libtmux::test::TestServer;
    ///
    /// let guard = TestServer::new()?;
    /// let session = guard.server().new_session("routed")?;
    ///
    /// let (sender, events) = ControlMode::attach(guard.server(), session.id())
    ///     ?
    ///     .split();
    /// let routed = guard.server().over_control_mode(&sender)?;
    ///
    /// // One line on the connection, not a process.
    /// assert_eq!(routed.sessions()?.len(), 1);
    ///
    /// // And a handle it found keeps the route.
    /// let pane = routed.panes()?.remove(0);
    /// pane.send_line("true")?;
    ///
    /// events.shutdown()?;
    /// guard.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [`reply_timeout`]: crate::control::ControlSender::reply_timeout
    /// [`default_timeout`]: Server::default_timeout
    /// [`ControlEvents`]: crate::control::ControlEvents
    /// [`ControlSender::watch_only`]: crate::control::ControlSender::watch_only
    #[cfg(feature = "control-mode")]
    pub fn over_control_mode(&self, sender: &crate::control::ControlSender) -> Result<Self, Error> {
        self.core
            .require_same_server(sender.identity(), "over_control_mode")?;
        let capabilities = self.core.capabilities()?.clone();

        Ok(Self {
            core: Arc::new(self.core.over_control_mode(sender.clone(), capabilities)),
        })
    }

    /// Whether this handle came from [`Self::over_control_mode`].
    #[cfg(all(feature = "control-mode", feature = "plan"))]
    pub(crate) fn routes_over_control_mode(&self) -> bool {
        self.core.routes_over_control_mode()
    }
}

impl PartialEq for Server {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

impl Eq for Server {}

impl Hash for Server {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identity().hash(state);
    }
}

impl fmt::Debug for Server {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Server")
            .field("identity", self.identity())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "server/sync_tests.rs"]
mod tests;

/// Options for creating a session.
///
/// A bare name is accepted wherever this is, so the common case stays short:
/// `server.new_session("work")`.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use libtmux::NewSessionOptions;
///
/// let guard = libtmux::test::TestServer::new()?;
/// let server = guard.server();
///
/// let session = server
///     .new_session(NewSessionOptions::new("work").window_name("editor"))
///     ?;
///
/// let window = session.active_window()?.expect("a session has a window");
/// assert_eq!(window.name().to_string_lossy(), "editor");
///
/// guard.shutdown()?;
/// # Ok(())
/// # }
/// ```
#[must_use = "options describe a session but do not create one"]
#[derive(Clone)]
pub struct NewSessionOptions {
    name: TmuxArg,
    start_directory: Option<TmuxArg>,
    window_name: Option<TmuxArg>,
    command: Option<OsString>,
    environment: Vec<(OsString, OsString)>,
    width: Option<u32>,
    height: Option<u32>,
}

impl fmt::Debug for NewSessionOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewSessionOptions")
            .field("has_start_directory", &self.start_directory.is_some())
            .field("has_window_name", &self.window_name.is_some())
            .field("has_command", &self.command.is_some())
            .field("environment_count", &self.environment.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl NewSessionOptions {
    /// Describe a session with the given name.
    ///
    /// The name is sent literally. [`TmuxArg::format`] opts into expansion.
    pub fn new(name: impl Into<TmuxArg>) -> Self {
        Self {
            name: name.into(),
            start_directory: None,
            window_name: None,
            command: None,
            environment: Vec::new(),
            width: None,
            height: None,
        }
    }

    /// Set the working directory for the session's first window.
    ///
    /// The directory is sent literally. [`TmuxArg::format`] opts into
    /// expansion, which is how `#{pane_current_path}` is asked for.
    pub fn start_directory(mut self, directory: impl Into<TmuxArg>) -> Self {
        self.start_directory = Some(directory.into());
        self
    }

    /// Name the session's first window, literally.
    pub fn window_name(mut self, name: impl Into<TmuxArg>) -> Self {
        self.window_name = Some(name.into());
        self
    }

    /// Run a command instead of the default shell.
    pub fn command(mut self, command: impl Into<OsString>) -> Self {
        self.command = Some(command.into());
        self
    }

    /// Set an environment variable for the process the new session starts.
    ///
    /// Call this more than once for more than one variable. tmux applies
    /// these to the new process only, not to the session. The value is not
    /// format-expanded, so it needs no escaping.
    pub fn environment(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((name.into(), value.into()));
        self
    }

    /// Set the initial size, which a detached session would otherwise default.
    ///
    /// tmux 3.2a accepts this and sets `default-size` as asked, but still
    /// draws the new, client-less window at its own classic default,
    /// `80x23`: a pane's reported width and height on 3.2a do not reflect
    /// this call, even though the option was set. Every later release draws
    /// at the requested size.
    pub fn size(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    /// Lower these options into a detached `new-session` command.
    ///
    /// `print_format` is placed with the other flags because tmux stops
    /// parsing flags at the first positional, and the shell command is one.
    fn into_command(self, print_format: &str) -> Command {
        // Always detached: attaching would take over the calling terminal,
        // which a library must never do on the caller's behalf.
        let mut command = Command::new("new-session")
            .arg("-d")
            .arg("-P")
            .arg("-F")
            .arg(print_format)
            .arg("-s")
            .arg(self.name.into_os_string());
        if let Some(directory) = self.start_directory {
            command = command.arg("-c").arg(directory.into_os_string());
        }
        if let Some(name) = self.window_name {
            command = command.arg("-n").arg(name.into_os_string());
        }
        if let (Some(width), Some(height)) = (self.width, self.height) {
            command = command
                .arg("-x")
                .arg(width.to_string())
                .arg("-y")
                .arg(height.to_string());
        }
        for (name, value) in self.environment {
            command = command
                .arg("-e")
                .sensitive_arg(crate::window::assignment(&name, &value));
        }
        if let Some(shell_command) = self.command {
            command = command.arg("--").sensitive_arg(shell_command);
        }
        command
    }
}

impl<T: Into<OsString>> From<T> for NewSessionOptions {
    fn from(name: T) -> Self {
        let name: OsString = name.into();
        Self::new(name)
    }
}

/// One of tmux's interactive choosers.
///
/// Each opens a mode on a client and returns as soon as tmux accepts it; the
/// user's eventual choice is not reported back here.
///
/// # Examples
///
/// ```no_run
/// #  fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
/// use libtmux::Chooser;
///
/// // A chooser draws in an attached client's terminal, so this needs one; with
/// // no client tmux has nowhere to put it.
/// let client = server.clients()?.into_iter().next();
/// server.choose(Chooser::Tree, client.as_ref())?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Chooser {
    /// Browse sessions, windows, and panes together.
    Tree,
    /// Browse attached clients.
    Client,
    /// Browse paste buffers.
    Buffer,
    /// Customize options and key bindings.
    Customize,
}

impl Chooser {
    /// Return the tmux command that opens this chooser.
    const fn command(self) -> &'static str {
        match self {
            Self::Tree => "choose-tree",
            Self::Client => "choose-client",
            Self::Buffer => "choose-buffer",
            Self::Customize => "customize-mode",
        }
    }
}
