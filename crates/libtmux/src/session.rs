//! Session handles and their snapshot getters.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::formats::TmuxText;
use crate::internal::core::Core;
use crate::internal::environment;
use crate::internal::listing::{self, Pushdown as _};
use crate::internal::options;
use crate::pane::Pane;
#[cfg(feature = "query")]
use crate::query::Filterable;
#[cfg(feature = "query")]
use crate::snapshot::SessionFields;
use crate::snapshot::SessionInfo;
use crate::target::{ServerIdentity, SessionId};
use crate::window::Window;
use crate::{Command, CommandResult, Error, IndexedHooks, ObjectKind, OptionValue, ReplaceMode};

/// What a session's environment holds for one name.
///
/// tmux keeps two different things under a name: a value, and a mark saying a
/// process started here must *not* inherit the name at all. They are not the
/// same as absence, and they are not the same as each other.
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
/// let session = guard.server().new_session("env").await?;
///
/// session.set_environment("EDITOR", "hx").await?;
/// assert!(matches!(
///     session.environment("EDITOR").await?,
///     Some(EnvironmentEntry::Set(value)) if value.as_bytes() == b"hx",
/// ));
///
/// // Hiding a name is not unsetting it: a process started here is handed the
/// // name *absent*, which tmux still records and reports.
/// session.hide_environment("EDITOR").await?;
/// assert_eq!(
///     session.environment("EDITOR").await?,
///     Some(EnvironmentEntry::Removed),
/// );
///
/// // Never set at all is the third thing, and the only one that is `None`.
/// assert_eq!(session.environment("NEVER_SET").await?, None);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentEntry {
    /// tmux holds this value, exactly as stored.
    Set(TmuxText),
    /// tmux will remove this name from the environment it hands out.
    Removed,
}

/// One tmux session, together with the snapshot it was discovered with.
///
/// A `Session` is cheap to clone and shares its connection with the [`Server`]
/// that produced it, but owns its snapshot outright. Cloning therefore does
/// not share observed state: refreshing one clone leaves the others as they
/// were.
///
/// Getters are synchronous because they read the owned snapshot. Anything that
/// consults tmux is `async`.
///
/// [`Server`]: crate::Server
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("work").await?;
///
/// // The name is read from the snapshot this handle owns, so it costs
/// // nothing; asking tmux for the windows is `async` because it does.
/// assert_eq!(session.name().to_string_lossy(), "work");
/// assert_eq!(session.windows().await?.len(), 1);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Session {
    core: Arc<Core>,
    info: SessionInfo,
}

impl Session {
    /// Build a handle from a hydrated snapshot.
    pub(crate) const fn new(core: Arc<Core>, info: SessionInfo) -> Self {
        Self { core, info }
    }

