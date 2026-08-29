use std::ffi::{OsStr, OsString};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::client::Client;
use crate::formats::TmuxText;
use crate::internal::core::Core;
#[cfg(test)]
use crate::internal::executor::Executor;
use crate::internal::listing;
#[cfg(feature = "control-mode")]
use crate::internal::process::PersistentChild;
use crate::internal::scoped;
use crate::pane::Pane;
use crate::session::Session;
#[cfg(feature = "control-mode")]
use crate::SessionId;
use crate::{
    Command, CommandChain, CommandResult, EngineCapabilities, Error, ReleaseSuffix, ReleaseVersion,
    ServerConfigurationErrorKind, ServerGeneration, ServerIdentity,
};

mod builder;
mod discovery;
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
/// # async fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
/// use libtmux::AccessMode;
///
/// // Server access control is tmux 3.3 and later. `ReadOnly` lets another user
/// // watch without being able to send keys.
/// server.grant_access("observer", AccessMode::ReadOnly).await?;
/// server.grant_access("pair", AccessMode::Write).await?;
///
/// let rules = server.access_rules().await?;
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

/// One entry of the server's access list.
///
/// # Examples
///
/// ```no_run
/// # async fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
/// use libtmux::AccessMode;
///
/// for rule in server.access_rules().await? {
///     if rule.mode() == AccessMode::Write {
///         println!("{} can type", rule.user());
///     }
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessRule {
    user: String,
    mode: AccessMode,
}

impl AccessRule {
    /// The user this entry names.
    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    /// What that user may do.
    #[must_use]
    pub const fn mode(&self) -> AccessMode {
        self.mode
    }
}

/// The first tmux release that remembers prompt history.
use crate::version::since::PROMPT_HISTORY as PROMPT_HISTORY_SINCE;

