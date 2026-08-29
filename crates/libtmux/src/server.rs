use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::client::Client;
use crate::formats::TmuxText;
use crate::internal::core::{BuildContext, Core, CoreConfiguration, SocketSelection};
use crate::internal::environment;
#[cfg(test)]
use crate::internal::executor::Executor;
use crate::internal::listing::{self, Pushdown as _};
use crate::internal::options;
use crate::pane::Pane;
#[cfg(feature = "query")]
use crate::query::{Filterable, ManyRelation};
use crate::session::Session;
#[cfg(feature = "query")]
use crate::snapshot::{SessionFields, WindowFields};
use crate::window::Window;
use crate::{
    Command, CommandChain, CommandResult, DispatchLimits, EngineCapabilities, EnvironmentEntry,
    Error, IndexedHooks, ObjectKind, OptionValue, OutputLimits, PaneId, ReleaseSuffix,
    ReleaseVersion, ReplaceMode, ServerConfigurationErrorKind, ServerGeneration, ServerIdentity,
    SessionId, SparseValues, WindowId,
};

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

impl Server {
    /// Build a handle onto a connection an object already holds.
    ///
    /// Not feature-gated: a `Client` resolving what it is attached to needs a
    /// server to look ids up through, and that has nothing to do with control
    /// mode.
    pub(crate) const fn from_core(core: Arc<Core>) -> Self {
        Self { core }
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

    /// Close the shared client executor and wait for active child cleanup.
    ///
    /// Shutdown is idempotent and affects every clone of this server.
    /// It never stops the tmux daemon itself. Await it before dropping the
    /// Tokio runtime when deterministic reaping of client subprocesses matters.
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

    /// List every session on the server, in tmux's own order.
    ///
    /// This is the lenient form: a server that is not running, or any other
    /// failure of the underlying list operation, yields an empty `Vec`. Use
    /// [`Server::sessions`] when the reason matters.
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
    /// // A fixture starts with no sessions. The lenient form reports that as
    /// // an empty listing rather than as the failure it also collapses.
    /// assert!(server.sessions_or_empty().await.is_empty());
    ///
    /// guard.session("work").await?;
    ///
    /// let sessions = server.sessions_or_empty().await;
    /// assert_eq!(sessions.len(), 1);
    /// assert_eq!(sessions[0].name().as_bytes(), b"work");
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn sessions_or_empty(&self) -> Vec<Session> {
        self.sessions().await.unwrap_or_else(|error| {
            listing::trace_discarded("list-sessions", &error);
            Vec::new()
        })
    }

    /// List every window on the server, in tmux's own order.
    ///
    /// A window linked into several sessions appears once per link, so a
    /// window id can repeat. See [`Window`] for what that means for equality.
    ///
    /// This is the lenient form; use [`Server::windows`] when the reason
    /// for an empty result matters.
    pub async fn windows_or_empty(&self) -> Vec<Window> {
        self.windows().await.unwrap_or_else(|error| {
            listing::trace_discarded("list-windows", &error);
            Vec::new()
        })
    }

    /// List every window on the server, preserving any failure.
    ///
    /// # Errors
    ///
    /// Returns an error when the list command cannot run, or when its output
    /// cannot be decoded into snapshots.
    pub async fn windows(&self) -> Result<Vec<Window>, Error> {
        let projections = listing::windows(&self.core, listing::Scope::Server, None).await?;

        Ok(projections
            .into_iter()
            .map(|projection| Window::new(Arc::clone(&self.core), projection))
            .collect())
    }

    /// List every pane on the server, in tmux's own order.
    ///
    /// Panes under a linked window appear once per link, matching
    /// [`Server::windows_or_empty`].
    ///
    /// This is the lenient form; use [`Server::panes`] when the reason for
    /// an empty result matters.
    pub async fn panes_or_empty(&self) -> Vec<Pane> {
        self.panes().await.unwrap_or_else(|error| {
            listing::trace_discarded("list-panes", &error);
            Vec::new()
        })
    }

    /// List every pane on the server, preserving any failure.
    ///
    /// # Errors
    ///
    /// Returns an error when the list command cannot run, or when its output
    /// cannot be decoded into snapshots.
    pub async fn panes(&self) -> Result<Vec<Pane>, Error> {
        let projections = listing::panes(&self.core, listing::Scope::Server, None).await?;

        Ok(projections
            .into_iter()
            .map(|projection| Pane::new(Arc::clone(&self.core), projection))
            .collect())
    }

    /// List every client attached to the server, in tmux's own order.
    ///
    /// This is the lenient form; use [`Server::clients`] when the reason for
    /// an empty result matters.
    pub async fn clients_or_empty(&self) -> Vec<Client> {
        self.clients().await.unwrap_or_else(|error| {
            listing::trace_discarded("list-clients", &error);
            Vec::new()
        })
    }

    /// List every client attached to the server, preserving any failure.
    ///
    /// # Errors
    ///
    /// Returns an error when the list command cannot run, or when its output
    /// cannot be decoded into snapshots.
    pub async fn clients(&self) -> Result<Vec<Client>, Error> {
        let infos = listing::clients(&self.core, None).await?;

        Ok(infos
            .into_iter()
            .map(|info| Client::new(Arc::clone(&self.core), info))
            .collect())
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

    /// Read one server option's exact stored value.
    ///
    /// Returns `None` when the option is known but holds no value. tmux
    /// prints nothing in that case, so an option set to the empty string
    /// cannot be told apart from an unset one.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn get_option(&self, name: &str) -> Result<Option<TmuxText>, Error> {
        options::get(&self.core, options::Scope::Server, name).await
    }

    /// List the server option names.
    ///
    /// Values are not included: tmux renders them for display with three
    /// different quoting styles, so re-parsing them would be guesswork. Read
    /// each value with [`Server::get_option`], which returns exact bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the listing.
    pub async fn option_names(&self) -> Result<Vec<String>, Error> {
        options::names(&self.core, options::Scope::Server).await
    }

    /// Read every option set at this server, decoded by its declared kind.
    ///
    /// Costs one tmux command per option, because each value is read as the
    /// bytes tmux stored rather than the form it lists them in. Use
    /// [`Self::option_names`] when only the names are wanted, and
    /// [`Self::typed_option`] for one value.
    ///
    /// An array option keeps the indexed name tmux lists it under, so
    /// `command-alias[0]` and `command-alias[1]` are separate entries.
    ///
    /// Reports what is set *at this scope*, not what the object resolves to.
    /// A session that has set nothing of its own answers empty even though
    /// every option still has an effective value it inherits.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the listing.
    /// An empty map means nothing is set, never that the listing failed.
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
    /// server.set_option("buffer-limit", "42").await?;
    /// let options = server.options().await?;
    /// assert!(options.contains_key("buffer-limit"));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn options(&self) -> Result<BTreeMap<String, OptionValue>, Error> {
        options::typed_all(&self.core, options::Scope::Server).await
    }