    /// Find the session this process is running in.
    ///
    /// Resolved through the pane, because `TMUX_PANE` names exactly one pane
    /// while a server's `TMUX` value names the session that was current when
    /// the client attached, which may since have changed.
    ///
    /// `Ok(None)` means tmux no longer has that pane.
    ///
    /// # Errors
    ///
    /// Returns an error when `TMUX_PANE` is absent or is not a pane ID, or
    /// when the listing fails.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
    /// use libtmux::{Pane, Session};
    ///
    /// let session = server.new_session("locating-session").await?;
    /// let pane = session.panes().await?.remove(0);
    ///
    /// // Standing in for the environment tmux gives a process it starts.
    /// let found = Session::from_env_value(server, Some(pane.id().as_ref())).await?;
    ///
    /// assert_eq!(found.expect("the session exists").id(), session.id());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_env(server: &crate::Server) -> Result<Option<Self>, Error> {
        Self::from_env_value(server, std::env::var_os("TMUX_PANE")).await
    }

    /// Find a session from an explicit `TMUX_PANE` value.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is absent or is not a pane ID, or when
    /// the listing fails.
    pub async fn from_env_value(
        server: &crate::Server,
        value: Option<impl AsRef<OsStr>>,
    ) -> Result<Option<Self>, Error> {
        let Some(pane) = Pane::from_env_value(server, value).await? else {
            return Ok(None);
        };

        server.session_by_id(pane.session_id()).await
    }

    /// Return the tmux session identity.
    ///
    /// This is the `$`-prefixed id tmux assigns, which is stable across
    /// renames. It is always present.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.session("work").await?;
    ///
    /// assert!(session.id().to_string().starts_with('$'));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub const fn id(&self) -> &SessionId {
        self.info.session_id()
    }

    /// Return the session name.
    ///
    /// Names are byte-preserving: tmux permits bytes that are not valid UTF-8,
    /// so this is [`TmuxText`] rather than `str`.
    #[must_use]
    pub fn name(&self) -> &TmuxText {
        self.info.session_name()
    }

    /// Return the session's working directory.
    #[must_use]
    pub fn path(&self) -> &TmuxText {
        self.info.session_path()
    }

    /// Return how many windows the session contains.
    #[must_use]
    pub fn window_count(&self) -> u32 {
        *self.info.session_windows()
    }

    /// Return how many clients are attached to the session.
    #[must_use]
    pub fn attached_client_count(&self) -> u32 {
        *self.info.session_attached()
    }

    /// Report whether any client is attached.
    ///
    /// A session tmux has never reported on is treated as unattached.
    #[must_use]
    pub fn is_attached(&self) -> bool {
        self.attached_client_count() > 0
    }

    /// Return when the session was created, as a Unix timestamp.
    #[must_use]
    pub fn created(&self) -> i64 {
        *self.info.session_created()
    }

    /// Return when a client last attached, as a Unix timestamp.
    ///
    /// This is `None` for a session that has never been attached, which is the
    /// ordinary state for one started with `new-session -d`.
    #[must_use]
    pub fn last_attached(&self) -> Option<i64> {
        self.info.session_last_attached().copied().available()
    }

    /// Return the identity of the server this session belongs to.
    #[must_use]
    pub(crate) fn server_identity(&self) -> &ServerIdentity {
        self.core.configuration().identity()
    }

    /// List the windows linked into this session, in tmux's own order.
    ///
    /// This is the lenient form; use [`Session::windows`] when the reason
    /// for an empty result matters.
    pub async fn windows_or_empty(&self) -> Vec<Window> {
        self.windows().await.unwrap_or_default()
    }

    /// List the windows linked into this session, preserving any failure.
    ///
    /// A window linked into other sessions appears here once, under this
    /// session's own index for it.
    ///
    /// # Errors
    ///
    /// Returns an error when the list command cannot run, or when its output
    /// cannot be decoded into snapshots.
    pub async fn windows(&self) -> Result<Vec<Window>, Error> {
        let target = self.id().to_string();
        let projections =
            listing::windows(&self.core, listing::Scope::Target(&target), None).await?;

        Ok(projections
            .into_iter()
            .map(|projection| Window::new(Arc::clone(&self.core), projection))
            .collect())
    }

    /// The windows under this session that a matcher accepts.
    ///
    /// Empty when the listing fails, which suits a status line. Use
    /// [`Self::search_windows`] when the difference matters.
    ///
    /// Filtering happens here rather than in tmux. A [`crate::query::FilterExpr`]
    /// is built to stay compilable to a tmux `-f` predicate, so pushing one
    /// down later would change what this costs and not what it answers.
    #[cfg(feature = "query")]
    #[must_use]
    pub async fn search_windows_or_empty<M: crate::query::Matcher<Window>>(
        &self,
        matcher: M,
    ) -> Vec<Window> {
        self.search_windows(matcher).await.unwrap_or_default()
    }

    /// The windows under this session that a matcher accepts, reporting why
    /// if the listing fails.
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
    /// use libtmux::query::Filterable as _;
    ///
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("searched").await?;
    /// session.new_window("build").await?;
    ///
    /// let fields = libtmux::Window::filter_fields();
    /// let found = session.search_windows(&fields.window_name.eq("build")).await?;
    /// assert_eq!(found.len(), 1);
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "query")]
    pub async fn search_windows<M: crate::query::Matcher<Window>>(
        &self,
        matcher: M,
    ) -> Result<Vec<Window>, Error> {
        use crate::query::QueryIteratorExt as _;

        let all = self.windows().await?;
        Ok(all.iter().matching(matcher).cloned().collect())
    }

    /// Move to the next window, and return it.
    ///
    /// `None` when there is no next window, which a session holding one
    /// window never has. That is an ordinary state rather than a failure, so
    /// it is reported as absence; an error means tmux could not be reached or
    /// refused the move for some other reason.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the move.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("moving").await?;
    ///
    /// // One window, so there is nowhere to go and nothing broke.
    /// assert!(session.next_window().await?.is_none());
    ///
    /// session.new_window("second").await?;
    /// assert!(session.next_window().await?.is_some());
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn next_window(&self) -> Result<Option<Window>, Error> {
        self.step("next-window").await
    }

    /// Move to the previous window, and return it.
    ///
    /// `None` when there is none; see [`Self::next_window`].
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the move.
    pub async fn previous_window(&self) -> Result<Option<Window>, Error> {
        self.step("previous-window").await
    }

    /// Move to the window that was active before this one, and return it.
    ///
    /// `None` when nothing else has been active yet; see
    /// [`Self::next_window`].
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the move.
    pub async fn last_window(&self) -> Result<Option<Window>, Error> {
        self.step("last-window").await
    }

    /// Run one window move and report what became active.
    async fn step(&self, command: &'static str) -> Result<Option<Window>, Error> {
        let target = self.id().to_string();
        let result = self
            .core
            .execute(Command::new(command).arg("-t").arg(&target))
            .await?;
        if !result.success() {
            let stderr = result.stderr_lossy();
            if crate::error::NO_SUCH_NEIGHBOUR.contains(&stderr.trim_end()) {
                return Ok(None);
            }
            return Err(Error::refused(
                command,
                result.exit_code(),
                stderr.into_owned(),
                Some(OsStr::new(&target)),
            ));
        }

        self.active_window().await
    }
    /// Return the session's active window.
    ///
    /// # Errors
    ///
    /// Returns an error when the window listing fails. A session always has an
    /// active window, so `Ok(None)` means the session disappeared between the
    /// snapshot and this call.
    pub async fn active_window(&self) -> Result<Option<Window>, Error> {
        Ok(self.windows().await?.into_iter().find(Window::is_active))
    }

    /// List every pane in this session, in tmux's own order.
    ///
    /// This is the lenient form; use [`Session::panes`] when the reason
    /// for an empty result matters.
    pub async fn panes_or_empty(&self) -> Vec<Pane> {
        self.panes().await.unwrap_or_default()
    }

    /// List every pane in this session, preserving any failure.
    ///
    /// # Errors
    ///
    /// Returns an error when the list command cannot run, or when its output
    /// cannot be decoded into snapshots.
    pub async fn panes(&self) -> Result<Vec<Pane>, Error> {
        let target = self.id().to_string();
        let projections =
            listing::panes(&self.core, listing::Scope::SessionTarget(&target), None).await?;

        Ok(projections
            .into_iter()
            .map(|projection| Pane::new(Arc::clone(&self.core), projection))
            .collect())
    }
    /// Replace this handle's snapshot with the session's current state.
    ///
    /// Only the receiver changes. Clones keep the snapshot they were taken
    /// with, because each handle owns its own.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ObjectGone`] when the session no longer exists, or a
    /// listing error when tmux could not be read.
    pub async fn refresh(&mut self) -> Result<&mut Self, Error> {
        let info = listing::sessions(&self.core, None)
            .await?
            .into_iter()
            .find(|info| info.session_id() == self.id())
            .ok_or_else(|| Error::ObjectGone {
                kind: ObjectKind::Session,
                id: self.id().to_string(),
            })?;

        self.info = info;
        Ok(self)
    }

    /// Return a new handle holding the session's current state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ObjectGone`] when the session no longer exists, or a
    /// listing error when tmux could not be read.
    pub async fn refreshed(&self) -> Result<Self, Error> {
        let mut refreshed = self.clone();
        refreshed.refresh().await?;
        Ok(refreshed)
    }

    /// Create a window in this session and return it.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command or its output cannot be
    /// decoded.
    /// tmux expands the name as a format before it checks it, so `#(command)`
    /// in one runs a shell command. See [the crate documentation][crate#a-name-reaches-tmux-as-a-format]
    /// before passing text a caller supplied.
    pub async fn new_window(&self, options: impl Into<NewWindowOptions>) -> Result<Window, Error> {
        let options = options.into();
        let session = self.id().to_string();
        let projection =
            listing::create_window(&self.core, |format| options.into_command(&session, format))
                .await?;

        Ok(Window::new(Arc::clone(&self.core), projection))
    }

    /// Rename the session and update this handle.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the name, which includes one that
    /// is empty or already taken.
    /// tmux expands the name as a format before it checks it, so `#(command)`
    /// in one runs a shell command. See [the crate documentation][crate#a-name-reaches-tmux-as-a-format]
    /// before passing text a caller supplied.
    pub async fn rename(&mut self, name: impl Into<OsString>) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "rename-session",
            Command::new("rename-session")
                .arg("-t")
                .arg(self.id().to_string())
                .arg(name.into()),
        )
        .await?;

        self.refresh().await?;
        Ok(self)
    }

    /// Kill the session.
    ///
    /// This consumes the handle: every other handle to the same session is now
    /// stale, and refreshing one reports [`Error::ObjectGone`].
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn kill(self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "kill-session",
            Command::new("kill-session")
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await
    }

    /// Read one option's exact stored value.
    ///
    /// A user option, whose name begins with `@`, exists only while it is
    /// set, so an unset one reports `None`. A built-in option always exists,
    /// so an unset one also reports `None`. An unrecognized built-in name is
    /// an error.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn get_option(&self, name: &str) -> Result<Option<TmuxText>, Error> {
        let target = self.id().to_string();
        options::get(&self.core, options::Scope::Session(&target), name).await
    }

    /// List the option names set at this session's scope.
    ///
    /// Values are not included: tmux renders them for display with three
    /// different quoting styles, so re-parsing them would be guesswork. Read
    /// each value with [`Self::get_option`], which returns exact bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the listing.
    pub async fn option_names(&self) -> Result<Vec<String>, Error> {
        let target = self.id().to_string();
        options::names(&self.core, options::Scope::Session(&target)).await
    }

    /// Read every option set at this session, decoded by its declared kind.
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
    /// let session = guard.server().new_session("opts").await?;
    ///
    /// session.set_option("status-left-length", "30").await?;
    /// let options = session.options().await?;
    /// assert!(options.contains_key("status-left-length"));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn options(&self) -> Result<BTreeMap<String, OptionValue>, Error> {
        let target = self.id().to_string();
        options::typed_all(&self.core, options::Scope::Session(&target)).await
    }

    /// Set one option.
    ///
    /// The value is marked sensitive, so it never reaches `Debug`, an error,
    /// or a tracing span.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    pub async fn set_option(&self, name: &str, value: impl Into<OsString>) -> Result<(), Error> {
        let target = self.id().to_string();
        options::set(
            &self.core,
            options::Scope::Session(&target),
            name,
            value,
            false,
        )
        .await
    }

    /// Append to one option rather than replacing it.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    pub async fn append_option(&self, name: &str, value: impl Into<OsString>) -> Result<(), Error> {
        let target = self.id().to_string();
        options::set(
            &self.core,
            options::Scope::Session(&target),
            name,
            value,
            true,
        )
        .await
    }

    /// Remove one option, restoring whatever it inherits.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
    pub async fn unset_option(&self, name: &str) -> Result<(), Error> {
        let target = self.id().to_string();
        options::unset(&self.core, options::Scope::Session(&target), name).await
    }

    /// Set one hook to a tmux command.
    ///
    /// Hooks live in the same option tables, so a hook is an array option and
    /// [`Self::get_option`] reads it under an indexed name such as
    /// `after-new-window[0]`.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the hook name or command.
    pub async fn set_hook(&self, name: &str, command: impl Into<OsString>) -> Result<(), Error> {
        let target = self.id().to_string();
        options::set_hook(&self.core, options::Scope::Session(&target), name, command).await
    }

    /// Remove one hook.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the hook name.
    pub async fn unset_hook(&self, name: &str) -> Result<(), Error> {
        let target = self.id().to_string();
        options::unset_hook(&self.core, options::Scope::Session(&target), name).await
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
    /// let session = guard.server().new_session("hooked").await?;
    ///
    /// let mut entries = BTreeMap::new();
    /// entries.insert(0, TmuxText::from(b"display-message first".to_vec()));
    /// entries.insert(3, TmuxText::from(b"display-message fourth".to_vec()));
    ///
    /// session
    ///     .set_hooks("alert-bell", &IndexedHooks::from(entries), ReplaceMode::Replace)
    ///     .await?;
    ///
    /// let written = session.hook("alert-bell").await?.expect("the hook is set");
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
        let target = self.id().to_string();
        options::set_hooks(
            &self.core,
            options::Scope::Session(&target),
            name,
            hooks,
            replace,
        )
        .await
    }

    /// Run a raw tmux command against this session.
    ///
    /// The escape hatch for anything this crate does not wrap. `-t` is placed
    /// after the subcommand for you, because tmux stops reading flags at the
    /// first positional: a target appended after one is taken as text, and the
    /// command then succeeds having acted on something else.
    ///
    /// A non-zero exit status comes back in the [`CommandResult`] rather than
    /// as an error, the same as [`crate::Server::cmd`].
    ///
    /// # Errors
    ///
    /// Returns an error when the process cannot be started, captured, or
    /// awaited.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("raw").await?;
    ///
    /// let result = session
    ///     .cmd(libtmux::Command::new("display-message").arg("-p").arg("#{session_name}"))
    ///     .await?;
    /// assert_eq!(result.stdout_lossy().trim(), "raw");
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn cmd(&self, command: Command) -> Result<CommandResult, Error> {
        self.core
            .execute(command.targeting(self.id().to_string()))
            .await
    }

    /// Expand a tmux format string in this session's context.
    ///
    /// The reading half of tmux's `display-message`, separated from the
    /// showing half ([`Self::display`]) because they answer different
    /// questions: one returns text to the caller, the other puts text in
    /// front of a person.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the format.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("shown").await?;
    ///
    /// let expanded = session.format("#{session_name}").await?;
    /// assert_eq!(expanded.to_string_lossy(), "shown");
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn format(&self, template: &str) -> Result<TmuxText, Error> {
        let result = self
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg(OsString::from(template)),
            )
            .await?;
        if !result.success() {
            return Err(Error::refused(
                "display-message",
                result.exit_code(),
                result.stderr_lossy().into_owned(),
                Some(OsStr::new(&self.id().to_string())),
            ));
        }

        // `-p` ends its output with a newline that frames it rather than
        // belonging to the expansion.
        let stdout = result.stdout();
        Ok(TmuxText::from(
            stdout.strip_suffix(b"\n").unwrap_or(stdout).to_vec(),
        ))
    }

    /// Show a message on the clients viewing this session.
    ///
    /// The showing half of tmux's `display-message`. Nothing is returned
    /// because nothing is read: use [`Self::format`] to get text back.
    /// Succeeds when nobody is watching, having shown it to nobody.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the message.
    pub async fn display(&self, message: &str) -> Result<(), Error> {
        let result = self
            .cmd(Command::new("display-message").arg(OsString::from(message)))
            .await?;
        if result.success() {
            return Ok(());
        }

        Err(Error::refused(
            "display-message",
            result.exit_code(),
            result.stderr_lossy().into_owned(),
            Some(OsStr::new(&self.id().to_string())),
        ))
    }

    /// Lock every client attached to this session.
    ///
    /// tmux runs the `lock-command` to lock, so what a locked client shows is
    /// that command's business rather than this one's.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the lock.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("locked").await?;
    ///
    /// // Locking a session nobody is attached to is accepted and does nothing.
    /// session.lock().await?;
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn lock(&self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "lock-session",
            Command::new("lock-session")
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await
    }

    /// Detach every client attached to this session.
    ///
    /// Succeeds when no client was attached. tmux reports that as a failure,
    /// but the state this asks for -- nobody attached to this session -- is
    /// already true, and a caller that has to tell "detached them" from
    /// "there was nobody" can compare [`crate::Server::clients_or_empty`] before and
    /// after.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the detach for
    /// any other reason.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("busy").await?;
    ///
    /// // Nothing is attached in a headless fixture, so this is a no-op.
    /// session.detach_clients().await?;
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn detach_clients(&self) -> Result<(), Error> {
        let target = self.id().to_string();
        let result = self
            .core
            .execute(Command::new("detach-client").arg("-s").arg(&target))
            .await?;
        if result.success() {
            return Ok(());
        }

        // tmux says this when it has no client to act on, which is the state
        // the caller was asking for.
        let stderr = result.stderr_lossy();
        if stderr.trim_end() == crate::error::NO_CURRENT_CLIENT {
            return Ok(());
        }

        Err(Error::refused(
            "detach-client",
            result.exit_code(),
            stderr.into_owned(),
            Some(OsStr::new(&target)),
        ))
    }

    /// Read every hook set at this session.
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
    /// let session = guard.server().new_session("hooked").await?;
    ///
    /// session.set_hook("alert-bell", "display-message rang").await?;
    /// let hooks = session.hooks().await?;
    /// assert!(hooks.contains_key("alert-bell"));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn hooks(&self) -> Result<BTreeMap<String, IndexedHooks>, Error> {
        let target = self.id().to_string();
        options::hooks(&self.core, options::Scope::Session(&target)).await
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
    /// let session = guard.server().new_session("hooked").await?;
    ///
    /// assert!(session.hook("alert-bell").await?.is_none());
    /// session.set_hook("alert-bell", "display-message rang").await?;
    /// assert!(session.hook("alert-bell").await?.is_some());
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn hook(&self, name: &str) -> Result<Option<IndexedHooks>, Error> {
        let target = self.id().to_string();
        options::hook(&self.core, options::Scope::Session(&target), name).await
    }

    /// Create a window, run an operation with it, then kill it.
    ///
    /// The window is killed whether the operation succeeded or failed, so a
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
    /// window could not be created, or could not be killed after the
    /// operation succeeded.
    pub async fn with_window<T, E>(
        &self,
        options: impl Into<NewWindowOptions>,
        operation: impl AsyncFnOnce(&Window) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<Error>,
    {
        let created = self.new_window(options).await?;
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

    /// Set an environment variable for processes this session starts.
    ///
    /// Existing panes keep the environment they were started with; this
    /// affects what new panes inherit.
    ///
    /// The value is marked sensitive, since an environment carries tokens.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    pub async fn set_environment(
        &self,
        name: &str,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        environment::set(
            &self.core,
            environment::Scope::Session(self.id().as_ref()),
            name,
            value.into(),
        )
        .await
    }

    /// Read one variable from the session's environment.
    ///
    /// tmux keeps two different things under a name: a value, and a mark
    /// saying a process started here must not inherit the name at all.
    /// [`EnvironmentEntry`] keeps them apart, because collapsing both to
    /// absence would hide the second, which a caller sets deliberately with
    /// [`Self::hide_environment`].
    ///
    /// `None` means tmux holds nothing under the name.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached.
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
    /// let session = guard.server().new_session("read").await?;
    ///
    /// assert_eq!(session.environment("EDITOR").await?, None);
    ///
    /// session.set_environment("EDITOR", "hx").await?;
    /// assert!(matches!(
    ///     session.environment("EDITOR").await?,
    ///     Some(EnvironmentEntry::Set(value)) if value.as_bytes() == b"hx",
    /// ));
    ///
    /// session.hide_environment("EDITOR").await?;
    /// assert_eq!(
    ///     session.environment("EDITOR").await?,
    ///     Some(EnvironmentEntry::Removed),
    /// );
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn environment(&self, name: &str) -> Result<Option<EnvironmentEntry>, Error> {
        environment::get(
            &self.core,
            environment::Scope::Session(self.id().as_ref()),
            name,
        )
        .await
    }

    /// Read the session's whole environment.
    ///
    /// tmux distinguishes a variable it holds a value for from one it has
    /// marked for *removal*, so that a process started in the session does not
    /// inherit it. Both appear in the listing, and [`EnvironmentEntry`] keeps
    /// them apart, exactly as [`Self::environment`] does for a single name.
    ///
    /// Costs one tmux command per variable. The listing alone cannot be
    /// trusted: a value containing a newline occupies more than one line, and
    /// a continuation line holding an `=` is indistinguishable from the next
    /// variable. Each name is therefore read back on its own, which also
    /// discards the continuation lines, because tmux refuses a name it does
    /// not hold.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the listing.
    /// An empty map means the session holds nothing, never that the listing
    /// failed.
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
    /// let session = guard.server().new_session("env").await?;
    ///
    /// session.set_environment("EDITOR", "vi").await?;
    /// session.hide_environment("PAGER").await?;
    ///
    /// let environment = session.environment_all().await?;
    /// assert!(matches!(
    ///     environment.get("EDITOR"),
    ///     Some(EnvironmentEntry::Set(value)) if value.as_bytes() == b"vi",
    /// ));
    /// assert_eq!(environment.get("PAGER"), Some(&EnvironmentEntry::Removed));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn environment_all(&self) -> Result<BTreeMap<String, EnvironmentEntry>, Error> {
        environment::all(&self.core, environment::Scope::Session(self.id().as_ref())).await
    }

    /// Hide a variable from processes started in this session.
    ///
    /// Different from [`Self::unset_environment`], which deletes the session's
    /// own entry and lets whatever tmux inherited show through. This keeps an
    /// entry and marks it, so a process started here is handed an environment
    /// with the name *absent* even though the tmux server has one. It is what
    /// [`EnvironmentEntry::Removed`] reports.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
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
    /// let session = guard.server().new_session("hidden").await?;
    ///
    /// session.hide_environment("PAGER").await?;
    /// assert_eq!(
    ///     session.environment_all().await?.get("PAGER"),
    ///     Some(&EnvironmentEntry::Removed),
    /// );
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn hide_environment(&self, name: &str) -> Result<(), Error> {
        environment::hide(
            &self.core,
            environment::Scope::Session(self.id().as_ref()),
            name,
        )
        .await
    }

    /// Remove an environment variable from the session.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
    pub async fn unset_environment(&self, name: &str) -> Result<(), Error> {
        environment::unset(
            &self.core,
            environment::Scope::Session(self.id().as_ref()),
            name,
        )
        .await
    }

    /// Read one option, decoded according to what tmux declares about it.
    ///
    /// A flag comes back as [`OptionValue::Flag`] and a number as
    /// [`OptionValue::Number`], so a caller does not decide for itself that
    /// `on` means one. Everything else, including user options, stays text.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn typed_option(&self, name: &str) -> Result<Option<OptionValue>, Error> {
        let target = self.id().to_string();
        Ok(
            options::get(&self.core, options::Scope::Session(&target), name)
                .await?
                .map(|value| OptionValue::decode(name, value)),
        )
    }

    /// Find this session's window with the given name.
    ///
    /// Names are compared as bytes. A session can hold several windows with
    /// one name, in which case the first in tmux's order is returned.
    ///
    /// # Errors
    ///
    /// Returns an error when the window listing fails.
    pub async fn window(&self, name: impl AsRef<[u8]>) -> Result<Option<Window>, Error> {
        let name = name.as_ref();

        Ok(self
            .windows()
            .await?
            .into_iter()
            .find(|window| window.name() == name))
    }

    /// Find this session's window at the given index.
    ///
    /// # Errors
    ///
    /// Returns an error when the window listing fails.
    pub async fn window_at(&self, index: i32) -> Result<Option<Window>, Error> {
        // An index is an integer, so tmux can match it and return one row.
        let target = self.id().to_string();
        let projections = listing::windows(
            &self.core,
            listing::Scope::Target(&target),
            Some(&index.predicate("window_index")),
        )
        .await?;

        Ok(projections
            .into_iter()
            .next()
            .map(|projection| Window::new(Arc::clone(&self.core), projection)))
    }
}