/// Which prompt tmux is remembering answers for.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::PromptKind;
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let version = guard.server().capabilities().await?.tmux_version().clone();
///
/// // tmux keeps a separate history per prompt kind, so a command typed at the
/// // `:` prompt is not offered when searching.
/// if version.meets(&libtmux::since::PROMPT_HISTORY) {
///     assert!(guard.server().prompt_history(PromptKind::Command).await?.is_empty());
///     assert!(guard.server().prompt_history(PromptKind::Search).await?.is_empty());
/// }
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
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
/// capability detection, request IDs, and executor shutdown state.
/// Dropping a handle is nonblocking and does not stop the tmux daemon. Runtime
/// owners should await [`Server::shutdown`] before tearing down Tokio when
/// deterministic client-child cleanup is required.
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
/// [`clients`], each with an `_or_empty` twin that reports no rows rather
/// than the reason for a failure. [`hierarchy`] gathers the whole tree in
/// three tmux commands rather than one per object.
///
/// **Changing things.** [`new_session`], [`kill`], and [`with_session`],
/// which cleans up after itself whether the body succeeded or not.
///
/// **Options and hooks.** [`get_option`] and [`set_option`] for this server,
/// [`get_global_option`] and [`set_global_option`] for the session and window
/// defaults, [`typed_option`] to get a value tmux's own schema has typed, and
/// [`set_hook`] and [`unset_hook`].
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
/// [`get_option`]: Server::get_option
/// [`set_option`]: Server::set_option
/// [`get_global_option`]: Server::get_global_option
/// [`set_global_option`]: Server::set_global_option
/// [`typed_option`]: Server::typed_option
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
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::test::TestServer;
///
/// // In your own code this is `libtmux::Server::new()?`; the fixture keeps
/// // the example off whichever tmux you are actually using.
/// let guard = TestServer::new().await?;
/// let server = guard.server();
///
/// server.new_session("work").await?;
/// assert_eq!(server.sessions().await?.len(), 1);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
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
    /// Build a handle onto a connection an object already holds.
    ///
    /// Not feature-gated: a `Client` resolving what it is attached to needs a
    /// server to look ids up through, and that has nothing to do with control
    /// mode.
    pub(crate) const fn from_core(core: Arc<Core>) -> Self {
        Self { core }
    }

    #[cfg(feature = "control-mode")]
    pub(crate) async fn spawn_control(
        &self,
        session: &SessionId,
    ) -> Result<PersistentChild, Error> {
        self.core.spawn_control(session).await
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
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::new()?;
    /// assert!(!server.capabilities().await?.tmux_version().raw().is_empty());
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn capabilities(&self) -> Result<&EngineCapabilities, Error> {
        self.core.capabilities().await
    }

    /// Execute one raw logical tmux command.
    ///
    /// A non-zero process exit status is returned in [`CommandResult`]. This
    /// raw boundary does not probe or enforce the supported-version floor;
    /// callers opt into that check with [`Server::capabilities`].
    ///
    /// # Errors
    ///
    /// Returns an error when the process cannot be started, captured, awaited,
    /// or is cancelled by timeout or shutdown.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/command-example-absent.sock")
    ///     .build()?;
    /// let result = server.cmd(libtmux::Command::new("list-sessions")).await?;
    /// assert!(result.exit_code().is_some());
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn cmd(&self, command: Command) -> Result<CommandResult, Error> {
        self.core.execute(command).await
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
    /// Returns an error when the process cannot be started, captured, awaited,
    /// or is cancelled by timeout or shutdown. A command tmux refuses is
    /// reported through the returned [`CommandResult`], not as an error.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::{Command, CommandChain};
    ///
    /// let server = libtmux::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/chain-example-absent.sock")
    ///     .build()?;
    /// let chain = CommandChain::new(Command::new("list-sessions"))
    ///     .then(Command::new("list-panes"));
    /// let result = server.chain(chain).await?;
    /// assert!(result.exit_code().is_some());
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn chain(&self, chain: CommandChain) -> Result<CommandResult, Error> {
        self.core.execute_chain(chain).await
    }

    /// Stop accepting clients and wait for every active client process.
    ///
    /// Shutdown is idempotent and affects every clone of this server.
    /// It never stops the tmux daemon itself. Await it before dropping the
    /// Tokio runtime when deterministic reaping matters. With control mode
    /// enabled, this also closes persistent control connections.
    ///
    /// # Errors
    ///
    /// Propagates failures reported by the shared executor shutdown boundary.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::new()?;
    /// server.shutdown().await?;
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn shutdown(&self) -> Result<(), Error> {
        self.core.shutdown().await
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
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

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
    /// This is the lenient form; use [`Server::attached_sessions`] when the
    /// reason for an empty result matters.
    pub async fn attached_sessions_or_empty(&self) -> Vec<Session> {
        self.attached_sessions().await.unwrap_or_else(|error| {
            listing::trace_discarded("list-sessions", &error);
            Vec::new()
        })
    }

    /// List the sessions that have at least one client attached.
    ///
    /// # Errors
    ///
    /// Returns an error when the session listing fails.
    pub async fn attached_sessions(&self) -> Result<Vec<Session>, Error> {
        let mut sessions = self.sessions().await?;
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
    pub async fn new_session(
        &self,
        options: impl Into<NewSessionOptions>,
    ) -> Result<Session, Error> {
        let options = options.into();
        let info =
            listing::create_session(&self.core, |format| options.into_command(format)).await?;

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
    pub async fn lock_all(&self) -> Result<(), Error> {
        listing::mutate(&self.core, "lock-server", Command::new("lock-server")).await
    }

    /// Stop the tmux daemon at this endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be run. A daemon that was already
    /// stopped is not an error.
    pub async fn kill(&self) -> Result<(), Error> {
        let result = self.cmd(Command::new("kill-server")).await?;
        if result.success() {
            return Ok(());
        }

        // tmux exits nonzero when no server was running, which is the state
        // the caller asked for.
        if self.is_alive().await {
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
    pub async fn set_buffer(
        &self,
        name: Option<&str>,
        data: impl Into<OsString>,
    ) -> Result<(), Error> {
        let mut command = Command::new("set-buffer");
        if let Some(name) = name {
            command = command.arg("-b").arg(OsString::from(name));
        }

        listing::mutate(&self.core, "set-buffer", command.sensitive_arg(data.into())).await
    }

    /// Read a paste buffer's exact bytes.
    ///
    /// Returns `None` when no buffer has that name. Buffer contents are
    /// arbitrary bytes, so this is not a string.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached.
    pub async fn buffer(&self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        let result = self
            .cmd(
                Command::new("show-buffer")
                    .arg("-b")
                    .arg(OsString::from(name)),
            )
            .await?;

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
    /// A name containing a newline cannot be told apart from two names,
    /// because tmux separates them with newlines and offers no framed form
    /// for this listing. Names come from [`Server::set_buffer`], so a caller
    /// that avoids newlines avoids the ambiguity.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the listing.
    pub async fn buffer_names(&self) -> Result<Vec<String>, Error> {
        let result = self
            .cmd(Command::new("list-buffers").arg("-F").arg("#{buffer_name}"))
            .await?;
        if !result.success() {
            return Err(Error::CommandFailed {
                command: "list-buffers",
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

    /// Delete one paste buffer.
    ///
    /// # Errors
    ///
    /// Returns an error when no buffer has that name.
    pub async fn delete_buffer(&self, name: &str) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "delete-buffer",
            Command::new("delete-buffer")
                .arg("-b")
                .arg(OsString::from(name)),
        )
        .await
    }

    /// Bind a key in one key table.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the key or the command.
    pub async fn bind_key(
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
                .arg(OsString::from(key))
                .arg(command.into()),
        )
        .await
    }

    /// Remove a key binding from one key table.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the key.
    pub async fn unbind_key(&self, table: &str, key: &str) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "unbind-key",
            Command::new("unbind-key")
                .arg("-T")
                .arg(OsString::from(table))
                .arg(OsString::from(key)),
        )
        .await
    }

    /// List key bindings as tmux prints them.
    ///
    /// Each line is a complete `bind-key` command in tmux's own quoting, which
    /// this crate deliberately does not re-parse: the same value is rendered
    /// bare, double quoted, or single quoted depending on content.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the listing.
    pub async fn key_bindings(&self, table: Option<&str>) -> Result<Vec<String>, Error> {
        let mut command = Command::new("list-keys");
        if let Some(table) = table {
            command = command.arg("-T").arg(OsString::from(table));
        }

        let result = self.cmd(command).await?;
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
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the pane belongs to another
    /// server, or an error when tmux rejects the format or the pane is gone.
    pub async fn format(&self, pane: Option<&Pane>, format: &str) -> Result<TmuxText, Error> {
        let mut command = Command::new("display-message").arg("-p");
        if let Some(pane) = pane {
            self.core
                .require_same_server(pane.server_identity(), "display-message")?;
            command = command.arg("-t").arg(pane.id().to_string());
        }

        let result = self.cmd(command.arg(OsString::from(format))).await?;
        if !result.success() {
            return Err(Error::from_refused_result("display-message", &result, None));
        }

        // `-p` terminates its output with a newline that is framing rather
        // than part of the expansion.
        let stdout = result.stdout();
        let value = stdout.strip_suffix(b"\n").unwrap_or(stdout);

        Ok(TmuxText::from(value.to_vec()))
    }

    /// Refuse a capability the running tmux is too old for.
    ///
    /// Checked rather than left to tmux, which usually accepts an unknown
    /// flag and ignores it: without this, "your tmux is too old" arrives as
    /// "the command did nothing".
    pub(crate) async fn require(
        &self,
        capability: &'static str,
        needs: ReleaseVersion,
    ) -> Result<(), Error> {
        let found = self.capabilities().await?.tmux_version();
        // A development build carries no numbered release to compare, so it
        // is taken at its word rather than refused.
        if found
            .behavior_release()
            .is_some_and(|release| release < needs)
        {
            return Err(Error::UnsupportedCapability {
                capability,
                needs,
                found: found.clone(),
            });
        }

        Ok(())
    }

    /// Refuse when the running release is inside a range that gets it wrong.
    ///
    /// `require` cannot express this: it asks for a floor, and a floor would
    /// refuse the older releases that work.
    async fn refuse_if_defective(
        &self,
        capability: &'static str,
        broken_in: ReleaseVersion,
        fixed_in: ReleaseVersion,
    ) -> Result<(), Error> {
        let found = self.capabilities().await?.tmux_version();
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
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::PromptKind;
    ///
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let server = guard.server();
    /// let version = server.capabilities().await?.tmux_version().clone();
    ///
    /// // A fresh server has answered no prompts.
    /// if version.meets(&libtmux::since::PROMPT_HISTORY) {
    ///     assert!(server.prompt_history(PromptKind::Command).await?.is_empty());
    /// }
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn prompt_history(&self, kind: PromptKind) -> Result<Vec<TmuxText>, Error> {
        self.require("prompt history", PROMPT_HISTORY_SINCE).await?;

        let result = self
            .cmd(
                Command::new("show-prompt-history")
                    .arg("-T")
                    .arg(kind.name()),
            )
            .await?;
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
    pub async fn clear_prompt_history(&self) -> Result<(), Error> {
        self.require("prompt history", PROMPT_HISTORY_SINCE).await?;

        listing::mutate(
            &self.core,
            "clear-prompt-history",
            Command::new("clear-prompt-history"),
        )
        .await
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
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let version = guard.server().capabilities().await?.tmux_version().clone();
    ///
    /// // Whoever started the server owns it and may act.
    /// if version.meets(&libtmux::since::SERVER_ACCESS) {
    ///     let rules = guard.server().access_rules().await?;
    ///     assert_eq!(rules.len(), 1);
    ///     assert_eq!(rules[0].mode(), libtmux::AccessMode::Write);
    /// }
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn access_rules(&self) -> Result<Vec<AccessRule>, Error> {
        self.require("the server access list", SERVER_ACCESS_SINCE)
            .await?;

        let result = self.cmd(Command::new("server-access").arg("-l")).await?;
        if !result.success() {
            return Err(Error::from_refused_result("server-access", &result, None));
        }

        // tmux writes `name (R)` or `name (W)`, one per line. Split from the
        // right because the flag is fixed width and a name is not.
        Ok(result
            .stdout_lossy()
            .lines()
            .filter_map(|line| {
                let (user, flag) = line.rsplit_once(' ')?;
                let mode = match flag {
                    "(R)" => AccessMode::ReadOnly,
                    "(W)" => AccessMode::Write,
                    _ => return None,
                };
                Some(AccessRule {
                    user: user.to_owned(),
                    mode,
                })
            })
            .collect())
    }

    /// Let a user attach to this server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] below tmux 3.3, and an error
    /// when tmux refuses -- which it does for the user who owns the server.
    pub async fn grant_access(&self, user: &str, mode: AccessMode) -> Result<(), Error> {
        self.require("the server access list", SERVER_ACCESS_SINCE)
            .await?;

        listing::mutate(
            &self.core,
            "server-access",
            Command::new("server-access")
                .arg("-a")
                .arg(match mode {
                    AccessMode::ReadOnly => "-r",
                    AccessMode::Write => "-w",
                })
                .arg(OsString::from(user)),
        )
        .await
    }

    /// Stop a user attaching to this server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] below tmux 3.3, and an error
    /// when tmux refuses -- which it does for the user who owns the server.
    pub async fn revoke_access(&self, user: &str) -> Result<(), Error> {
        self.require("the server access list", SERVER_ACCESS_SINCE)
            .await?;

        listing::mutate(
            &self.core,
            "server-access",
            Command::new("server-access")
                .arg("-d")
                .arg(OsString::from(user)),
        )
        .await
    }

    /// Load a tmux configuration file into this server.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot read the file or a command in it
    /// fails.
    pub async fn source_file(&self, path: impl Into<PathBuf>) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "source-file",
            Command::new("source-file").arg(path.into().into_os_string()),
        )
        .await
    }

    /// Create a session, run an operation with it, then kill it.
    ///
    /// Once this future is polled, the scope owns creation and cleanup.
    /// Cancellation or unwinding can let an in-flight creation finish, but a
    /// session whose creation yields a handle is killed while the Tokio
    /// runtime remains active. Ordinary handle `Drop` remains non-destructive.
    ///
    /// Setup and teardown failures convert into the operation's own error
    /// type, so a caller writes one `?` rather than unwrapping twice. When
    /// both the operation and cleanup fail, the cleanup error is returned as
    /// [`Error::AfterEffect`], because tmux had already accepted the scope's
    /// creation; the operation error is discarded. When the operation fails
    /// and cleanup succeeds, its generic error is returned unchanged: the
    /// scope cannot certify replay safety for arbitrary callback work.
    /// A canceled caller cannot receive a cleanup error, so tracing is its
    /// only report.
    ///
    /// # Errors
    ///
    /// Returns the operation's error, or a converted [`Error`] when the
    /// session could not be created or could not be killed after creation.
    pub async fn with_session<T, E>(
        &self,
        options: impl Into<NewSessionOptions>,
        operation: impl AsyncFnOnce(&Session) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<Error>,
    {
        let server = self.clone();
        let options = options.into();
        scoped::run(
            "with-session",
            async move { server.new_session(options).await },
            Session::kill,
            operation,
        )
        .await
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
    pub async fn run_shell(&self, command: impl Into<OsString>) -> Result<Vec<TmuxText>, Error> {
        self.refuse_if_defective(
            "run-shell output",
            ReleaseVersion::new(3, 3, ReleaseSuffix::FINAL),
            ReleaseVersion::new(3, 5, ReleaseSuffix::FINAL),
        )
        .await?;

        let result = self
            .cmd(Command::new("run-shell").sensitive_arg(command.into()))
            .await?;
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
    pub async fn spawn_shell(&self, command: impl Into<OsString>) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "run-shell",
            Command::new("run-shell")
                .arg("-b")
                .sensitive_arg(command.into()),
        )
        .await
    }

    /// Signal a `wait-for` channel, releasing anything waiting on it.
    ///
    /// [`Server::wait_for_channel`] is the other half, and either order works:
    /// tmux keeps a signal nobody is waiting on, so a command that finishes
    /// before its watcher starts does not lose the race. The latch releases
    /// one wait, and one signal releases every waiter already there.
    /// Signalling the same channel twice before a wait clears the latch, so
    /// this operation is not idempotent.
    ///
    /// Signalling is not scoped to a pane or a session. The channel is a name
    /// on the server, so anything that can reach the socket can signal it,
    /// which is what makes it useful for telling an orchestrator that a
    /// command inside a pane is done.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the channel name.
    pub async fn signal_channel(&self, channel: &str) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "wait-for",
            Command::new("wait-for")
                .arg("-S")
                .arg(OsString::from(channel)),
        )
        .await
    }

    /// Lock a `wait-for` channel, blocking later lock attempts on it.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the channel name.
    pub async fn lock_channel(&self, channel: &str) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "wait-for",
            Command::new("wait-for")
                .arg("-L")
                .arg(OsString::from(channel)),
        )
        .await
    }

    /// Unlock a `wait-for` channel.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the channel name.
    pub async fn unlock_channel(&self, channel: &str) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "wait-for",
            Command::new("wait-for")
                .arg("-U")
                .arg(OsString::from(channel)),
        )
        .await
    }

    /// Wait for a `wait-for` channel to be signalled.
    ///
    /// The blocking half of [`Server::signal_channel`]. Nothing polls: tmux
    /// releases the wait when the channel is signalled, so a caller costs one
    /// idle client rather than a loop.
    ///
    /// This waits for something to *say* it happened. It does not watch a
    /// pane, so what signals the channel is the caller's to arrange -- a
    /// command ending with `tmux wait-for -S <channel>` is the usual shape.
    ///
    /// The channel latches. Signalling one nothing is waiting on is kept, and
    /// the next wait returns at once; the latch is one-shot, so a second wait
    /// blocks again. One signal releases every waiter present at the time. So
    /// signalling before the wait starts is safe, which is what makes this
    /// usable for a command that may finish first.
    ///
    /// That holds across the supported range. `cmd-wait-for.c` is identical
    /// between 3.5a and 3.7c, and the only changes since 3.2a are an argument
    /// table gaining a field, an accessor replacing a direct index, and a
    /// local being renamed -- none of them near the flag the latch is kept in.
    /// Measured directly on 3.5a and 3.7c.
    ///
    /// `within` is capped at [`Server::default_timeout`], because a dispatch
    /// is bounded and this is one: ask for longer by building the server with
    /// a longer timeout.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the channel name or cannot be
    /// reached. Running out of time is [`ChannelWait::TimedOut`] rather than
    /// an error, so "nothing signalled it" stays distinct from "the command
    /// did not get through" -- the caller retries only one of those.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::ChannelWait;
    /// use std::time::Duration;
    ///
    /// # let guard = libtmux::test::TestServer::builder().start().await?;
    /// # let server = guard.server();
    /// // Signalling first is safe: the channel keeps it.
    /// server.signal_channel("ready").await?;
    /// let outcome = server.wait_for_channel("ready", Duration::from_secs(5)).await?;
    /// assert_eq!(outcome, ChannelWait::Signalled);
    ///
    /// // The latch is spent, so a second wait runs out of time instead.
    /// let again = server.wait_for_channel("ready", Duration::from_millis(200)).await?;
    /// assert_eq!(again, ChannelWait::TimedOut);
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn wait_for_channel(
        &self,
        channel: &str,
        within: Duration,
    ) -> Result<ChannelWait, Error> {
        let budget = within.min(self.default_timeout());
        let waited = tokio::time::timeout(
            budget,
            listing::mutate(
                &self.core,
                "wait-for",
                Command::new("wait-for").arg(OsString::from(channel)),
            ),
        )
        .await;

        match waited {
            Ok(Ok(())) => Ok(ChannelWait::Signalled),
            // The dispatch reaching its own bound first is the same event, so
            // it is reported the same way rather than as two outcomes a
            // caller would have to unify.
            Ok(Err(error)) if error.kind() == crate::ErrorKind::Timeout => {
                Ok(ChannelWait::TimedOut)
            }
            Ok(Err(error)) => Err(error),
            // Dropping the dispatch kills the tmux client that was waiting.
            // The server is unaffected and the channel stays usable, measured
            // by killing a waiter outright and signalling it afterwards.
            Err(_elapsed) => Ok(ChannelWait::TimedOut),
        }
    }

    /// Start a tmux server without creating a session.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be run.
    pub async fn start(&self) -> Result<(), Error> {
        listing::mutate(&self.core, "start-server", Command::new("start-server")).await
    }

    /// Show a popup over a client, running a command inside it.
    ///
    /// This needs a client with a terminal, so it fails on a server nothing is
    /// attached to.
    ///
    /// Unlike [`Self::command_prompt`] and [`Self::display_menu`], this does
    /// not wait for a person -- but it does wait for the command. The popup is
    /// opened with `-E`, so it closes when what runs inside it exits, and this
    /// returns then. A command that does not end holds the call until the
    /// dispatch timeout does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the client belongs to another
    /// server, or an error when no suitable client exists, when tmux refuses
    /// the command, and when the dispatch timeout expires before it exits.
    pub async fn display_popup(
        &self,
        client: Option<&Client>,
        command: impl Into<OsString>,
    ) -> Result<(), Error> {
        let mut popup = Command::new("display-popup").arg("-E");
        if let Some(client) = client {
            self.core
                .require_same_server(client.server_identity(), "display-popup")?;
            popup = popup
                .arg("-t")
                .arg(client.name().to_string_lossy().into_owned());
        }

        listing::mutate(&self.core, "display-popup", popup.arg(command.into())).await
    }

    /// Show a menu over a client.
    ///
    /// Items are `(label, key, command)` triples in the order tmux should show
    /// them. This needs a client with a terminal.
    ///
    /// Like [`Self::command_prompt`], this waits for the person: tmux holds the
    /// invocation until an item is chosen or the menu is dismissed, and the
    /// dispatch timeout is what ends the wait when nobody does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the client belongs to another
    /// server, or an error when no suitable client exists, when tmux refuses
    /// an item, and when the dispatch timeout expires before anyone chooses.
    pub async fn display_menu(
        &self,
        client: Option<&Client>,
        title: &str,
        items: impl IntoIterator<Item = (String, String, String)>,
    ) -> Result<(), Error> {
        let mut menu = Command::new("display-menu")
            .arg("-T")
            .arg(OsString::from(title));
        if let Some(client) = client {
            self.core
                .require_same_server(client.server_identity(), "display-menu")?;
            menu = menu
                .arg("-t")
                .arg(client.name().to_string_lossy().into_owned());
        }
        for (label, key, command) in items {
            menu = menu
                .arg(OsString::from(label))
                .arg(OsString::from(key))
                .arg(OsString::from(command));
        }

        listing::mutate(&self.core, "display-menu", menu).await
    }

    /// Open a command prompt on a client.
    ///
    /// The prompt runs `command` once the user answers, with `%%` replaced by
    /// what they typed.
    ///
    /// This does not return when the prompt opens. tmux holds the invocation
    /// until somebody answers or dismisses it, so a caller is waiting on a
    /// person -- and on a server nobody is watching, on nobody. The dispatch
    /// timeout is what ends that wait: with the default it fails after thirty
    /// seconds having opened a prompt that is still there. Give the call a
    /// server whose `default_timeout` suits a human, or drive it from a task
    /// that may take that long.
    ///
    /// Passing a client is not what decides this. Both forms wait; naming one
    /// only decides which terminal the prompt appears on.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the client belongs to another
    /// server, or an error when no suitable client exists, when tmux refuses,
    /// and when the dispatch timeout expires before the prompt is answered.
    pub async fn command_prompt(
        &self,
        client: Option<&Client>,
        prompt: Option<&str>,
        command: impl Into<OsString>,
    ) -> Result<(), Error> {
        let mut request = Command::new("command-prompt");
        if let Some(client) = client {
            self.core
                .require_same_server(client.server_identity(), "command-prompt")?;
            request = request
                .arg("-t")
                .arg(client.name().to_string_lossy().into_owned());
        }
        if let Some(prompt) = prompt {
            request = request.arg("-p").arg(OsString::from(prompt));
        }

        listing::mutate(&self.core, "command-prompt", request.arg(command.into())).await
    }

    /// Open one of tmux's interactive choosers on a client.
    ///
    /// A chooser opens *in a pane*, which is why this needs no client and
    /// succeeds on a server nothing is attached to: the pane's `pane_in_mode`
    /// becomes `1` and its `pane_mode` becomes `tree-mode`, client or not.
    ///
    /// That is the difference from [`Self::display_popup`],
    /// [`Self::display_menu`], [`Self::command_prompt`] and
    /// [`Self::display_panes`], which draw *on a client* and report "no current
    /// client" without one. Passing `client` here only says which client's
    /// current pane to open in.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the client belongs to another
    /// server, or an error when tmux refuses the command. A missing client is
    /// not one: this does not need one.
    pub async fn choose(&self, chooser: Chooser, client: Option<&Client>) -> Result<(), Error> {
        let name = chooser.command();
        let mut request = Command::new(name);
        if let Some(client) = client {
            self.core
                .require_same_server(client.server_identity(), name)?;
            request = request
                .arg("-t")
                .arg(client.name().to_string_lossy().into_owned());
        }

        listing::mutate(&self.core, name, request).await
    }

    /// Open tmux's window finder for a search string.
    ///
    /// This is separate from [`Server::choose`] because it needs something to
    /// search for, where the other choosers list what already exists. Like them
    /// it opens in a pane rather than on a client, so it needs no client and
    /// leaves the pane in `tree-mode` on a server nothing is attached to.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the client belongs to another
    /// server, or an error when tmux refuses the command. A missing client is
    /// not one: this does not need one.
    pub async fn find_window(&self, client: Option<&Client>, search: &str) -> Result<(), Error> {
        let mut request = Command::new("find-window");
        if let Some(client) = client {
            self.core
                .require_same_server(client.server_identity(), "find-window")?;
            request = request
                .arg("-t")
                .arg(client.name().to_string_lossy().into_owned());
        }

        listing::mutate(
            &self.core,
            "find-window",
            request.arg(OsString::from(search)),
        )
        .await
    }

    /// Briefly show each pane's number on a client.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the client belongs to another
    /// server, or an error when no suitable client exists or tmux refuses.
    pub async fn display_panes(&self, client: Option<&Client>) -> Result<(), Error> {
        let mut request = Command::new("display-panes");
        if let Some(client) = client {
            self.core
                .require_same_server(client.server_identity(), "display-panes")?;
            request = request
                .arg("-t")
                .arg(client.name().to_string_lossy().into_owned());
        }

        listing::mutate(&self.core, "display-panes", request).await
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
    /// that is not a pid and a start time.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let server = guard.server();
    /// server.new_session("work").await?;
    ///
    /// let generation = server.generation().await?;
    /// // Unchanged while the daemon is the same one.
    /// server.require_generation(generation).await?;
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn generation(&self) -> Result<ServerGeneration, Error> {
        // Both are server-scoped and available in every list profile since
        // 3.2a. The start time is what defeats pid reuse: a replacement daemon
        // can be handed the pid of the one it replaced.
        let answer = self.format(None, "#{pid} #{start_time}").await?;
        let text = answer.to_string_lossy();
        let mut parts = text.split_whitespace();
        let parsed = parts
            .next()
            .and_then(|pid| pid.parse::<u32>().ok())
            .zip(parts.next().and_then(|start| start.parse::<i64>().ok()));

        let Some((pid, start_time)) = parsed else {
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
    pub async fn require_generation(&self, expected: ServerGeneration) -> Result<(), Error> {
        let found = self.generation().await?;
        if found == expected {
            return Ok(());
        }

        Err(Error::ServerGenerationChanged { expected, found })
    }

    /// Report whether a tmux daemon is answering at this endpoint.
    ///
    /// Every failure becomes `false`, including a missing executable. Use
    /// [`Server::check_alive`] when the difference matters.
    pub async fn is_alive(&self) -> bool {
        self.check_alive().await.is_ok()
    }

    /// Require a tmux daemon to be answering at this endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerGone`] when no daemon is answering, or another
    /// dispatch error when tmux cannot be asked.
    pub async fn check_alive(&self) -> Result<(), Error> {
        let result = self.cmd(Command::new("list-sessions")).await?;
        if result.success() {
            return Ok(());
        }

        Err(Error::from_refused_result("list-sessions", &result, None))
    }

    #[cfg(test)]
    pub(crate) fn from_executor_for_test(executor: Arc<dyn Executor>) -> Self {
        Self {
            core: Arc::new(Core::from_executor_for_test(executor)),
        }
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
mod tests {

    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use tokio::sync::{Notify, watch};

    use super::{NewSessionOptions, Server};
    use crate::command::{CommandRequest, CommandResult, ProcessStatus};
    use crate::formats::{DecoderKind, FormatDescriptor, FormatPlan, ListProfile};
    use crate::internal::executor::{DispatchFuture, Executor, ShutdownFuture};
    use crate::{Error, ErrorKind, TmuxVersion};

    struct RefusingExecutor {
        calls: AtomicUsize,
    }

    #[derive(Clone, Copy)]
    enum SessionFollowup {
        DispatchError,
        EmptyListing,
    }

    struct ComposedSessionExecutor {
        calls: AtomicUsize,
        sessions_stdout: Vec<u8>,
        followup: SessionFollowup,
    }

    impl Executor for ComposedSessionExecutor {
        fn execute(&self, request: CommandRequest) -> DispatchFuture {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let sessions_stdout = self.sessions_stdout.clone();
            let followup = self.followup;
            DispatchFuture::new(async move {
                if call == 3 && matches!(followup, SessionFollowup::DispatchError) {
                    return Err(Error::Overloaded {
                        request_id: request.request_id().get(),
                        command: request.summary().clone(),
                        in_flight: 1,
                    });
                }

                let stdout = match call {
                    0 => b"tmux 3.7b\n".to_vec(),
                    1 => sessions_stdout,
                    2 | 3 => Vec::new(),
                    _ => panic!("one probe, one listing, one mutation, and one refresh"),
                };
                Ok(CommandResult::new(
                    request.request_id(),
                    request.summary().clone(),
                    ProcessStatus::from_exit_status(ExitStatus::from_raw(0)),
                    stdout,
                    Vec::new(),
                ))
            })
        }

        fn shutdown(&self) -> ShutdownFuture {
            ShutdownFuture::new(async { Ok(()) })
        }
    }

    fn default_format_value(descriptor: &FormatDescriptor) -> &'static [u8] {
        match descriptor.name() {
            "session_id" => b"$1",
            "window_id" => b"@1",
            "pane_id" => b"%1",
            "client_name" => b"client",
            _ => match descriptor.decoder() {
                DecoderKind::Ascii => b"ascii",
                DecoderKind::Text => b"text",
                DecoderKind::Bool
                | DecoderKind::U8
                | DecoderKind::U32
                | DecoderKind::U64
                | DecoderKind::I32
                | DecoderKind::Timestamp
                | DecoderKind::PaneProgress => b"0",
                DecoderKind::SessionId => b"$1",
                DecoderKind::WindowId => b"@1",
                DecoderKind::PaneId => b"%1",
                DecoderKind::PaneProgressState => b"normal",
            },
        }
    }

    fn session_listing_stdout() -> Vec<u8> {
        let version = TmuxVersion::parse_output(b"tmux 3.7b\n").expect("fixture version");
        let plan = FormatPlan::for_profile(ListProfile::Sessions, &version);
        let mut stdout = Vec::new();
        for descriptor in plan.descriptors_for_test() {
            for byte in default_format_value(descriptor) {
                if matches!(*byte, b'\\' | b'%') {
                    stdout.push(b'\\');
                }
                stdout.push(*byte);
            }
            stdout.push(b'%');
        }
        stdout.push(b'\n');
        stdout
    }

    impl Executor for RefusingExecutor {
        fn execute(&self, request: CommandRequest) -> DispatchFuture {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            DispatchFuture::new(async move {
                let (status, stdout, stderr) = if call == 0 {
                    (0, b"tmux 3.7b\n".to_vec(), Vec::new())
                } else {
                    assert_eq!(call, 1, "one probe and one run-shell dispatch");
                    assert_eq!(request.summary().sensitive_argument_count(), 1);
                    (1 << 8, Vec::new(), b"sentinel-run-shell-output\n".to_vec())
                };
                Ok(CommandResult::new(
                    request.request_id(),
                    request.summary().clone(),
                    ProcessStatus::from_exit_status(ExitStatus::from_raw(status)),
                    stdout,
                    stderr,
                ))
            })
        }

        fn shutdown(&self) -> ShutdownFuture {
            ShutdownFuture::new(async { Ok(()) })
        }
    }

    struct BlockingShutdownExecutor {
        closed: AtomicBool,
        shutdown_started: Notify,
        release: watch::Receiver<bool>,
    }

    impl Executor for BlockingShutdownExecutor {
        fn execute(&self, request: CommandRequest) -> DispatchFuture {
            let closed = self.closed.load(Ordering::SeqCst);
            DispatchFuture::new(async move {
                assert!(
                    closed,
                    "test dispatch occurs only after shutdown closes admission"
                );
                Err(Error::executor_shutdown(
                    request.request_id().get(),
                    request.summary().clone(),
                ))
            })
        }

        fn shutdown(&self) -> ShutdownFuture {
            self.closed.store(true, Ordering::SeqCst);
            self.shutdown_started.notify_one();
            let mut release = self.release.clone();
            ShutdownFuture::new(async move {
                while !*release.borrow() {
                    release
                        .changed()
                        .await
                        .expect("test release sender remains alive");
                }
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn aborting_one_server_shutdown_keeps_all_clones_closed_until_later_completion() {
        let (release_sender, release_receiver) = watch::channel(false);
        let executor = Arc::new(BlockingShutdownExecutor {
            closed: AtomicBool::new(false),
            shutdown_started: Notify::new(),
            release: release_receiver,
        });
        let server = Server::from_executor_for_test(executor.clone());
        let started = executor.shutdown_started.notified();
        let shutdown_server = server.clone();
        let shutdown = tokio::spawn(async move { shutdown_server.shutdown().await });
        started.await;
        shutdown.abort();
        assert!(
            shutdown
                .await
                .expect_err("shutdown task was aborted")
                .is_cancelled()
        );

        assert!(matches!(
            server.cmd(crate::Command::new("display-message")).await,
            Err(Error::ExecutorShutdown { .. })
        ));
        release_sender
            .send(true)
            .expect("test release receiver remains alive");
        server
            .clone()
            .shutdown()
            .await
            .expect("later clone completes shutdown");
    }

    #[tokio::test]
    async fn run_shell_failure_withholds_sensitive_output() {
        let server = Server::from_executor_for_test(Arc::new(RefusingExecutor {
            calls: AtomicUsize::new(0),
        }));

        let error = server
            .run_shell("sentinel-run-shell-command")
            .await
            .expect_err("the command is refused");
        let diagnostic = format!("{error:?} {error}");
        for secret in ["sentinel-run-shell-command", "sentinel-run-shell-output"] {
            assert!(!diagnostic.contains(secret), "{diagnostic}");
        }
    }

    #[tokio::test]
    async fn session_refresh_failure_after_rename_marks_the_completed_effect() {
        let executor = Arc::new(ComposedSessionExecutor {
            calls: AtomicUsize::new(0),
            sessions_stdout: session_listing_stdout(),
            followup: SessionFollowup::DispatchError,
        });
        let server = Server::from_executor_for_test(executor.clone());
        let mut session = server
            .sessions()
            .await
            .expect("fixture session listing")
            .pop()
            .expect("one fixture session");

        let error = session
            .rename("renamed")
            .await
            .expect_err("refresh dispatch fails after rename succeeds");

        assert_eq!(executor.calls.load(Ordering::SeqCst), 4);
        assert_eq!(error.kind(), ErrorKind::PartialEffect);
        assert!(
            matches!(
                error,
                Error::AfterEffect { operation: "rename-session", source }
                    if source.kind() == ErrorKind::Refused && source.is_transient()
            ),
            "the refresh error remains available as the source",
        );
    }

    #[tokio::test]
    async fn successful_session_step_requires_an_active_window_postcondition() {
        let executor = Arc::new(ComposedSessionExecutor {
            calls: AtomicUsize::new(0),
            sessions_stdout: session_listing_stdout(),
            followup: SessionFollowup::EmptyListing,
        });
        let server = Server::from_executor_for_test(executor.clone());
        let session = server
            .sessions()
            .await
            .expect("fixture session listing")
            .pop()
            .expect("one fixture session");

        let error = session
            .next_window()
            .await
            .expect_err("an accepted step must leave an active window");

        assert_eq!(executor.calls.load(Ordering::SeqCst), 4);
        assert!(matches!(
            error,
            Error::AfterEffect { operation: "next-window", source }
                if matches!(*source, Error::ObjectGone { .. })
        ));
    }

    #[test]
    fn new_session_options_redact_the_shell_command() {
        let secret = "sentinel-session-command";
        let options = NewSessionOptions::new("work").command(secret);
        assert!(!format!("{options:?}").contains(secret));

        let summary = options.into_command("#{session_id}").summary();
        assert_eq!(summary.sensitive_argument_count(), 1);
        assert!(!summary.to_string().contains(secret));
    }
}

/// Options for creating a session.
///
/// A bare name is accepted wherever this is, so the common case stays short:
/// `server.new_session("work")`.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::NewSessionOptions;
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let server = guard.server();
///
/// let session = server
///     .new_session(NewSessionOptions::new("work").window_name("editor"))
///     .await?;
///
/// let window = session.active_window().await?.expect("a session has a window");
/// assert_eq!(window.name().to_string_lossy(), "editor");
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[must_use = "options describe a session but do not create one"]
#[derive(Clone)]
pub struct NewSessionOptions {
    name: OsString,
    start_directory: Option<PathBuf>,
    window_name: Option<OsString>,
    command: Option<OsString>,
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
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl NewSessionOptions {
    /// Describe a session with the given name.
    pub fn new(name: impl Into<OsString>) -> Self {
        Self {
            name: name.into(),
            start_directory: None,
            window_name: None,
            command: None,
            width: None,
            height: None,
        }
    }

    /// Set the working directory for the session's first window.
    pub fn start_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.start_directory = Some(directory.into());
        self
    }

    /// Name the session's first window.
    pub fn window_name(mut self, name: impl Into<OsString>) -> Self {
        self.window_name = Some(name.into());
        self
    }

    /// Run a command instead of the default shell.
    pub fn command(mut self, command: impl Into<OsString>) -> Self {
        self.command = Some(command.into());
        self
    }

    /// Set the initial size, which a detached session would otherwise default.
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
            .arg(self.name);
        if let Some(directory) = self.start_directory {
            command = command.arg("-c").arg(directory.into_os_string());
        }
        if let Some(name) = self.window_name {
            command = command.arg("-n").arg(name);
        }
        if let (Some(width), Some(height)) = (self.width, self.height) {
            command = command
                .arg("-x")
                .arg(width.to_string())
                .arg("-y")
                .arg(height.to_string());
        }
        if let Some(shell_command) = self.command {
            command = command.sensitive_arg(shell_command);
        }
        command
    }
}

impl<T: Into<OsString>> From<T> for NewSessionOptions {
    fn from(name: T) -> Self {
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
/// # async fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
/// use libtmux::Chooser;
///
/// // A chooser draws in an attached client's terminal, so this needs one; with
/// // no client tmux has nowhere to put it.
/// let client = server.clients().await?.into_iter().next();
/// server.choose(Chooser::Tree, client.as_ref()).await?;
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
