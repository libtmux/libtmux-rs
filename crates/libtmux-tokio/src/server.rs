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
use crate::internal::support::capabilities::RUN_SHELL_OUTPUT_IN_COPY_MODE;
use crate::pane::Pane;
use crate::session::Session;
use crate::{
    Command, CommandChain, CommandResult, EngineCapabilities, Error, ListingDecodeError,
    NewSession, ReleaseVersion, ServerConfigurationErrorKind, ServerGeneration, ServerIdentity,
    TmuxVersion,
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

/// How much a user on the server's access list may do.
///
/// An enum rather than two flags because tmux's `-r` and `-w` are exclusive:
/// passing both is a contradiction the type makes unrepresentable.
///
/// # Examples
///
/// ```no_run
/// # async fn example(server: &libtmux_tokio::Server) -> Result<(), libtmux_tokio::Error> {
/// use libtmux_tokio::AccessMode;
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
/// # async fn example(server: &libtmux_tokio::Server) -> Result<(), libtmux_tokio::Error> {
/// use libtmux_tokio::Principal;
///
/// for rule in server.access_rules().await? {
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
/// # async fn example(server: &libtmux_tokio::Server) -> Result<(), libtmux_tokio::Error> {
/// use libtmux_tokio::AccessMode;
///
/// for rule in server.access_rules().await? {
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

/// Which prompt tmux is remembering answers for.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux_tokio::PromptKind;
///
/// let guard = libtmux_tokio::test::TestServer::new().await?;
///
/// // tmux keeps a separate history per prompt kind, so a command typed at the
/// // `:` prompt is not offered when searching.
/// assert!(guard.server().prompt_history(PromptKind::Command).await?.is_empty());
/// assert!(guard.server().prompt_history(PromptKind::Search).await?.is_empty());
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
/// **Connecting.** Reach the server tmux picks, one that `LIBTMUX_SOCKET_PATH`
/// or `LIBTMUX_SOCKET` names, one you name, or the server this process is
/// already inside, from `TMUX` or from a value you pass; ask whether anything
/// is listening. A socket path does not identify a daemon, so
/// `generation` captures the one answering, and `require_generation` fails once
/// a different one has taken the endpoint. With the `control-mode` feature,
/// which is off by default, run a handle's commands over an open control
/// connection. Shutting down stops accepting clients and waits for the active
/// ones; it never stops the daemon.
///
/// [`new`], [`builder`], [`from_env`], [`from_env_value`], [`is_alive`],
/// [`check_alive`], [`generation`], [`require_generation`],
/// [`over_control_mode`], [`owns_control_client`], [`shutdown`]
///
/// **Reading how a handle was built.** Read back the endpoint identity, the
/// socket by path and by name, the config file, the color mode, the tmux
/// executable, and the timeout each command gets.
///
/// [`identity`], [`socket_path`], [`socket_name`], [`config_file`], [`colors`],
/// [`tmux_executable`], [`default_timeout`]
///
/// **Asking which tmux this is.** Detect what the configured tmux executable
/// supports, which also describes one below the supported daemon minimum, and
/// retry after a failed detection. Read the version of the daemon this handle
/// reaches, which has passed the supported-minimum check and runs no user hook.
/// Report where the configured tmux resolves.
///
/// [`capabilities`], [`daemon_version`], [`resolved_tmux_executable`]
///
/// **Finding one thing.** Look up a session or client by name, or a session,
/// window or pane by tmux ID. Each reports `Ok(None)` when tmux does not have
/// it.
///
/// [`session`], [`client`], [`session_by_id`], [`window_by_id`],
/// [`pane_by_id`]
///
/// **Listing everything.** List every session, window, pane or client; each
/// keeps the reason it failed, so an outage does not read as an empty server.
/// Or gather the tree with one generation probe and three listings, regardless
/// of how many objects it contains.
///
/// [`sessions`], [`windows`], [`panes`], [`clients`], [`hierarchy`]
///
/// **Querying things and getting around.** Ask whether a session exists and
/// which ones a client is attached to. Narrow a listing with `.iter()` and a
/// predicate built from the typed fields [`Filterable`] provides, then take
/// exactly one or one-or-none, or search one branch; the `query` feature
/// supplies this and is on by default. The [`SessionTree`] and [`WindowTree`]
/// that `hierarchy` returns hold their children as fields, so walking them
/// costs no further tmux command. Moving up re-reads tmux and reports
/// `Ok(None)` when the parent has gone.
///
/// - Ask: [`has_session`], [`attached_sessions`]
/// - Narrow: [`QueryIteratorExt::matching`], [`QueryIteratorExt::exactly_one`],
///   [`QueryIteratorExt::one_or_none`], [`Session::search_windows`],
///   [`Window::search_panes`]
/// - Down: [`Session::windows`], [`Window::panes`],
///   [`Session::active_window`], [`Window::active_pane`]
/// - Up: [`Pane::window`], [`Pane::session`], [`Window::session`]
///
/// **Changing things.** Create a session, kill the server, lock every client,
/// or start a server without creating a session. A body can also run in a
/// session that is killed afterwards: a failure to create it, to run the body
/// or to kill it comes back as a `ScopeError`, and a cleanup failure carries
/// [`Error::AfterEffect`]. If the future is dropped, the session is still
/// killed while the Tokio runtime is alive.
///
/// [`new_session`], [`kill`], [`lock_all`], [`start`], [`with_session`]
///
/// **Options and hooks.** Read the options set on the server itself, as names
/// or decoded by their declared kind, or read one by name; the session and
/// window defaults that sessions inherit have a global read and write of their
/// own. A typed write checks the value against tmux's option table before
/// anything is sent, and an unchecked write sends text and leaves tmux to
/// refuse it. An option that holds a numbered set, such as `command-alias`, has
/// its own reads and writes. Hooks can be set, written whole, removed, and read
/// back, either every hook that holds something or one by name.
/// [`OptionValue`] says how the reads and writes fit together.
///
/// - Read: [`option_names`], [`options`], [`typed_option`],
///   [`typed_global_option`], [`typed_global_window_option`]
/// - Typed write: [`set_typed_option`], [`set_typed_global_option`],
///   [`set_typed_global_window_option`]
/// - Unchecked write: [`set_option`], [`set_global_option`],
///   [`set_global_window_option`]
/// - Remove: [`unset_option`]
/// - Arrays: [`array_option`], [`set_array_option`], [`append_array_option`],
///   [`unset_array_option`]
/// - Hooks: [`set_hook`], [`set_hooks`], [`unset_hook`], [`hooks`], [`hook`]
///
/// **Everything else tmux keeps.** Paste buffers, including moving one through
/// a file; key bindings, also as typed fields on tmux 3.7 and later; the
/// global environment; format expansion, configuration, and layout
/// validation, which checks each layout and its pane count before any command
/// applies it; shell commands; wait-for channels, including a lock held for
/// the length of a body, which is released when the body returns, fails or
/// panics, or when the scope is dropped; the history of each kind of prompt,
/// which clearing forgets for every kind at once; and the access list, whose
/// owner is always present and writable, so a caller cannot lock itself out.
///
/// - Buffers: [`buffer`], [`set_buffer`], [`buffer_names`], [`delete_buffer`],
///   [`load_buffer`], [`save_buffer`]
/// - Keys: [`bind_key`], [`unbind_key`], [`key_bindings`],
///   [`typed_key_bindings`]
/// - Environment: [`environment`], [`environment_all`], [`set_environment`],
///   [`hide_environment`], [`unset_environment`]
/// - Formats and config: [`format`], [`source_file`], [`validate_layouts`]
/// - Shell: [`run_shell`], [`spawn_shell`]
/// - Channels: [`lock_channel`], [`unlock_channel`], [`signal_channel`],
///   [`wait_for_channel`], [`with_channel_lock`]
/// - Prompts: [`prompt_history`], [`clear_prompt_history`]
/// - Access: [`access_rules`], [`grant_access`], [`revoke_access`]
///
/// **Things that need a terminal.** Draw a popup, menu, prompt, chooser,
/// window finder or pane numbers on an attached client. Popups, menus and
/// prompts fail without one; tmux accepts the chooser with no client, and pane
/// numbers from tmux 3.8, because each opens as a mode in a pane.
///
/// [`display_popup`], [`display_menu`], [`command_prompt`], [`choose`],
/// [`find_window`], [`display_panes`]
///
/// Anything tmux can do that is not here is reachable through [`cmd`], which
/// runs an arbitrary command and hands back its result, or [`chain`], which
/// dispatches several as one invocation that stops at the first failure and
/// cannot say which one failed.
///
/// [`new`]: Server::new
/// [`builder`]: Server::builder
/// [`from_env`]: Server::from_env
/// [`is_alive`]: Server::is_alive
/// [`check_alive`]: Server::check_alive
/// [`generation`]: Server::generation
/// [`require_generation`]: Server::require_generation
/// [`resolved_tmux_executable`]: Server::resolved_tmux_executable
/// [`over_control_mode`]: Server::over_control_mode
/// [`owns_control_client`]: Server::owns_control_client
/// [`from_env_value`]: Server::from_env_value
/// [`shutdown`]: Server::shutdown
/// [`identity`]: Server::identity
/// [`socket_path`]: Server::socket_path
/// [`socket_name`]: Server::socket_name
/// [`config_file`]: Server::config_file
/// [`colors`]: Server::colors
/// [`tmux_executable`]: Server::tmux_executable
/// [`default_timeout`]: Server::default_timeout
/// [`capabilities`]: Server::capabilities
/// [`daemon_version`]: Server::daemon_version
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
/// [`has_session`]: Server::has_session
/// [`attached_sessions`]: Server::attached_sessions
/// [`QueryIteratorExt::matching`]: crate::query::QueryIteratorExt::matching
/// [`QueryIteratorExt::exactly_one`]: crate::query::QueryIteratorExt::exactly_one
/// [`QueryIteratorExt::one_or_none`]: crate::query::QueryIteratorExt::one_or_none
/// [`Filterable`]: crate::query::Filterable
/// [`Session::search_windows`]: crate::Session::search_windows
/// [`Window::search_panes`]: crate::Window::search_panes
/// [`SessionTree`]: crate::SessionTree
/// [`WindowTree`]: crate::WindowTree
/// [`Session::windows`]: crate::Session::windows
/// [`Window::panes`]: crate::Window::panes
/// [`Pane::window`]: crate::Pane::window
/// [`Pane::session`]: crate::Pane::session
/// [`Window::session`]: crate::Window::session
/// [`Session::active_window`]: crate::Session::active_window
/// [`Window::active_pane`]: crate::Window::active_pane
/// [`new_session`]: Server::new_session
/// [`kill`]: Server::kill
/// [`lock_all`]: Server::lock_all
/// [`with_session`]: Server::with_session
/// [`start`]: Server::start
/// [`Error::AfterEffect`]: crate::Error::AfterEffect
/// [`option_names`]: Server::option_names
/// [`options`]: Server::options
/// [`unset_option`]: Server::unset_option
/// [`set_global_option`]: Server::set_global_option
/// [`set_global_window_option`]: Server::set_global_window_option
/// [`typed_option`]: Server::typed_option
/// [`set_typed_option`]: Server::set_typed_option
/// [`typed_global_option`]: Server::typed_global_option
/// [`set_typed_global_option`]: Server::set_typed_global_option
/// [`typed_global_window_option`]: Server::typed_global_window_option
/// [`set_typed_global_window_option`]: Server::set_typed_global_window_option
/// [`set_option`]: Server::set_option
/// [`OptionValue`]: crate::OptionValue
/// [`set_hook`]: Server::set_hook
/// [`unset_hook`]: Server::unset_hook
/// [`set_hooks`]: Server::set_hooks
/// [`hooks`]: Server::hooks
/// [`hook`]: Server::hook
/// [`array_option`]: Server::array_option
/// [`set_array_option`]: Server::set_array_option
/// [`append_array_option`]: Server::append_array_option
/// [`unset_array_option`]: Server::unset_array_option
/// [`buffer`]: Server::buffer
/// [`set_buffer`]: Server::set_buffer
/// [`buffer_names`]: Server::buffer_names
/// [`delete_buffer`]: Server::delete_buffer
/// [`load_buffer`]: Server::load_buffer
/// [`save_buffer`]: Server::save_buffer
/// [`bind_key`]: Server::bind_key
/// [`unbind_key`]: Server::unbind_key
/// [`key_bindings`]: Server::key_bindings
/// [`typed_key_bindings`]: Server::typed_key_bindings
/// [`environment`]: Server::environment
/// [`environment_all`]: Server::environment_all
/// [`set_environment`]: Server::set_environment
/// [`hide_environment`]: Server::hide_environment
/// [`unset_environment`]: Server::unset_environment
/// [`format`]: Server::format
/// [`source_file`]: Server::source_file
/// [`validate_layouts`]: Server::validate_layouts
/// [`run_shell`]: Server::run_shell
/// [`spawn_shell`]: Server::spawn_shell
/// [`lock_channel`]: Server::lock_channel
/// [`unlock_channel`]: Server::unlock_channel
/// [`signal_channel`]: Server::signal_channel
/// [`wait_for_channel`]: Server::wait_for_channel
/// [`with_channel_lock`]: Server::with_channel_lock
/// [`prompt_history`]: Server::prompt_history
/// [`clear_prompt_history`]: Server::clear_prompt_history
/// [`access_rules`]: Server::access_rules
/// [`grant_access`]: Server::grant_access
/// [`revoke_access`]: Server::revoke_access
/// [`display_popup`]: Server::display_popup
/// [`display_menu`]: Server::display_menu
/// [`command_prompt`]: Server::command_prompt
/// [`choose`]: Server::choose
/// [`find_window`]: Server::find_window
/// [`display_panes`]: Server::display_panes
/// [`cmd`]: Server::cmd
/// [`chain`]: Server::chain
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux_tokio::test::TestServer;
///
/// // In your own code this is `libtmux_tokio::Server::new()?`; the fixture keeps
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
/// use libtmux_tokio::ChannelWait;
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

    pub(crate) async fn endpoint_version(&self) -> Result<TmuxVersion, Error> {
        self.core.endpoint_version().await
    }

    #[cfg(feature = "test-support")]
    pub(crate) async fn fixture_pid(&self) -> Result<CommandResult, Error> {
        self.core.fixture_pid().await
    }

    #[cfg(feature = "control-mode")]
    pub(crate) fn native_source(&self) -> &Self {
        self.core.native_server().unwrap_or(self)
    }

    #[cfg(feature = "control-mode")]
    pub(crate) async fn spawn_control(
        &self,
        session: &SessionId,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<PersistentChild, Error> {
        self.core.spawn_control(session, deadline).await
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
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux_tokio::test::TestServer::new().await?;
    /// let server = guard.server();
    ///
    /// // Nothing has opened a control client yet.
    /// assert!(!server.owns_control_client(std::process::id()));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "control-mode")]
    #[must_use]
    pub fn owns_control_client(&self, pid: u32) -> bool {
        self.core.owns_control_client(pid)
    }

    /// Construct a server whose socket is chosen as [`ServerBuilder::build`]
    /// describes: from `LIBTMUX_SOCKET_PATH` or `LIBTMUX_SOCKET`, else left to
    /// tmux.
    ///
    /// # Errors
    ///
    /// Returns an error when the working directory or socket root cannot be
    /// captured.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux_tokio::Server::new()?;
    /// assert!(server.socket_path().is_absolute());
    /// # Ok::<(), libtmux_tokio::Error>(())
    /// ```
    pub fn new() -> Result<Self, Error> {
        Self::builder().build()
    }

    /// Start a consuming server builder.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux_tokio::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/builder-example.sock")
    ///     .build()?;
    /// assert_eq!(server.socket_path(), std::path::Path::new("/tmp/libtmux-rs-test/builder-example.sock"));
    /// # Ok::<(), libtmux_tokio::Error>(())
    /// ```
    pub fn builder() -> ServerBuilder {
        ServerBuilder::new()
    }

    /// Return the captured structural endpoint identity.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux_tokio::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/identity-example.sock")
    ///     .build()?;
    /// assert_eq!(server.identity(), server.identity());
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// let server = libtmux_tokio::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/socket-example.sock")
    ///     .build()?;
    /// assert!(server.socket_path().is_absolute());
    /// # Ok::<(), libtmux_tokio::Error>(())
    /// ```
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        self.identity().socket_path()
    }

    /// Return the socket name tmux is given as `-L`, if any.
    ///
    /// It is `None` when a socket path was given, and when nothing chose a
    /// socket, so that tmux picks the server itself.
    ///
    /// # Examples
    ///
    /// ```
    /// let server = libtmux_tokio::Server::builder().socket_name("example").build()?;
    /// assert_eq!(server.socket_name(), Some(std::ffi::OsStr::new("example")));
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// let server = libtmux_tokio::Server::builder()
    ///     .config_file("/tmp/libtmux-example.conf")
    ///     .build()?;
    /// assert_eq!(server.config_file(), Some(std::path::Path::new("/tmp/libtmux-example.conf")));
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// let server = libtmux_tokio::Server::builder().colors(256).build()?;
    /// assert_eq!(server.colors(), Some(256));
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// let server = libtmux_tokio::Server::builder().tmux_executable("tmux").build()?;
    /// assert_eq!(server.tmux_executable(), std::ffi::OsStr::new("tmux"));
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// let server = libtmux_tokio::Server::builder()
    ///     .tmux_executable("/bin/sh")
    ///     .build()?;
    /// assert_eq!(
    ///     server.resolved_tmux_executable(),
    ///     Some(std::path::PathBuf::from("/bin/sh")),
    /// );
    ///
    /// let missing = libtmux_tokio::Server::builder()
    ///     .tmux_executable("tmux-that-is-not-installed")
    ///     .build()?;
    /// assert_eq!(missing.resolved_tmux_executable(), None);
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// let server = libtmux_tokio::Server::builder().default_timeout(timeout).build()?;
    /// assert_eq!(server.default_timeout(), timeout);
    /// # Ok::<(), libtmux_tokio::Error>(())
    /// ```
    #[must_use]
    pub fn default_timeout(&self) -> Duration {
        self.core.configuration().timeout()
    }

    /// Detect and share capabilities for the configured tmux executable.
    ///
    /// Failed initialization is not cached and may be retried. This diagnostic
    /// also describes executables below the supported daemon minimum.
    ///
    /// # Errors
    ///
    /// Returns an error when the executable cannot be run, its version probe
    /// fails, or its output is invalid.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux_tokio::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/capabilities-example-absent.sock")
    ///     .build()?;
    /// assert!(!server.capabilities().await?.tmux_version().raw().is_empty());
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux_tokio::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn capabilities(&self) -> Result<&EngineCapabilities, Error> {
        self.core.capabilities().await
    }

    /// Return the version of the daemon this handle reaches.
    ///
    /// The version has passed the supported-minimum check. A cold endpoint
    /// reports the executable that would start the daemon. The answer is
    /// cached with the handle's other version checks, so asking costs one
    /// probe at most, and no user hook runs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedTmuxVersion`] or
    /// [`Error::UnrecognizedTmuxVersion`] for a daemon libtmux refuses,
    /// [`Error::InvalidVersionOutput`] or [`Error::VersionProbeFailed`] when
    /// no version can be read, and a transport error when tmux cannot be run.
    /// A version probe may already have reached the daemon before an error.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux_tokio::test::TestServer::new().await?;
    /// let version = guard.server().daemon_version().await?;
    /// assert!(version.meets(&libtmux_tokio::TmuxVersion::MIN_SUPPORTED));
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn daemon_version(&self) -> Result<TmuxVersion, Error> {
        self.core.format_version().await
    }

    /// Execute one raw logical tmux command.
    ///
    /// A non-zero process exit status is returned in [`CommandResult`]. The
    /// connected daemon must meet [`TmuxVersion::MIN_SUPPORTED`]; a cold
    /// endpoint checks the executable that would start it. A queue guard
    /// refuses a replacement with a different version before effects.
    /// Standalone `-V` and `-h` retain native executable diagnostics below the
    /// floor, including on a handle with an open control connection.
    ///
    /// # Errors
    ///
    /// Returns an error when the process cannot be started, captured, awaited,
    /// or is cancelled by timeout or shutdown. Invalid or unsupported daemon
    /// versions and failed version probes are errors before dispatch.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux_tokio::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/command-example-absent.sock")
    ///     .build()?;
    /// let result = server.cmd(libtmux_tokio::Command::new("list-sessions")).await?;
    /// assert!(result.exit_code().is_some());
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux_tokio::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn cmd(&self, command: Command) -> Result<CommandResult, Error> {
        self.core.execute(command).await
    }

    /// Validate every planned layout before scripts or tmux mutations.
    ///
    /// Each item pairs layout bytes with the required pane count. Classic
    /// layouts are checked for checksums, nonempty trees, unsigned fields and
    /// depth up to 256 nested groups. tmux owns geometry correction and pruning.
    /// JSON layouts require tmux 3.8 or newer; tmux validates their
    /// contents. The version check queries the daemon even when no layouts need a version
    /// check; a cold endpoint uses the selected executable. Unique name
    /// abbreviations are accepted.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLayout`] for invalid syntax, ambiguous names or
    /// insufficient pane cells, [`Error::UnsupportedCapability`] for layouts
    /// unavailable on the daemon, [`Error::UnsupportedTmuxVersion`] below the
    /// core floor, or the version probe's transport/decode error.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn validate(server: &libtmux_tokio::Server) -> Result<(), libtmux_tokio::Error> {
    /// use std::ffi::OsStr;
    /// server.validate_layouts([(OsStr::new("even-h"), 2)]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn validate_layouts<'a>(
        &self,
        layouts: impl IntoIterator<Item = (&'a OsStr, usize)>,
    ) -> Result<(), Error> {
        let pending = crate::layout::prepare(layouts)?;
        crate::layout::resolve(&pending, &crate::layout::version(self).await?)?;
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
    /// use libtmux_tokio::{Command, CommandChain};
    ///
    /// let server = libtmux_tokio::Server::builder()
    ///     .socket_path("/tmp/libtmux-rs-test/chain-example-absent.sock")
    ///     .build()?;
    /// let chain = CommandChain::new(Command::new("list-sessions"))
    ///     .then(Command::new("list-panes"));
    /// let result = server.chain(chain).await?;
    /// assert!(result.exit_code().is_some());
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// enabled, it also closes persistent control connections owned by this
    /// server. Calling shutdown on a control-routed server drains the ordinary
    /// subprocess pool it shares with its source and sibling routes. The
    /// original sender and event receiver retain their borrowed connection.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ShutdownPending`] if cleanup exceeds the one-second
    /// allowance. Admission stays closed. Keep the runtime alive and call
    /// this method again to await the same supervisors. Once they finish,
    /// each shutdown call reports the first ordinary subprocess cleanup
    /// failure, including when its caller dropped the request future.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux_tokio::Server::new()?;
    /// server.shutdown().await?;
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux_tokio::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn shutdown(&self) -> Result<(), Error> {
        crate::internal::operation::finish_shutdown(self.core.shutdown()).await
    }

    /// Build a server from the `TMUX` variable tmux exports into every pane.
    ///
    /// This is how a caller asks for that server by name, as `-S`. [`Server::new`]
    /// and [`Server::builder`] pass no socket of their own, so tmux picks.
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
    /// let outside = libtmux_tokio::Server::from_env_value(None::<OsString>);
    /// assert!(outside.is_err(), "a process outside tmux has no TMUX value");
    ///
    /// let inside = libtmux_tokio::Server::from_env_value(Some("/tmp/tmux-1000/default,7,$0"))?;
    /// assert_eq!(inside.socket_path(), std::path::Path::new("/tmp/tmux-1000/default"));
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    pub async fn attached_sessions(&self) -> Result<Vec<Session>, Error> {
        let mut sessions = self.sessions().await?;
        sessions.retain(Session::is_attached);

        Ok(sessions)
    }

    /// Create a detached session and return the identities tmux acknowledged.
    ///
    /// The receipt contains the session, first window, first pane and daemon
    /// generation from one action. It can survive a later hook, deadline or
    /// client failure through [`Error::creation_receipt`].
    ///
    /// # Errors
    ///
    /// Returns version, admission and transport failures, or the shared
    /// operation's classified creation failure. An absent or ambiguous receipt
    /// leaves the effect unknown and must not cause an automatic retry.
    ///
    /// # Cancel safety
    ///
    /// Dropping a submitted request asks its existing supervisor to terminate
    /// and reap the client. tmux may already have created the session. The
    /// owner retains any verified receipt while cleanup finishes.
    pub async fn new_session(
        &self,
        options: impl Into<NewSession>,
    ) -> Result<libtmux_core::CreatedSession, Error> {
        let deadline = self.core.operation_deadline();
        self.new_session_until(options.into(), deadline).await
    }

    pub(crate) async fn new_session_until(
        &self,
        options: NewSession,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<libtmux_core::CreatedSession, Error> {
        let nonce = crate::internal::operation::nonce()?;
        let operation = crate::internal::operation::OperationContext::creation(
            self.socket_path(),
            nonce,
            deadline,
        );
        let (result, late) = self
            .core
            .execute_operation(
                options.ordinary_arguments(nonce),
                Command::new("new-session").summary(),
                operation,
            )
            .await?;
        let status = result.exit_code();
        let (stdout, stderr) = result.into_streams();
        libtmux_core::Reply::new(status, stdout, stderr.len(), late)
            .ordinary_creation(options, self.socket_path(), nonce)
            .map_err(Into::into)
    }

    /// Read owned observations of every session, window, pane or client.
    ///
    /// Both native clients return the same snapshot types. A snapshot keeps
    /// the field values from its listing after the daemon changes.
    ///
    /// # Errors
    ///
    /// Returns an error when the deadline expires or tmux cannot supply complete rows.
    pub async fn snapshots<S: libtmux_core::Snapshot>(&self) -> Result<Vec<S>, Error> {
        let deadline = self.core.operation_deadline();
        let version = match deadline {
            Some(end) => tokio::time::timeout_at(end, self.daemon_version())
                .await
                .map_err(|_| libtmux_core::Error::LateObservation {
                    operation: S::COMMAND,
                })??,
            None => self.daemon_version().await?,
        };
        let request =
            libtmux_core::SnapshotList::<S>::new(&version, crate::internal::operation::nonce()?);
        let (result, late) = self
            .core
            .execute_operation(
                request.arguments(),
                Command::new(S::COMMAND.to_string()).summary(),
                crate::internal::operation::OperationContext::observation(deadline),
            )
            .await?;
        request
            .decode(
                result.exit_code(),
                result.stdout(),
                result.stderr().len(),
                late,
            )
            .map_err(Into::into)
    }

    /// Observe an exact pane's endpoint and daemon generation without attaching.
    ///
    /// # Errors
    ///
    /// Rejects missing or inconsistent pane metadata, unsupported versions,
    /// admission failures, interrupted clients and late observations.
    pub async fn observe_pane(&self, pane: crate::PaneId) -> Result<libtmux_core::PaneKey, Error> {
        let observed = self
            .observe_pane_until(pane, self.core.operation_deadline())
            .await?;
        Ok(observed.key().clone())
    }

    /// Capture viewport bytes only from the pane and generation named by a key.
    ///
    /// Metadata and capture share one deadline. The capture queue checks the
    /// daemon generation, pane, version and viewport extent before reading.
    /// Hook output after the viewport is excluded from the returned bytes.
    ///
    /// # Errors
    ///
    /// Rejects another endpoint, a replaced or absent pane, interrupted or late
    /// reads, failed hooks and incomplete viewport output. Failed reads never
    /// expose a partial screen through their error.
    pub async fn capture(&self, key: &libtmux_core::PaneKey) -> Result<Vec<u8>, Error> {
        self.capture_grid(key, false).await
    }

    pub(crate) async fn capture_grid(
        &self,
        key: &libtmux_core::PaneKey,
        escapes: bool,
    ) -> Result<Vec<u8>, Error> {
        self.capture_grid_until(key, escapes, self.core.operation_deadline())
            .await
    }

    pub(crate) async fn capture_grid_until(
        &self,
        key: &libtmux_core::PaneKey,
        escapes: bool,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Vec<u8>, Error> {
        let mut command = Command::new("capture-pane")
            .arg("-p")
            .arg("-t")
            .arg(key.id().to_string());
        if escapes {
            command = command.arg("-e");
        }
        self.capture_ordinary_until(key, &command, (None, None), false, deadline)
            .await
    }

    pub(crate) async fn capture_ordinary_until(
        &self,
        key: &libtmux_core::PaneKey,
        command: &Command,
        range: (Option<&str>, Option<&str>),
        pending: bool,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Vec<u8>, Error> {
        if key.server_identity() != self.identity() {
            return Err(libtmux_core::Error::WrongEndpoint.into());
        }
        let observed = self.observe_pane_until(key.id(), deadline).await?;
        let ranged = !pending && (range.0.is_some() || range.1.is_some());
        let mut arguments = if ranged {
            observed.capture_range_arguments(self.socket_path(), key)?
        } else {
            observed.capture_arguments(self.socket_path(), key)?
        };
        arguments.extend(command.logical_words().skip(4).map(OsStr::to_os_string));
        if pending {
            arguments.push(OsString::from("-C"));
        }
        let (result, late) = self
            .core
            .execute_operation(
                arguments,
                Command::new("capture-pane").summary(),
                crate::internal::operation::OperationContext::observation(deadline),
            )
            .await?;
        if let Some(error) = crate::internal::operation::server_gone_for_observation(
            self.socket_path(),
            "capture-pane",
            &result,
            late,
        ) {
            return Err(error);
        }
        let status = result.exit_code();
        let (stdout, stderr) = result.into_streams();
        if pending {
            observed.decode_pending_capture(status, stdout, &stderr, late)
        } else if ranged {
            observed.decode_capture_range(status, stdout, &stderr, late, range)
        } else {
            observed.decode_capture(status, stdout, &stderr, late)
        }
        .map_err(Into::into)
    }

    #[cfg(feature = "plan")]
    pub(crate) async fn capture_bound_grid(
        &self,
        pane: crate::PaneId,
        escapes: bool,
    ) -> Result<Vec<u8>, Error> {
        self.capture_grid(&self.core.observed_key(pane)?, escapes)
            .await
    }

    pub(crate) async fn observe_pane_until(
        &self,
        pane: crate::PaneId,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<libtmux_core::OrdinaryPaneObservation, Error> {
        let probe = libtmux_core::OrdinaryPaneProbe::new(
            pane.clone(),
            crate::internal::operation::nonce()?,
        );
        let (result, late) = self
            .core
            .execute_operation(
                probe.arguments(),
                Command::new("run-shell").summary(),
                crate::internal::operation::OperationContext::observation(deadline),
            )
            .await?;
        if !late {
            if let Some(error) = result.refusal_for("observe pane") {
                if error.is_object_gone() {
                    return Err(libtmux_core::Error::PaneNotFound(pane).into());
                }
            }
        }
        if let Some(error) = crate::internal::operation::server_gone_for_observation(
            self.socket_path(),
            "observe pane",
            &result,
            late,
        ) {
            return Err(error);
        }
        probe
            .decode(
                self.socket_path(),
                result.exit_code(),
                result.stdout(),
                result.stderr(),
                late,
            )
            .map_err(Into::into)
    }

    /// Inspect the session identified by a creation receipt from either driver.
    ///
    /// The returned handle uses the receipt's daemon generation for later
    /// commands, including cleanup. Reading the receipt's three IDs costs no I/O.
    ///
    /// # Errors
    ///
    /// Fails on a different endpoint, a replaced daemon or a session that
    /// disappeared before its snapshot could be read.
    pub async fn inspect_session(
        &self,
        receipt: &libtmux_core::CreatedSession,
    ) -> Result<Session, Error> {
        self.core
            .require_same_server(receipt.pane().server_identity(), "inspect-session")?;
        let id = receipt.session();
        self.bound_to(receipt.generation())?
            .session_by_id(&id)
            .await?
            .ok_or_else(|| Error::ObjectGone {
                kind: crate::ObjectKind::Session,
                id: id.to_string(),
            })
    }

    /// Kill the session recorded in a receipt, including one returned by a failed hook.
    ///
    /// # Errors
    ///
    /// Refuses another endpoint or daemon generation. Returns an error if the
    /// session disappeared or tmux refused the command.
    pub async fn kill_created_session(
        &self,
        receipt: &libtmux_core::CreatedSession,
    ) -> Result<(), Error> {
        self.core
            .require_same_server(receipt.pane().server_identity(), "kill-session")?;
        let server = self.bound_to(receipt.generation())?;
        let result = server
            .core
            .execute_chain(receipt.kill_session_command())
            .await?;
        if result.success() {
            Ok(())
        } else {
            Err(Error::from_refused_result(
                "kill-session",
                &result,
                Some(OsStr::new(receipt.session().as_ref())),
            ))
        }
    }

    fn for_peer(&self, peer: Option<&Core>, operation: &'static str) -> Result<Self, Error> {
        let Some(peer) = peer else {
            return Ok(self.clone());
        };
        self.core
            .require_same_server(peer.configuration().identity(), operation)?;
        let generation = peer
            .expected_generation()
            .ok_or(libtmux_core::Error::InvalidKey)?;
        self.bound_to(generation)
    }

    pub(crate) fn bound_to(&self, generation: ServerGeneration) -> Result<Self, Error> {
        Ok(Self::from_core(Arc::new(self.core.bound_to(generation)?)))
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
    /// Returns an error if tmux cannot be run or refuses the command. A
    /// daemon already absent at the initial endpoint check counts as stopped.
    pub async fn kill(&self) -> Result<(), Error> {
        let result = self.core.kill_server().await?;
        if result.success() {
            Ok(())
        } else {
            Err(Error::from_refused_result("kill-server", &result, None))
        }
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
        command = command.arg("--");

        listing::mutate(&self.core, "set-buffer", command.sensitive_arg(data.into())).await
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
    pub async fn load_buffer(
        &self,
        name: Option<&str>,
        path: impl AsRef<Path>,
    ) -> Result<(), Error> {
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
        .await
    }

    /// Write a paste buffer to a file, letting tmux do the writing.
    ///
    /// The counterpart to [`Self::load_buffer`], and the same reasoning: the
    /// bytes never cross a command line, so a buffer larger than an argument
    /// can hold still round-trips. tmux writes as the user running the
    /// server.
    ///
    /// A buffer can hold a pane's text, a command line or a secret, and tmux
    /// creates a missing file with the mode its own umask leaves, `0644` under
    /// the usual `022`. So a missing file is created first, by this process,
    /// with mode `0600`, and tmux then writes into a file only this user can
    /// read. A file that already exists keeps its mode, and tmux truncates it
    /// as before. When tmux refuses the write, a file this call created is
    /// removed. After a timeout or a lost connection it is left, because tmux
    /// may still write it.
    ///
    /// This process makes the file, so a path it cannot create, such as one in
    /// a directory only the server's user can write, is left for tmux to
    /// create with tmux's mode, or to refuse. The same holds for a server on
    /// another host. `-` is tmux's name for standard output, and makes no
    /// file.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is missing or tmux cannot write the
    /// path.
    pub async fn save_buffer(
        &self,
        name: Option<&str>,
        path: impl AsRef<Path>,
    ) -> Result<(), Error> {
        let path = path.as_ref();
        let created = create_private_file(path);
        let mut command = Command::new("save-buffer");
        if let Some(name) = name {
            command = command.arg("-b").arg(OsString::from(name));
        }

        let outcome = listing::mutate(
            &self.core,
            "save-buffer",
            command.arg("--").arg(path.as_os_str().to_os_string()),
        )
        .await;
        if created {
            if let Err(error) = &outcome {
                if tmux_refused_before_writing(error) {
                    // Best effort: the file is empty and ours, and the error
                    // that matters is tmux's.
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        outcome
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
    pub async fn buffer(&self, name: impl AsRef<[u8]>) -> Result<Option<Vec<u8>>, Error> {
        let result = self
            .cmd(
                Command::new("show-buffer")
                    .arg("-b")
                    .arg(OsString::from_vec(name.as_ref().to_vec())),
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
    /// A name is [`TmuxText`] because whoever made the buffer chose it: tmux
    /// before 3.7 stores any bytes, a newline included. Each name is read
    /// framed, so one holding a newline or `=` is one name, and
    /// [`Self::buffer`] and [`Self::delete_buffer`] take it back unchanged.
    /// A configured `after-list-buffers` hook shares the listing's stdout.
    /// Its output can be decoded as an additional name if it has the same row format.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the listing, and
    /// [`Error::DecodeListing`] when its output does not decode.
    pub async fn buffer_names(&self) -> Result<Vec<TmuxText>, Error> {
        let dialect = TransportDialect::for_version(&self.core.format_version().await?);
        let result = self
            .cmd(
                Command::new("list-buffers")
                    .arg("-F")
                    .arg("#{q:buffer_name}="),
            )
            .await?;
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
    pub async fn delete_buffer(&self, name: impl AsRef<[u8]>) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "delete-buffer",
            Command::new("delete-buffer")
                .arg("-b")
                .arg(OsString::from_vec(name.as_ref().to_vec())),
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
                .arg("--")
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
                .arg("--")
                .arg(OsString::from(key)),
        )
        .await
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
    pub async fn format(&self, pane: Option<&Pane>, format: &str) -> Result<TmuxText, Error> {
        let server = self.for_peer(pane.map(|pane| pane.core.as_ref()), "display-message")?;
        let mut command = Command::new("display-message").arg("-p");
        if let Some(pane) = pane {
            command = command.arg("-t").arg(pane.id().to_string());
        }

        let result = server
            .cmd(command.arg("--").arg(OsString::from(format)))
            .await?;
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
    pub(crate) async fn require(
        &self,
        capability: &'static str,
        needs: ReleaseVersion,
    ) -> Result<(), Error> {
        self.core
            .format_version()
            .await?
            .require(capability, needs)
            .map_err(Into::into)
    }

    /// The entries tmux remembers for one kind of prompt.
    ///
    /// tmux keeps a separate history per prompt kind, so the kind is required
    /// rather than defaulted: asking without one returns every kind at once,
    /// which is a different question.
    ///
    /// # Errors
    ///
    /// Returns an error when the daemon is unsupported, cannot be reached, or
    /// refuses the listing.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux_tokio::PromptKind;
    ///
    /// let guard = libtmux_tokio::test::TestServer::new().await?;
    /// let server = guard.server();
    ///
    /// // A fresh server has answered no prompts.
    /// assert!(server.prompt_history(PromptKind::Command).await?.is_empty());
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn prompt_history(&self, kind: PromptKind) -> Result<Vec<TmuxText>, Error> {
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
    /// Returns an error when the daemon is unsupported, cannot be reached, or
    /// refuses the command.
    pub async fn clear_prompt_history(&self) -> Result<(), Error> {
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
    /// Returns an error when the daemon is unsupported, cannot be reached, or
    /// refuses the listing.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux_tokio::test::TestServer::new().await?;
    ///
    /// // Whoever started the server owns it and may act.
    /// let rules = guard.server().access_rules().await?;
    /// assert_eq!(rules.len(), 1);
    /// assert_eq!(rules[0].mode(), libtmux_tokio::AccessMode::Write);
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn access_rules(&self) -> Result<Vec<AccessRule>, Error> {
        let result = self.cmd(Command::new("server-access").arg("-l")).await?;
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
    /// Returns an error when the daemon is unsupported or refuses the change,
    /// which it does for the user who owns the server.
    pub async fn grant_access(&self, user: &str, mode: AccessMode) -> Result<(), Error> {
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
        .await
    }

    /// Stop a user attaching to this server.
    ///
    /// # Errors
    ///
    /// Returns an error when the daemon is unsupported or refuses the change,
    /// which it does for the user who owns the server.
    pub async fn revoke_access(&self, user: &str) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "server-access",
            Command::new("server-access")
                .arg("-d")
                .arg("--")
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
            Command::new("source-file")
                .arg("--")
                .arg(path.into().into_os_string()),
        )
        .await
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
    /// # Cancel safety
    ///
    /// While the runtime is alive, a supervisor attempts cleanup for an
    /// acknowledged session even when the caller drops the future or panics.
    /// An interrupted creation without a receipt may leave an unidentified
    /// session. The `tracing` feature records unobserved cleanup failures.
    pub async fn with_session<T, E>(
        &self,
        options: impl Into<NewSession>,
        operation: impl AsyncFnOnce(&Session) -> Result<T, E>,
    ) -> Result<T, crate::ScopeError<T, E>> {
        let server = self.clone();
        let options = options.into();
        scoped::run(
            "with-session",
            async move {
                let inspected = match server.new_session(options).await {
                    Ok(receipt) => server
                        .inspect_session(&receipt)
                        .await
                        .map_err(|source| source.after_creation(receipt)),
                    Err(error) => Err(error),
                };
                let Err(primary) = inspected else {
                    return inspected;
                };
                let Some(receipt) = primary.creation_receipt() else {
                    return Err(primary);
                };
                match server.kill_created_session(receipt).await {
                    Ok(()) => Err(primary),
                    Err(cleanup) => Err(Error::OperationAndCleanup {
                        primary: Box::new(primary),
                        cleanup: Box::new(cleanup),
                    }),
                }
            },
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
    ///
    /// # Cancel safety
    ///
    /// The effect may have happened, and may still be happening. The shell
    /// command runs under the tmux server, not this dispatch, so a dropped
    /// call stops waiting for it without stopping it, and its output is lost.
    pub async fn run_shell(&self, command: impl Into<OsString>) -> Result<Vec<TmuxText>, Error> {
        let result = self
            .cmd(
                Command::new("run-shell")
                    .refusing_interval(
                        "run-shell output",
                        RUN_SHELL_OUTPUT_IN_COPY_MODE.broken_in,
                        RUN_SHELL_OUTPUT_IN_COPY_MODE.fixed_in,
                    )
                    .arg("--")
                    .sensitive_arg(command.into()),
            )
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
                .arg("--")
                .sensitive_arg(command.into()),
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

    /// Which tmux daemon is answering on this endpoint.
    ///
    /// tmux can reuse object IDs after a restart on the same socket. Handles
    /// obtained through discovery carry the generation. Their queue guard
    /// checks it again before a command uses the ID. The PID and start time
    /// cannot distinguish daemons that reuse a PID within the same second.
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
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux_tokio::test::TestServer::new().await?;
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
        let deadline = self.core.operation_deadline();
        let probe =
            libtmux_core::OrdinaryGenerationProbe::new(crate::internal::operation::nonce()?);
        let (result, late) = self
            .core
            .execute_operation(
                probe.arguments(),
                Command::new("run-shell").summary(),
                crate::internal::operation::OperationContext::observation(deadline),
            )
            .await?;
        if !late {
            if let Some(error @ Error::ServerGone { .. }) = result.refusal_for("server generation")
            {
                return Err(error);
            }
        }
        probe
            .decode(result.exit_code(), result.stdout(), result.stderr(), late)
            .map_err(Into::into)
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
    pub(crate) fn from_executor_for_test(
        executor: Arc<dyn crate::internal::executor::Executor>,
    ) -> Self {
        Self {
            core: Arc::new(Core::from_executor_for_test(executor)),
        }
    }

    /// Use an attached control connection for commands with bounded, unambiguous replies.
    ///
    /// Numeric projections and commands such as `send-keys` can share the connection.
    /// A command that waits, prints arbitrary text or cannot be encoded as a control
    /// line runs through an ordinary tmux client. A chain uses one route for all of
    /// its commands. Route selection happens before submission. A failed command
    /// is never submitted again on the other route.
    ///
    /// Ordinary commands use the configured executable and tmux's ordinary client
    /// context. Pass an explicit target when the selected session or pane matters.
    /// The daemon generation checked at attach applies on both routes. Standalone
    /// executable diagnostics such as `-V` run without contacting that daemon.
    ///
    /// The source and its routed servers share ordinary request IDs, admission
    /// and shutdown. Calling [`Server::shutdown`] on the routed server drains
    /// that pool and ends dispatch through the view. The caller can still use
    /// the independently held sender until the connection owner closes it.
    ///
    /// The sender's [reply timeout] bounds control replies. The server's
    /// [default timeout] bounds ordinary dispatch. A slow event consumer can
    /// receive a gap while the connection continues processing command replies.
    ///
    /// # Errors
    ///
    /// Returns an error when the sender belongs to another endpoint or the
    /// server expects another daemon generation. Also returns an error if the
    /// configured executable cannot report its version.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux_tokio::control::ControlMode;
    /// use libtmux_tokio::test::TestServer;
    ///
    /// let guard = TestServer::new().await?;
    /// # let version = guard.server().capabilities().await?.tmux_version().clone();
    /// # let eligible = version.has_behavior(&libtmux_tokio::since::CONTROL_ACQUISITION);
    /// # if !eligible {
    /// #     guard.shutdown().await?;
    /// #     return Ok(());
    /// # }
    /// let session = guard.session("routed").await?;
    ///
    /// let (sender, events) = ControlMode::attach(guard.server(), session.id())
    ///     .await?
    ///     .split();
    /// let routed = guard.server().over_control_mode(&sender).await?;
    ///
    /// assert_eq!(routed.sessions().await?.len(), 1);
    ///
    /// let pane = routed.panes().await?.remove(0);
    /// pane.send_line("true").await?;
    ///
    /// events.shutdown().await?;
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [reply timeout]: crate::control::ControlSender::reply_timeout
    /// [default timeout]: Server::default_timeout
    #[cfg(feature = "control-mode")]
    pub async fn over_control_mode(
        &self,
        sender: &crate::control::ControlSender,
    ) -> Result<Self, Error> {
        self.core
            .require_same_server(sender.identity(), "over_control_mode")?;
        if let Some(expected) = self.core.expected_generation() {
            if expected != sender.generation() {
                return Err(Error::ServerGenerationChanged {
                    expected,
                    found: sender.generation(),
                });
            }
        }
        let capabilities = self.core.capabilities().await?.clone();

        Ok(Self {
            core: Arc::new(self.core.over_control_mode(sender.clone(), capabilities)?),
        })
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

/// Create `path` with mode `0600` unless something is already there, and say
/// whether this call made it.
///
/// `O_EXCL` means a file, link or directory that exists is never followed or
/// opened, so a path an attacker prepared is left to tmux as before. Any other
/// failure, such as a missing directory, is tmux's to report.
fn create_private_file(path: &Path) -> bool {
    use std::os::unix::fs::OpenOptionsExt as _;

    // tmux's name for standard output, not a file.
    if path.as_os_str() == "-" {
        return false;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .is_ok()
}

/// Whether an error says tmux did not write, so a file made for it is safe to
/// remove. A timeout or a lost connection says nothing about what tmux did.
fn tmux_refused_before_writing(error: &Error) -> bool {
    use crate::ErrorKind;

    matches!(
        error.kind(),
        ErrorKind::Refused
            | ErrorKind::ObjectGone
            | ErrorKind::ServerGone
            | ErrorKind::Unreachable
            | ErrorKind::UnsupportedVersion
            | ErrorKind::InvalidInput
    )
}

#[cfg(test)]
mod tests {

    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use tokio::sync::{Notify, watch};

    use super::{AccessMode, NewSession, Principal, Server, parse_access_rule};
    use crate::command::{CommandRequest, CommandResult, ProcessStatus};
    use crate::formats::{DecoderKind, FormatDescriptor, FormatPlan, ListProfile};
    use crate::internal::executor::{DispatchFuture, Executor, ShutdownFuture};
    use crate::{Error, ErrorKind, TmuxVersion};

    struct RefusingExecutor;

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
                if matches!(*byte, b'\\' | b'%' | b'=') {
                    stdout.push(b'\\');
                }
                stdout.push(*byte);
            }
            stdout.push(b'=');
        }
        stdout.push(b'\n');
        stdout
    }

    impl Executor for RefusingExecutor {
        fn execute(&self, request: CommandRequest) -> DispatchFuture {
            DispatchFuture::new(async move {
                assert_eq!(request.summary().sensitive_argument_count(), 1);
                Ok(CommandResult::new(
                    request.request_id(),
                    request.summary().clone(),
                    ProcessStatus::from_exit_status(ExitStatus::from_raw(1 << 8)),
                    Vec::new(),
                    b"sentinel-run-shell-output\n".to_vec(),
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
    async fn explicit_shutdown_reports_pending_cleanup_and_can_be_awaited_again() {
        let (release, blocked) = watch::channel(false);
        let executor = Arc::new(BlockingShutdownExecutor {
            closed: AtomicBool::new(false),
            shutdown_started: Notify::new(),
            release: blocked,
        });
        let server = Server::from_executor_for_test(executor.clone());
        let error = tokio::time::timeout(
            libtmux_core::supervision::CLEANUP_ALLOWANCE * 3,
            server.shutdown(),
        )
        .await
        .expect("shutdown reports while cleanup is still pending")
        .expect_err("cleanup cannot finish before the supervisor is released");
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert!(matches!(error, Error::ShutdownPending));
        assert!(executor.closed.load(Ordering::SeqCst));
        release.send_replace(true);
        server.shutdown().await.expect("cleanup finished");
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn kill_distinguishes_a_refused_generation_from_a_stopped_endpoint() {
        let fixture = crate::test::TestServer::new()
            .await
            .expect("private daemon");
        let server = fixture.server();
        let generation = server.generation().await.expect("running generation");
        let stale = server
            .bound_to(crate::ServerGeneration::from_raw(
                generation.pid(),
                generation.unix_start_time().wrapping_sub(1),
            ))
            .expect("bind the previous observation");

        let result = stale.kill().await;
        assert_eq!(
            server
                .generation()
                .await
                .expect("daemons survive stale kills"),
            generation
        );
        let endpoint = Server::builder()
            .socket_path(fixture.socket_path())
            .build()
            .expect("same private endpoint");
        fixture.shutdown().await.expect("close private fixture");
        result.expect_err("tmux refused the stale generation");
        endpoint
            .kill()
            .await
            .expect("the endpoint is already stopped");
    }

    #[tokio::test]
    async fn run_shell_failure_withholds_sensitive_output() {
        let server = Server::from_executor_for_test(Arc::new(RefusingExecutor));

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
        let server = Server::from_executor_for_test(executor.clone())
            .bound_to(crate::ServerGeneration::from_raw(42, 1))
            .expect("the fixture daemon generation");
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
        let server = Server::from_executor_for_test(executor.clone())
            .bound_to(crate::ServerGeneration::from_raw(42, 1))
            .expect("the fixture daemon generation");
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
        let options = NewSession::new("work").command(secret);
        assert!(!format!("{options:?}").contains(secret));

        let summary = crate::Command::new_session(&options, "#{session_id}").summary();
        assert_eq!(summary.sensitive_argument_count(), 1);
        assert!(!summary.to_string().contains(secret));
    }

    #[test]
    fn access_rule_lines_decode_both_the_legacy_and_the_group_acl_grammar() {
        // A release before group ACLs prints no principal marker at all, and
        // every row it can print names a user.
        let legacy_write = parse_access_rule("alice (W)").expect("legacy write row parses");
        assert_eq!(legacy_write.name(), "alice");
        assert_eq!(legacy_write.principal(), Principal::User);
        assert_eq!(legacy_write.mode(), AccessMode::Write);

        let legacy_read = parse_access_rule("bob (R)").expect("legacy read-only row parses");
        assert_eq!(legacy_read.principal(), Principal::User);
        assert_eq!(legacy_read.mode(), AccessMode::ReadOnly);

        // A release with group ACLs marks every row, owner included, so the
        // decoder must not assume the legacy shape just because a row is
        // read-write.
        let user_row = parse_access_rule("carol (U,W)").expect("compound user row parses");
        assert_eq!(user_row.name(), "carol");
        assert_eq!(user_row.principal(), Principal::User);
        assert_eq!(user_row.mode(), AccessMode::Write);

        let group_row = parse_access_rule("admins (G,R)").expect("compound group row parses");
        assert_eq!(group_row.name(), "admins");
        assert_eq!(group_row.principal(), Principal::Group);
        assert_eq!(group_row.mode(), AccessMode::ReadOnly);
    }

    /// An unrecognized `server-access -l` marker is reported through
    /// `Error::UnreadableAccessRule` rather than silently dropped: a
    /// listing with only such a row used to come back empty.
    #[test]
    fn an_unrecognized_access_rule_marker_is_reported_not_dropped() {
        let bad_principal = parse_access_rule("mallory (X,W)")
            .expect_err("an unknown principal letter is rejected");
        assert!(
            matches!(&bad_principal, Error::UnreadableAccessRule { marker } if marker == "(X,W)"),
            "the refusal names the marker rather than silently dropping the row: \
             {bad_principal:?}",
        );
        assert_eq!(bad_principal.kind(), ErrorKind::Decode);

        let bad_mode =
            parse_access_rule("mallory (U,X)").expect_err("an unknown mode letter is rejected");
        assert!(matches!(
            &bad_mode,
            Error::UnreadableAccessRule { marker } if marker == "(U,X)"
        ));

        let no_marker =
            parse_access_rule("mallory").expect_err("a line with no trailing marker is rejected");
        assert!(matches!(no_marker, Error::UnreadableAccessRule { .. }));
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
/// # async fn example(server: &libtmux_tokio::Server) -> Result<(), libtmux_tokio::Error> {
/// use libtmux_tokio::Chooser;
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