/// Sessions compare by server endpoint and session id.
///
/// Equal-looking ids on different servers are different sessions, and two
/// handles for the same session remain equal even when their snapshots were
/// taken at different times.
impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        self.server_identity() == other.server_identity() && self.id() == other.id()
    }
}

impl Eq for Session {}

impl Hash for Session {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.server_identity().hash(state);
        self.id().hash(state);
    }
}

/// Renders identity only, never snapshot text.
///
/// Session names can carry arbitrary bytes from the user's environment, so
/// they stay out of diagnostics.
impl fmt::Debug for Session {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Session")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}

/// Filtering a session uses the same handles as the snapshot beneath it.
///
/// Matching and validation delegate to that snapshot, so an expression can
/// only name fields the catalog knows. The companion is re-parameterized to
/// [`Session`] so the type a listing returns is the type an expression
/// filters.
#[cfg(feature = "query")]
impl Filterable for Session {
    type Fields = SessionFields<Self>;

    const FILTER_TARGET: &'static str = <SessionInfo as Filterable>::FILTER_TARGET;

    fn filter_fields() -> Self::Fields {
        Self::Fields::for_target(Self::FILTER_TARGET)
    }

    fn __filter_matches(&self, predicate: &crate::query::__private::Predicate) -> bool {
        self.info.__filter_matches(predicate)
    }