    /// Set one server option.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    pub async fn set_option(&self, name: &str, value: impl Into<OsString>) -> Result<(), Error> {
        options::set(&self.core, options::Scope::Server, name, value, false).await
    }

    /// Remove one server option.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
    pub async fn unset_option(&self, name: &str) -> Result<(), Error> {
        options::unset(&self.core, options::Scope::Server, name).await
    }

    /// Read one global session option.
    ///
    /// Sessions inherit from this table, so it is where a default belongs.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn get_global_option(&self, name: &str) -> Result<Option<TmuxText>, Error> {
        options::get(&self.core, options::Scope::GlobalSession, name).await
    }

    /// Set one global session option.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    pub async fn set_global_option(
        &self,
        name: &str,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        options::set(
            &self.core,
            options::Scope::GlobalSession,
            name,
            value,
            false,
        )
        .await
    }

    /// Set a variable in the server's own environment.
    ///
    /// tmux keeps this and each session's environment in separate stores, and
    /// merges them only when it starts a process. So a name set here is
    /// reported as an unknown variable by [`Session::environment`] -- reading
    /// a session does not fall back to the server -- while a pane started
    /// afterwards is handed it all the same.
    ///
    /// Where both hold a name, the session's value is the one the process
    /// gets. [`Self::hide_environment`] removes the name from the merge
    /// entirely.
    ///
    /// Panes already running keep the environment they were started with.
    ///
    /// The value is marked sensitive, since an environment carries tokens.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::EnvironmentEntry;
    ///
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let server = guard.server();
    ///
    /// server.set_environment("EDITOR", "hx").await?;
    /// assert!(matches!(
    ///     server.environment("EDITOR").await?,
    ///     Some(EnvironmentEntry::Set(value)) if value.as_bytes() == b"hx",
    /// ));
    ///
    /// // Separate stores: the session has no entry of its own, and reading it
    /// // does not fall back to the server. The value still reaches a process
    /// // the session starts.
    /// let session = server.new_session("separate").await?;
    /// assert_eq!(session.environment("EDITOR").await?, None);
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn set_environment(
        &self,
        name: &str,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        environment::set(&self.core, environment::Scope::Global, name, value.into()).await
    }

    /// Read one variable from the server's environment.
    ///
    /// tmux keeps two different things under a name: a value, and a mark
    /// saying a process started from it must not inherit the name at all.
    /// [`EnvironmentEntry`] keeps them apart, because collapsing both to
    /// absence would hide the second, which a caller sets deliberately with
    /// [`Self::hide_environment`].
    ///
    /// `None` means tmux holds nothing under the name.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached.
    pub async fn environment(&self, name: &str) -> Result<Option<EnvironmentEntry>, Error> {
        environment::get(&self.core, environment::Scope::Global, name).await
    }

    /// Read the server's whole environment.
    ///
    /// Costs one tmux command per variable, for the reason given on
    /// [`Session::environment_all`]: a value containing a newline occupies
    /// more than one line of the listing, and a continuation line holding an
    /// `=` cannot be told from the next variable.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the listing.
    /// An empty map means the server holds nothing, never that the listing
    /// failed.
    pub async fn environment_all(&self) -> Result<BTreeMap<String, EnvironmentEntry>, Error> {
        environment::all(&self.core, environment::Scope::Global).await
    }

    /// Hide a variable from processes tmux starts.
    ///
    /// Different from [`Self::unset_environment`], which deletes the server's
    /// own entry and lets whatever tmux was started with show through. This
    /// keeps an entry and marks it, so a process started afterwards is handed
    /// an environment with the name *absent* even though the tmux server was
    /// started with one. It is what [`EnvironmentEntry::Removed`] reports.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
    pub async fn hide_environment(&self, name: &str) -> Result<(), Error> {
        environment::hide(&self.core, environment::Scope::Global, name).await
    }

    /// Remove a variable from the server's environment.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
    pub async fn unset_environment(&self, name: &str) -> Result<(), Error> {
        environment::unset(&self.core, environment::Scope::Global, name).await
    }

    /// Read every value an array option holds, by index.
    ///
    /// Some tmux options hold a numbered set rather than one value:
    /// `command-alias` and `terminal-overrides` are the common ones, and every
    /// hook is one too. The indices are sparse and tmux keeps the gaps, so
    /// this reports them rather than a list. An empty result means the option
    /// holds nothing.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
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
    /// // Written far apart on purpose: nothing renumbers, so the gap stays.
    /// server.set_array_option("command-alias", 30, "thirty=display -p 30").await?;
    /// server.set_array_option("command-alias", 35, "five=display -p 35").await?;
    ///
    /// let aliases = server.array_option("command-alias").await?;
    /// assert_eq!(aliases.get(31), None, "the gap is tmux's, and it is kept");
    /// assert_eq!(
    ///     aliases.get(35).map(|value| value.to_string_lossy().into_owned()),
    ///     Some("five=display -p 35".to_owned()),
    /// );
    ///
    /// server.unset_array_option("command-alias", 35).await?;
    /// assert_eq!(server.array_option("command-alias").await?.get(35), None);
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn array_option(&self, name: &str) -> Result<SparseValues<TmuxText>, Error> {
        Ok(SparseValues::from(
            options::indexed(&self.core, options::Scope::GlobalSession, name).await?,
        ))
    }

    /// Write one index of an array option, leaving the others alone.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the option name or the value.
    pub async fn set_array_option(
        &self,
        name: &str,
        index: u32,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        options::set(
            &self.core,
            options::Scope::GlobalSession,
            &format!("{name}[{index}]"),
            value,
            false,
        )
        .await
    }

    /// Extend the value already at one index of an array option.
    ///
    /// Appends to that index's value rather than adding an entry, which is
    /// what tmux's `-a` does here.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the option name or the value.
    pub async fn append_array_option(
        &self,
        name: &str,
        index: u32,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        options::set(
            &self.core,
            options::Scope::GlobalSession,
            &format!("{name}[{index}]"),
            value,
            true,
        )
        .await
    }

    /// Remove one index of an array option, leaving a gap where it was.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the option name.
    pub async fn unset_array_option(&self, name: &str, index: u32) -> Result<(), Error> {
        options::unset(
            &self.core,
            options::Scope::GlobalSession,
            &format!("{name}[{index}]"),
        )
        .await
    }

    /// Read one global window option.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn get_global_window_option(&self, name: &str) -> Result<Option<TmuxText>, Error> {
        options::get(&self.core, options::Scope::GlobalWindow, name).await
    }

    /// Set one global window option.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    pub async fn set_global_window_option(
        &self,
        name: &str,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        options::set(&self.core, options::Scope::GlobalWindow, name, value, false).await
    }

    /// Set one global hook.
    ///
    /// Hooks live in the option tables, so [`Server::get_global_option`] reads
    /// one back under an indexed name such as `after-new-window[0]`.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the hook name or command.
    pub async fn set_hook(&self, name: &str, command: impl Into<OsString>) -> Result<(), Error> {
        options::set_hook(&self.core, options::Scope::GlobalSession, name, command).await
    }

    /// Remove one global hook.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the hook name.
    pub async fn unset_hook(&self, name: &str) -> Result<(), Error> {
        options::unset_hook(&self.core, options::Scope::GlobalSession, name).await
    }

    /// Write a whole hook at once.
    ///
    /// [`ReplaceMode::Replace`] clears the hook first, so only what is
    /// written remains; [`ReplaceMode::Merge`] leaves entries at indices the
    /// write does not name.
    ///
    /// Sent as one tmux invocation rather than one per index. That costs one
    /// process instead of several, but it is not atomic: tmux applies a
    /// shared invocation in order and stops at the first refusal, so a
    /// rejected entry leaves the ones before it written.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or any command.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use std::collections::BTreeMap;
    /// use libtmux::{IndexedHooks, ReplaceMode, TmuxText};
    ///
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let server = guard.server();
    ///
    /// let mut entries = BTreeMap::new();
    /// entries.insert(0, TmuxText::from(b"display-message first".to_vec()));
    /// entries.insert(3, TmuxText::from(b"display-message fourth".to_vec()));
    ///
    /// server
    ///     .set_hooks("alert-bell", &IndexedHooks::from(entries), ReplaceMode::Replace)
    ///     .await?;
    ///
    /// let written = server.hook("alert-bell").await?.expect("the hook is set");
    /// assert_eq!(written.len(), 2);
    /// assert!(written.get(1).is_none(), "the gap is kept");
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn set_hooks(
        &self,
        name: &str,
        hooks: &IndexedHooks,
        replace: ReplaceMode,
    ) -> Result<(), Error> {
        options::set_hooks(
            &self.core,
            options::Scope::GlobalSession,
            name,
            hooks,
            replace,
        )
        .await
    }

    /// Read every hook set at this server.
    ///
    /// Only hooks holding something are reported: tmux lists every hook name
    /// it knows, and the ones holding nothing are absent here rather than
    /// present and empty.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the listing.
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
    /// server.set_hook("alert-bell", "display-message rang").await?;
    /// let hooks = server.hooks().await?;
    /// assert!(hooks.contains_key("alert-bell"));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn hooks(&self) -> Result<BTreeMap<String, IndexedHooks>, Error> {
        options::hooks(&self.core, options::Scope::GlobalSession).await
    }

    /// Read one hook's commands, or `None` when it holds nothing.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the listing.
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
    /// assert!(server.hook("alert-bell").await?.is_none());
    /// server.set_hook("alert-bell", "display-message rang").await?;
    /// assert!(server.hook("alert-bell").await?.is_some());
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn hook(&self, name: &str) -> Result<Option<IndexedHooks>, Error> {
        options::hook(&self.core, options::Scope::GlobalSession, name).await
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
    /// # Errors
    ///
    /// Returns an error when tmux rejects the format or the pane is gone.
    pub async fn format(&self, pane: Option<&Pane>, format: &str) -> Result<TmuxText, Error> {
        let mut command = Command::new("display-message").arg("-p");
        if let Some(pane) = pane {
            command = command.arg("-t").arg(pane.id().to_string());
        }

        let result = self.cmd(command.arg(OsString::from(format))).await?;
        if !result.success() {
            return Err(Error::refused(
                "display-message",
                result.exit_code(),
                result.stderr_lossy().into_owned(),
                None,
            ));
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
            return Err(Error::refused(
                "show-prompt-history",
                result.exit_code(),
                result.stderr_lossy().into_owned(),
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
            return Err(Error::refused(
                "server-access",
                result.exit_code(),
                result.stderr_lossy().into_owned(),
                None,
            ));
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
    /// The session is killed whether the operation succeeded or failed, so a
    /// short-lived task does not leave one behind. A panic still skips
    /// cleanup: `Drop` on these handles is deliberately not destructive.
    ///
    /// Setup and teardown failures convert into the operation's own error
    /// type, so a caller writes one `?` rather than unwrapping twice. When
    /// both the operation and the cleanup fail, the operation's error is
    /// returned, because that is the work the caller was doing; the discarded
    /// cleanup failure is recorded through `tracing` when that feature is on.
    ///
    /// # Errors
    ///
    /// Returns the operation's error, or a converted [`Error`] when the
    /// session could not be created, or could not be killed after the
    /// operation succeeded.
    pub async fn with_session<T, E>(
        &self,
        options: impl Into<NewSessionOptions>,
        operation: impl AsyncFnOnce(&Session) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<Error>,
    {
        let created = self.new_session(options).await?;
        let outcome = operation(&created).await;

        match (outcome, created.kill().await) {
            (outcome, Ok(())) => outcome,
            (Ok(_), Err(error)) => Err(error.into()),
            (Err(outcome), Err(cleanup)) => {
                listing::trace_discarded_cleanup(&cleanup);
                Err(outcome)
            }
        }
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
            return Err(Error::CommandFailed {
                command: "run-shell",
                exit_code: result.exit_code(),
                stderr: result.stderr_lossy().into_owned(),
            });
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
    /// What waits is a tmux command elsewhere: `tmux wait-for <channel>` with
    /// no flag blocks until this releases it, and that form is not offered
    /// here. A dispatch carries the server's `default_timeout`, so a wait
    /// would be cut off mid-wait rather than waiting, and a channel that was
    /// never signalled is a different answer from tmux failing to reply.
    /// Until those are told apart, the waiting side stays with
    /// [`Server::cmd`], where the timeout is the caller's to reason about.
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
    /// # Errors
    ///
    /// Returns an error when no suitable client exists or tmux refuses the
    /// command.
    pub async fn display_popup(
        &self,
        client: Option<&Client>,
        command: impl Into<OsString>,
    ) -> Result<(), Error> {
        let mut popup = Command::new("display-popup").arg("-E");
        if let Some(client) = client {
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
    /// # Errors
    ///
    /// Returns an error when no suitable client exists or tmux refuses an
    /// item.
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
    /// what they typed. Success means tmux opened the prompt, not that anyone
    /// answered it.
    ///
    /// # Errors
    ///
    /// Returns an error when no suitable client exists or tmux refuses.
    pub async fn command_prompt(
        &self,
        client: Option<&Client>,
        prompt: Option<&str>,
        command: impl Into<OsString>,
    ) -> Result<(), Error> {
        let mut request = Command::new("command-prompt");
        if let Some(client) = client {
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
    /// # Errors
    ///
    /// Returns an error when no suitable client exists or tmux refuses.
    pub async fn choose(&self, chooser: Chooser, client: Option<&Client>) -> Result<(), Error> {
        let name = chooser.command();
        let mut request = Command::new(name);
        if let Some(client) = client {
            request = request
                .arg("-t")
                .arg(client.name().to_string_lossy().into_owned());
        }

        listing::mutate(&self.core, name, request).await
    }

    /// Open tmux's window finder for a search string.
    ///
    /// This is separate from [`Server::choose`] because it needs something to
    /// search for, where the other choosers list what already exists.
    ///
    /// # Errors
    ///
    /// Returns an error when no suitable client exists or tmux refuses.
    pub async fn find_window(&self, client: Option<&Client>, search: &str) -> Result<(), Error> {
        let mut request = Command::new("find-window");
        if let Some(client) = client {
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
    /// Returns an error when no suitable client exists or tmux refuses.
    pub async fn display_panes(&self, client: Option<&Client>) -> Result<(), Error> {
        let mut request = Command::new("display-panes");
        if let Some(client) = client {
            request = request
                .arg("-t")
                .arg(client.name().to_string_lossy().into_owned());
        }

        listing::mutate(&self.core, "display-panes", request).await
    }

    /// Read one server option, decoded according to its declared kind.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn typed_option(&self, name: &str) -> Result<Option<OptionValue>, Error> {
        Ok(options::get(&self.core, options::Scope::Server, name)
            .await?
            .map(|value| OptionValue::decode(name, value)))
    }

    /// Read one global session option, decoded according to its declared kind.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn typed_global_option(&self, name: &str) -> Result<Option<OptionValue>, Error> {
        Ok(
            options::get(&self.core, options::Scope::GlobalSession, name)
                .await?
                .map(|value| OptionValue::decode(name, value)),
        )
    }

    /// Find the session with this exact name.
    ///
    /// Names are compared as bytes, because tmux permits names that are not
    /// valid UTF-8.
    ///
    /// The comparison happens here rather than through tmux's `-f`, which
    /// would filter server-side but requires building a format string around
    /// the name. A name containing `#`, `}`, or a comma would change the
    /// predicate's meaning, and tmux documents no escaping for those values,
    /// so a lookup would be an injection point.
    ///
    /// # Errors
    ///
    /// Returns an error when the session listing fails.
    pub async fn session(&self, name: impl AsRef<[u8]>) -> Result<Option<Session>, Error> {
        let name = name.as_ref();

        Ok(self
            .sessions()
            .await?
            .into_iter()
            .find(|session| session.name() == name))
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

    /// Find the session with this id.
    ///
    /// # Errors
    ///
    /// Returns an error when the session listing fails.
    pub async fn session_by_id(&self, id: &SessionId) -> Result<Option<Session>, Error> {
        // An id is a sigil and digits, so it can be handed to tmux as a
        // predicate and matched server-side. tmux returns the one row rather
        // than every row for this to scan.
        let infos = listing::sessions(&self.core, Some(&id.predicate("session_id"))).await?;

        Ok(infos
            .into_iter()
            .next()
            .map(|info| Session::new(Arc::clone(&self.core), info)))
    }

    /// Find the window with this id, through the first link that reaches it.
    ///
    /// A window linked into several sessions is returned once. Use
    /// [`Server::windows_or_empty`] when the link matters.
    ///
    /// # Errors
    ///
    /// Returns an error when the window listing fails.
    pub async fn window_by_id(&self, id: &WindowId) -> Result<Option<Window>, Error> {
        let projections = listing::windows(
            &self.core,
            listing::Scope::Server,
            Some(&id.predicate("window_id")),
        )
        .await?;

        // A window linked into several sessions has one row per link, and
        // activity belongs to the link rather than to the window: the same
        // window can be current in one session and not in another. Taking
        // whichever row tmux happened to list first would pick by session
        // name, so the current link wins and the lowest index breaks a tie.
        Ok(projections
            .into_iter()
            .min_by_key(|projection| {
                (
                    !projection.link().is_active(),
                    projection.link().identity().window_index(),
                )
            })
            .map(|projection| Window::new(Arc::clone(&self.core), projection)))
    }

    /// Find the pane with this id.
    ///
    /// # Errors
    ///
    /// Returns an error when the pane listing fails.
    pub async fn pane_by_id(&self, id: &PaneId) -> Result<Option<Pane>, Error> {
        let projections = listing::panes(
            &self.core,
            listing::Scope::Server,
            Some(&id.predicate("pane_id")),
        )
        .await?;

        Ok(projections
            .into_iter()
            .next()
            .map(|projection| Pane::new(Arc::clone(&self.core), projection)))
    }

    /// Find the client attached to this terminal.
    ///
    /// # Errors
    ///
    /// Returns an error when the client listing fails.
    pub async fn client(&self, name: impl AsRef<[u8]>) -> Result<Option<Client>, Error> {
        let name = name.as_ref();

        Ok(self
            .clients()
            .await?
            .into_iter()
            .find(|client| client.name() == name))
    }

    /// Fetch the whole hierarchy in three commands.
    ///
    /// Walking down with [`Server::sessions_or_empty`], then each session's windows,
    /// then each window's panes costs one command per object. tmux can answer
    /// the same question with `list-sessions`, `list-windows -a`, and
    /// `list-panes -a`, so this issues three regardless of how much is
    /// running and stitches the result by winlink.
    ///
    /// Use it when you want everything. Use the scoped accessors when you
    /// want one branch: they fetch less.
    ///
    /// The three listings are separate tmux commands, so this is not an
    /// atomic capture. A window created between them appears in one listing
    /// and not another, and is dropped rather than reported half-formed.
    ///
    /// # Errors
    ///
    /// Returns an error when any of the three listings fails.
    pub async fn hierarchy(&self) -> Result<Vec<SessionTree>, Error> {
        let (sessions, windows, panes) =
            tokio::try_join!(self.sessions(), self.windows(), self.panes(),)?;

        // Grouping is by the numeric part of each ID rather than the ID: it
        // is Copy and unique among IDs of one kind, so stitching the three
        // listings together allocates nothing per object.
        //
        // `list-panes -a` yields one row per winlink, so a pane in a window
        // that two sessions link appears twice. A pane belongs to exactly one
        // window however it was reached, so the duplicate rows describe the
        // same pane and only the first is kept.
        let mut seen = HashSet::new();
        let mut panes_by_window: HashMap<u32, Vec<Pane>> = HashMap::new();
        for pane in panes {
            if !seen.insert(pane.id().number()) {
                continue;
            }
            panes_by_window
                .entry(pane.window_id().number())
                .or_default()
                .push(pane);
        }

        let mut windows_by_session: HashMap<u32, Vec<WindowTree>> = HashMap::new();
        for window in windows {
            // Cloned, not moved: a window linked into several sessions appears
            // under each of them, and it holds the same panes in every one.
            let panes = panes_by_window
                .get(&window.id().number())
                .cloned()
                .unwrap_or_default();
            windows_by_session
                .entry(window.session_id().number())
                .or_default()
                .push(WindowTree { window, panes });
        }

        Ok(sessions
            .into_iter()
            .map(|session| {
                let windows = windows_by_session
                    .remove(&session.id().number())
                    .unwrap_or_default();
                SessionTree { session, windows }
            })
            .collect())
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
    /// Returns an error when tmux cannot be run at all. A daemon that is
    /// simply not running is reported through `Ok(false)`-shaped emptiness by
    /// the listings, so this distinguishes "nothing started" from "cannot ask".
    pub async fn check_alive(&self) -> Result<(), Error> {
        let result = self.cmd(Command::new("list-sessions")).await?;
        if result.success() {
            return Ok(());
        }

        Err(Error::ObjectGone {
            kind: ObjectKind::Session,
            id: self.identity().socket_path().display().to_string(),
        })
    }

    /// Report whether a session with this exact name exists.
    ///
    /// The comparison is over raw bytes, because tmux permits session names
    /// that are not valid UTF-8.
    ///
    /// # Errors
    ///
    /// Returns an error when the session listing fails.
    pub async fn has_session(&self, name: impl AsRef<[u8]>) -> Result<bool, Error> {
        let name = name.as_ref();

        Ok(self
            .sessions()
            .await?
            .iter()
            .any(|session| session.name() == name))
    }

    /// Record a listing failure that a lenient accessor is about to discard.
    ///
    /// The lenient contract hides the cause from the return type, so this is
    /// the only place it survives. Without the `tracing` feature the failure
    /// is dropped, which is why every lenient accessor has a loud
    /// List every session on the server, preserving any failure.
    ///
    /// # Errors
    ///
    /// Returns an error when the list command cannot run, or when its output
    /// cannot be decoded into snapshots.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// guard.session("work").await?;
    ///
    /// let sessions = guard.server().sessions().await?;
    /// assert_eq!(sessions.len(), 1);
    /// assert!(sessions[0].id().to_string().starts_with('$'));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn sessions(&self) -> Result<Vec<Session>, Error> {
        let infos = listing::sessions(&self.core, None).await?;

        Ok(infos
            .into_iter()
            .map(|info| Session::new(Arc::clone(&self.core), info))
            .collect())
    }

    #[cfg(test)]
    fn from_executor_for_test(executor: Arc<dyn Executor>) -> Self {
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

/// A consuming builder for one inert [`Server`] handle.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use libtmux::{DispatchLimits, OutputLimits, Server};
/// use std::time::Duration;
///
/// // Naming a socket and a budget is the whole of the configuration; everything
/// // else is per-command.
/// let server = Server::builder()
///     .socket_name("builder-example")
///     .default_timeout(Duration::from_secs(5))
///     .output_limits(OutputLimits::default().max_stdout_bytes(1024 * 1024))
///     .dispatch_limits(DispatchLimits::default().max_in_flight(4))
///     .build()?;
///
/// // Building does not start tmux, so this has not touched the machine yet.
/// let _ = server.identity();
/// # Ok(())
/// # }
/// ```
#[must_use = "a server builder has no effect until build is called"]
pub struct ServerBuilder {
    socket_name: Option<OsString>,
    socket_path: Option<PathBuf>,
    config_file: Option<PathBuf>,
    colors: Option<u16>,
    executable: OsString,
    timeout: Duration,
    output_limits: OutputLimits,
    dispatch_limits: DispatchLimits,
    #[cfg(feature = "test-support")]
    prevent_server_start: bool,
}

// Redacted like `ServerIdentity`'s, and for the same reason: a builder holds
// the socket path and the config path, and a caller who prints one while
// debugging should not put either into a log.
impl fmt::Debug for ServerBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerBuilder")
            .field(
                "socket_name",
                &self.socket_name.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "socket_path",
                &self.socket_path.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "config_file",
                &self.config_file.as_ref().map(|_| "<redacted>"),
            )
            .field("colors", &self.colors)
            .field("timeout", &self.timeout)
            .field("output_limits", &self.output_limits)
            .field("dispatch_limits", &self.dispatch_limits)
            .finish_non_exhaustive()
    }
}

impl ServerBuilder {
    fn new() -> Self {
        Self {
            socket_name: None,
            socket_path: None,
            config_file: None,
            colors: None,
            executable: OsString::from("tmux"),
            timeout: CoreConfiguration::default_timeout(),
            output_limits: OutputLimits::default(),
            dispatch_limits: DispatchLimits::default(),
            #[cfg(feature = "test-support")]
            prevent_server_start: false,
        }
    }

    /// Bound how many bytes one command may read from tmux.
    ///
    /// tmux answers with as many bytes as it has: a pane with a long history,
    /// a buffer holding a pasted file, a `run-shell` that keeps printing.
    /// Without a ceiling the operating system decides when to stop, and it
    /// does that by killing this process.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::OutputLimits;
    ///
    /// let server = libtmux::Server::builder()
    ///     .output_limits(OutputLimits::default().max_stdout_bytes(1024 * 1024))
    ///     .build()?;
    /// # let _ = server;
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the limits"]
    pub const fn output_limits(mut self, limits: OutputLimits) -> Self {
        self.output_limits = limits;
        self
    }

    /// Bound how many commands may run at once.
    ///
    /// Each one is a tmux client process with its own pipes and reader tasks,
    /// and tmux serializes them on the far side anyway, so past a point more
    /// clients buy queueing rather than throughput. A caller that fans out
    /// wide -- an agent, a reconciler sweeping every pane -- otherwise turns
    /// its own concurrency into pressure on the machine.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::DispatchLimits;
    ///
    /// let server = libtmux::Server::builder()
    ///     .dispatch_limits(DispatchLimits::default().max_in_flight(4))
    ///     .build()?;
    /// # let _ = server;
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the limits"]
    pub const fn dispatch_limits(mut self, limits: DispatchLimits) -> Self {
        self.dispatch_limits = limits;
        self
    }

    /// Select a named tmux socket.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder().socket_name("example").build()?;
    /// assert_eq!(server.socket_name(), Some(std::ffi::OsStr::new("example")));
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the socket name"]
    pub fn socket_name(mut self, name: impl Into<OsString>) -> Self {
        self.socket_name = Some(name.into());
        self
    }

    /// Select an explicit tmux socket path.
    ///
    /// Relative paths are joined to the working directory captured by
    /// [`ServerBuilder::build`].
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/explicit-example.sock")
    ///     .build()?;
    /// assert!(server.socket_path().is_absolute());
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the socket path"]
    pub fn socket_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.socket_path = Some(path.into());
        self
    }

    /// Select a tmux configuration file.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder()
    ///     .config_file("/tmp/libtmux-config-example.conf")
    ///     .build()?;
    /// assert!(server.config_file().is_some());
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the config path"]
    pub fn config_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_file = Some(path.into());
        self
    }

    /// Select tmux's 88- or 256-color compatibility mode.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder().colors(88).build()?;
    /// assert_eq!(server.colors(), Some(88));
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the color mode"]
    pub const fn colors(mut self, colors: u16) -> Self {
        self.colors = Some(colors);
        self
    }

    /// Select the tmux executable without checking that it exists yet.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder().tmux_executable("tmux").build()?;
    /// assert_eq!(server.tmux_executable(), std::ffi::OsStr::new("tmux"));
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the executable"]
    pub fn tmux_executable(mut self, executable: impl Into<OsString>) -> Self {
        self.executable = executable.into();
        self
    }

    /// Set the per-command process deadline.
    ///
    /// A duration too large for the platform's monotonic clock is treated as
    /// unbounded while command cancellation remains available.
    ///
    /// # Examples
    ///
    /// ```
    /// let timeout = std::time::Duration::from_secs(4);
    /// let server = libtmux::Server::builder().default_timeout(timeout).build()?;
    /// assert_eq!(server.default_timeout(), timeout);
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use = "use the returned builder to retain the timeout"]
    pub const fn default_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[cfg(feature = "test-support")]
    pub(crate) const fn prevent_server_start(mut self) -> Self {
        self.prevent_server_start = true;
        self
    }

    /// Capture process context and construct an inert server handle.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidServerConfiguration`] for invalid selector,
    /// path, color, working-directory, or socket-root inputs.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux::Server::builder().build()?;
    /// assert!(server.socket_path().is_absolute());
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    pub fn build(self) -> Result<Server, Error> {
        let selection = match (self.socket_name, self.socket_path) {
            (Some(name), None) => SocketSelection::Name(name),
            (None, Some(path)) => SocketSelection::Path(path),
            (None, None) => SocketSelection::Automatic,
            (Some(_), Some(_)) => {
                return Err(Error::invalid_server_configuration(
                    ServerConfigurationErrorKind::ConflictingSocketSelectors,
                ));
            }
        };
        let configuration = CoreConfiguration::resolve(
            &selection,
            self.config_file,
            self.colors,
            self.executable,
            self.timeout,
            BuildContext::capture(),
        )
        .map_err(Error::invalid_server_configuration)?
        .with_limits(self.output_limits, self.dispatch_limits);
        #[cfg(feature = "test-support")]
        let configuration = if self.prevent_server_start {
            configuration.prevent_server_start()
        } else {
            configuration
        };
        Ok(Server {
            core: Arc::new(Core::new(configuration)),
        })
    }
}

#[cfg(test)]
mod tests {

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio::sync::{Notify, watch};

    use super::Server;
    use crate::Error;
    use crate::command::CommandRequest;
    use crate::internal::executor::{DispatchFuture, Executor, ShutdownFuture};

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
#[derive(Clone, Debug)]
pub struct NewSessionOptions {
    name: OsString,
    start_directory: Option<PathBuf>,
    window_name: Option<OsString>,
    command: Option<OsString>,
    width: Option<u32>,
    height: Option<u32>,
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
            command = command.arg(shell_command);
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

/// One session and everything under it, from [`Server::hierarchy`].
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("work").await?;
/// session.new_window("editor").await?;
///
/// // One round of listings for the whole hierarchy, rather than one call per
/// // session and another per window.
/// let tree = guard.server().hierarchy().await?;
/// let found = tree
///     .iter()
///     .find(|branch| branch.session.name().to_string_lossy() == "work")
///     .expect("the session just created");
/// assert_eq!(found.windows.len(), 2);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SessionTree {
    /// The session.
    pub session: Session,
    /// Its windows, in tmux's order. A window linked into several sessions
    /// appears under each of them, as the listings report it.
    pub windows: Vec<WindowTree>,
}

/// One window and its panes, from [`Server::hierarchy`].
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::SplitDirection;
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("work").await?;
/// let window = session.active_window().await?.expect("a session has a window");
/// window.split(SplitDirection::Below).await?;
///
/// let tree = guard.server().hierarchy().await?;
/// let panes: usize = tree
///     .iter()
///     .flat_map(|branch| branch.windows.iter())
///     .map(|branch| branch.panes.len())
///     .sum();
/// assert_eq!(panes, 2);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct WindowTree {
    /// The window, carrying the link it was reached through.
    pub window: Window,
    /// Its panes, in tmux's order.
    pub panes: Vec<Pane>,
}

/// Typed filter handles for [`SessionTree`].
///
/// The session's own fields sit under [`session`], and [`windows`] is the
/// relation that makes a question about a session's contents expressible.
///
/// [`session`]: SessionTreeFields::session
/// [`windows`]: SessionTreeFields::windows
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "query")] {
/// use libtmux::query::Filterable as _;
/// use libtmux::{SessionTree, WindowTree};
///
/// let sessions = SessionTree::filter_fields();
/// let windows = WindowTree::filter_fields();
///
/// // The session's own fields sit beside the relation rather than behind it, so
/// // a question about the session and a question about what it contains compose.
/// let building = sessions
///     .session
///     .session_name
///     .starts_with("build")
///     .and(sessions.windows.any(windows.window.window_name.eq("editor")));
/// # let _ = building;
/// # }
/// ```
#[cfg(feature = "query")]
#[non_exhaustive]
pub struct SessionTreeFields {
    /// The session's own fields, the same set [`Session`] filters on.
    pub session: SessionFields<SessionTree>,
    /// The windows under this session.
    pub windows: ManyRelation<SessionTree, WindowTree>,
}

// Named rather than exhaustive, as the generated field sets are: every handle
// is a zero-sized name, so listing them prints a page of nothing.
#[cfg(feature = "query")]
impl fmt::Debug for SessionTreeFields {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionTreeFields")
            .finish_non_exhaustive()
    }
}

/// Typed filter handles for [`WindowTree`].
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "query")] {
/// use libtmux::query::Filterable as _;
/// use libtmux::{Pane, WindowTree};
///
/// let windows = WindowTree::filter_fields();
/// let panes = Pane::filter_fields();
///
/// // `any` asks whether some pane matches, which is not the same question as
/// // filtering the panes themselves: this keeps whole windows.
/// let has_dead_pane = windows.panes.any(panes.pane_dead.eq(true));
/// # let _ = has_dead_pane;
/// # }
/// ```
#[cfg(feature = "query")]
#[non_exhaustive]
pub struct WindowTreeFields {
    /// The window's own fields, the same set [`Window`] filters on.
    pub window: WindowFields<WindowTree>,
    /// The panes in this window.
    pub panes: ManyRelation<WindowTree, Pane>,
}

#[cfg(feature = "query")]
impl fmt::Debug for WindowTreeFields {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WindowTreeFields")
            .finish_non_exhaustive()
    }
}

/// The wire name of the relation from a session to its windows.
#[cfg(feature = "query")]
const WINDOWS_RELATION: &str = "windows";

/// The wire name of the relation from a window to its panes.
#[cfg(feature = "query")]
const PANES_RELATION: &str = "panes";

/// Filtering a hierarchy branch reaches the session's fields and its windows.
///
/// A [`Session`] handle cannot carry a relation, because it does not hold its
/// windows -- it fetches them. This is the shape that does hold them, so it is
/// the shape a relation can be asked about.
#[cfg(feature = "query")]
impl Filterable for SessionTree {
    type Fields = SessionTreeFields;

    const FILTER_TARGET: &'static str = "session_tree";

    fn filter_fields() -> Self::Fields {
        Self::Fields {
            session: SessionFields::for_target(Self::FILTER_TARGET),
            windows: crate::query::__private::many_relation(Self::FILTER_TARGET, WINDOWS_RELATION),
        }
    }

    fn __filter_matches(&self, predicate: &crate::query::__private::Predicate) -> bool {
        if predicate.field() == WINDOWS_RELATION {
            return predicate.matches_many(&self.windows);
        }

        self.session.__filter_matches(predicate)
    }

    fn __filter_validate(
        predicate: &crate::query::__private::Predicate,
    ) -> Result<(), crate::query::FilterExpressionError> {
        if predicate.field() == WINDOWS_RELATION {
            return predicate.validate_many::<WindowTree>();
        }

        <Session as Filterable>::__filter_validate(predicate)
    }
}

/// Filtering a window branch reaches the window's fields and its panes.
#[cfg(feature = "query")]
impl Filterable for WindowTree {
    type Fields = WindowTreeFields;

    const FILTER_TARGET: &'static str = "window_tree";

    fn filter_fields() -> Self::Fields {
        Self::Fields {
            window: WindowFields::for_target(Self::FILTER_TARGET),
            panes: crate::query::__private::many_relation(Self::FILTER_TARGET, PANES_RELATION),
        }
    }

    fn __filter_matches(&self, predicate: &crate::query::__private::Predicate) -> bool {
        if predicate.field() == PANES_RELATION {
            return predicate.matches_many(&self.panes);
        }

        self.window.__filter_matches(predicate)
    }

    fn __filter_validate(
        predicate: &crate::query::__private::Predicate,
    ) -> Result<(), crate::query::FilterExpressionError> {
        if predicate.field() == PANES_RELATION {
            return predicate.validate_many::<Pane>();
        }

        <Window as Filterable>::__filter_validate(predicate)
    }
}