    fn __filter_validate(
        predicate: &crate::query::__private::Predicate,
    ) -> Result<(), crate::query::FilterExpressionError> {
        <SessionInfo as Filterable>::__filter_validate(predicate)
    }
}

/// Options for creating a window in a session.
///
/// A bare name is accepted wherever this is: `session.new_window("editor")`.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::NewWindowOptions;
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("work").await?;
///
/// let window = session
///     .new_window(NewWindowOptions::new("editor").environment("EDITOR", "vi"))
///     .await?;
/// assert_eq!(window.name().to_string_lossy(), "editor");
///
/// // A bare name works too, because `new_window` takes anything that converts.
/// session.new_window("logs").await?;
/// assert_eq!(session.windows().await?.len(), 3);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[must_use = "options describe a window but do not create one"]
#[derive(Clone, Debug)]
pub struct NewWindowOptions {
    name: Option<OsString>,
    start_directory: Option<std::path::PathBuf>,
    command: Option<OsString>,
    index: Option<i32>,
    placement: Option<WindowPlacement>,
    environment: Vec<(OsString, OsString)>,
    replace_existing: bool,
    select: bool,
}

/// Where a new window goes, relative to the index it is given.
///
/// Without one, tmux takes the first free index.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::{NewWindowOptions, WindowPlacement};
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("work").await?;
/// let first = session.active_window().await?.expect("a session has a window");
///
/// // Inserting before an index shifts the windows already at or after it, so
/// // the index a caller holds is only good until the next insert.
/// let inserted = session
///     .new_window(
///         NewWindowOptions::new("inserted")
///             .index(first.index())
///             .placement(WindowPlacement::Before),
///     )
///     .await?;
/// assert!(inserted.index() <= first.index());
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WindowPlacement {
    /// Insert before the target index, shifting later windows along.
    Before,
    /// Insert after it.
    After,
}

impl NewWindowOptions {
    /// Describe a window with no name, letting tmux choose one.
    pub const fn unnamed() -> Self {
        Self {
            name: None,
            start_directory: None,
            command: None,
            index: None,
            placement: None,
            environment: Vec::new(),
            replace_existing: false,
            select: false,
        }
    }

    /// Describe a window with the given name.
    pub fn new(name: impl Into<OsString>) -> Self {
        Self {
            name: Some(name.into()),
            ..Self::unnamed()
        }
    }

    /// Set the window's working directory.
    pub fn start_directory(mut self, directory: impl Into<std::path::PathBuf>) -> Self {
        self.start_directory = Some(directory.into());
        self
    }

    /// Run a command instead of the default shell.
    pub fn command(mut self, command: impl Into<OsString>) -> Self {
        self.command = Some(command.into());
        self
    }

    /// Place the window at a window index rather than the first free one.
    pub const fn index(mut self, index: i32) -> Self {
        self.index = Some(index);
        self
    }

    /// Insert relative to the index rather than at it.
    ///
    /// Without an index this is relative to the session's current window,
    /// which is what tmux does.
    pub const fn placement(mut self, placement: WindowPlacement) -> Self {
        self.placement = Some(placement);
        self
    }

    /// Set an environment variable for the process the new window starts.
    ///
    /// Call this more than once for more than one variable. tmux applies
    /// these to the new process only, not to the session.
    pub fn environment(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((name.into(), value.into()));
        self
    }

    /// Replace whatever already occupies the target index.
    ///
    /// Without this, an index that is taken is an error rather than a silent
    /// overwrite.
    pub const fn replace_existing(mut self) -> Self {
        self.replace_existing = true;
        self
    }

    /// Make the new window active in its session.
    ///
    /// Creation does not select by default, so building a workspace does not
    /// leave the session pointing at whichever window happened to be last.
    pub const fn select(mut self) -> Self {
        self.select = true;
        self
    }

    /// Lower these options into a `new-window` command for one session.
    ///
    /// `print_format` is placed with the other flags because tmux stops
    /// parsing flags at the first positional, and the shell command is one.
    fn into_command(self, session: &str, print_format: &str) -> Command {
        let target = self
            .index
            .map_or_else(|| session.to_owned(), |index| format!("{session}:{index}"));
        let mut command = Command::new("new-window")
            .arg("-P")
            .arg("-F")
            .arg(print_format)
            .arg("-t")
            .arg(target);
        if !self.select {
            command = command.arg("-d");
        }
        match self.placement {
            Some(WindowPlacement::Before) => command = command.arg("-b"),
            Some(WindowPlacement::After) => command = command.arg("-a"),
            None => {}
        }
        if self.replace_existing {
            command = command.arg("-k");
        }
        if let Some(name) = self.name {
            command = command.arg("-n").arg(name);
        }
        if let Some(directory) = self.start_directory {
            command = command.arg("-c").arg(directory.into_os_string());
        }
        for (name, value) in self.environment {
            command = command
                .arg("-e")
                .arg(crate::window::assignment(&name, &value));
        }
        if let Some(shell_command) = self.command {
            command = command.arg(shell_command);
        }
        command
    }
}

impl<T: Into<OsString>> From<T> for NewWindowOptions {
    fn from(name: T) -> Self {
        Self::new(name)
    }
}

/// Renders the session id, which is what a tmux target wants.
impl fmt::Display for Session {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.id())
    }
}
