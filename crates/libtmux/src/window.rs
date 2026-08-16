//! Window handles and their snapshot getters.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::formats::TmuxText;
use crate::internal::core::Core;
use crate::internal::listing::{self, Pushdown as _};
use crate::internal::options;
use crate::pane::Pane;
#[cfg(feature = "query")]
use crate::query::Filterable;
use crate::session::Session;
use crate::snapshot::WindowProjection;
#[cfg(feature = "query")]
use crate::snapshot::{WindowFields, WindowInfo};
use crate::target::{ServerIdentity, SessionId, WindowId};
use crate::{Command, CommandResult, Error, IndexedHooks, ObjectKind, OptionValue, ReplaceMode};

/// One tmux window, as reached through one session that links it.
///
/// A window is not owned by a single session. `link-window` makes the same
/// underlying window appear in several sessions at once, so discovery returns
/// one `Window` per link rather than per window. Two handles for the same
/// window reached through different sessions are **not** equal: they describe
/// different places in the hierarchy.
///
/// Getters that describe the window itself, such as [`Window::name`], read the
/// window. Getters that describe its place in a session, such as
/// Which way to move focus among a window's panes.
///
/// tmux decides what is "up" from a pane's position on screen rather than
/// from its index, so these follow the layout rather than the pane order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum PaneDirection {
    /// The pane above.
    Above,
    /// The pane below.
    Below,
    /// The pane to the left.
    Left,
    /// The pane to the right.
    Right,
}

impl PaneDirection {
    /// The tmux flag that selects this direction.
    const fn flag(self) -> &'static str {
        match self {
            Self::Above => "-U",
            Self::Below => "-D",
            Self::Left => "-L",
            Self::Right => "-R",
        }
    }
}

/// [`Window::index`] and [`Window::is_active`], read the link.
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
///
/// // Splitting is detached by default, so focus stays where it was.
/// window.split(SplitDirection::Below).await?;
/// assert_eq!(window.panes().await?.len(), 2);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Window {
    core: Arc<Core>,
    projection: WindowProjection,
}

impl Window {
    /// Build a handle from a hydrated projection.
    pub(crate) const fn new(core: Arc<Core>, projection: WindowProjection) -> Self {
        Self { core, projection }
    }

    /// Find the window this process is running in.
    ///
    /// Resolved through the pane named by `TMUX_PANE`, which is the only
    /// value tmux gives a process that identifies exactly where it is.
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
    /// use libtmux::Window;
    ///
    /// let session = server.new_session("locating-window").await?;
    /// let pane = session.panes().await?.remove(0);
    ///
    /// // Standing in for the environment tmux gives a process it starts.
    /// let found = Window::from_env_value(server, Some(pane.id().as_ref())).await?;
    ///
    /// assert_eq!(found.expect("the window exists").id(), pane.window_id());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_env(server: &crate::Server) -> Result<Option<Self>, Error> {
        Self::from_env_value(server, std::env::var_os("TMUX_PANE")).await
    }

    /// Find a window from an explicit `TMUX_PANE` value.
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

        server.window_by_id(pane.window_id()).await
    }

    /// Return the tmux window identity.
    ///
    /// This is the `@`-prefixed id, which is shared by every link to the same
    /// window.
    #[must_use]
    pub const fn id(&self) -> &WindowId {
        self.projection.window().window_id()
    }

    /// Return the session this handle reached the window through.
    #[must_use]
    pub const fn session_id(&self) -> &SessionId {
        self.projection.link().identity().session_id()
    }

    /// Return the window's index within [`Window::session_id`].
    ///
    /// A linked window can hold a different index in each session, so this is
    /// a property of the link rather than of the window.
    #[must_use]
    pub const fn index(&self) -> i32 {
        self.projection.link().identity().window_index()
    }

    /// Return the window name.
    #[must_use]
    pub fn name(&self) -> &TmuxText {
        self.projection.window().window_name()
    }

    /// Return how many panes the window contains.
    #[must_use]
    pub fn pane_count(&self) -> u32 {
        *self.projection.window().window_panes()
    }

    /// Return the window width in cells.
    #[must_use]
    pub fn width(&self) -> u32 {
        *self.projection.window().window_width()
    }

    /// Return the window height in cells.
    #[must_use]
    pub fn height(&self) -> u32 {
        *self.projection.window().window_height()
    }

    /// Return the window's pane layout string.
    #[must_use]
    pub fn layout(&self) -> &TmuxText {
        self.projection.window().window_layout()
    }

    /// Report whether this window is the active one in its session.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.projection.link().is_active()
    }

    /// Report whether the window is linked into more than one session.
    #[must_use]
    pub const fn is_linked(&self) -> bool {
        self.projection.link().is_linked()
    }

    /// Report whether the window has unseen activity in this session.
    #[must_use]
    pub const fn has_activity(&self) -> bool {
        self.projection.link().has_activity()
    }

    /// Report whether the window rang a bell in this session.
    #[must_use]
    pub const fn has_bell(&self) -> bool {
        self.projection.link().has_bell()
    }

    /// Report whether one of the window's panes is zoomed to fill it.
    ///
    /// This is the window's own flag rather than the pane's, because tmux
    /// only reports `pane_zoomed_flag` from 3.7 onwards.
    #[must_use]
    pub fn is_zoomed(&self) -> bool {
        *self.projection.window().window_zoomed_flag()
    }

    /// Return the identity of the server this window belongs to.
    pub(crate) fn server_identity(&self) -> &ServerIdentity {
        self.core.configuration().identity()
    }

    /// List this window's panes, in tmux's own order.
    ///
    /// This is the lenient form; use [`Window::panes`] when the reason for
    /// an empty result matters.
    pub async fn panes_or_empty(&self) -> Vec<Pane> {
        self.panes().await.unwrap_or_default()
    }

    /// List this window's panes, preserving any failure.
    ///
    /// Panes are addressed by window id rather than by session and index, so
    /// this returns the same panes through every link to the window.
    ///
    /// # Errors
    ///
    /// Returns an error when the list command cannot run, or when its output
    /// cannot be decoded into snapshots.
    pub async fn panes(&self) -> Result<Vec<Pane>, Error> {
        let target = self.id().to_string();
        let projections = listing::panes(&self.core, listing::Scope::Target(&target), None).await?;

        Ok(projections
            .into_iter()
            .map(|projection| Pane::new(Arc::clone(&self.core), projection))
            .collect())
    }

    /// The panes under this window that a matcher accepts.
    ///
    /// Empty when the listing fails, which suits a status line. Use
    /// [`Self::search_panes`] when the difference matters.
    ///
    /// Filtering happens here rather than in tmux. A [`crate::query::FilterExpr`]
    /// is built to stay compilable to a tmux `-f` predicate, so pushing one
    /// down later would change what this costs and not what it answers.
    #[cfg(feature = "query")]
    #[must_use]
    pub async fn search_panes_or_empty<M: crate::query::Matcher<Pane>>(
        &self,
        matcher: M,
    ) -> Vec<Pane> {
        self.search_panes(matcher).await.unwrap_or_default()
    }

    /// The panes under this window that a matcher accepts, reporting why
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
    /// let window = session.active_window().await?.expect("a window");
    ///
    /// let fields = libtmux::Pane::filter_fields();
    /// let found = window.search_panes(&fields.pane_active.eq(true)).await?;
    /// assert_eq!(found.len(), 1);
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "query")]
    pub async fn search_panes<M: crate::query::Matcher<Pane>>(
        &self,
        matcher: M,
    ) -> Result<Vec<Pane>, Error> {
        use crate::query::QueryIteratorExt as _;

        let all = self.panes().await?;
        Ok(all.iter().matching(matcher).cloned().collect())
    }

    /// Return the window's active pane.
    ///
    /// # Errors
    ///
    /// Returns an error when the pane listing fails. A window always has an
    /// active pane, so `Ok(None)` means the window disappeared between the
    /// snapshot and this call.
    pub async fn active_pane(&self) -> Result<Option<Pane>, Error> {
        Ok(self.panes().await?.into_iter().find(Pane::is_active))
    }

    /// Move focus one pane in this direction, and report where it landed.
    ///
    /// tmux wraps: asking to go up from the topmost pane lands on the bottom
    /// one, and a window holding a single pane stays where it is. Neither is
    /// a failure, and tmux reports neither, so this returns the pane rather
    /// than absence. A caller that wants to know whether it moved compares
    /// the returned ID with the one it started from.
    ///
    /// Direction follows the layout, not the pane index: "up" means the pane
    /// drawn above this one.
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
    /// use libtmux::{PaneDirection, SplitDirection};
    ///
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("directions").await?;
    /// let window = session.active_window().await?.expect("a session has a window");
    ///
    /// // Splitting leaves focus where it was, so the top pane is still active.
    /// let lower = window.split(SplitDirection::Below).await?;
    ///
    /// let moved = window.focus_direction(PaneDirection::Below).await?;
    /// assert_eq!(moved.id(), lower.id(), "focus moved down to the new pane");
    ///
    /// // Down again from the bottom wraps rather than failing.
    /// let wrapped = window.focus_direction(PaneDirection::Below).await?;
    /// assert_ne!(wrapped.id(), lower.id(), "the edge wraps back to the top");
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn focus_direction(&self, direction: PaneDirection) -> Result<Pane, Error> {
        let target = self.id().to_string();
        listing::mutate(
            &self.core,
            "select-pane",
            Command::new("select-pane")
                .arg(direction.flag())
                .arg("-t")
                .arg(&target),
        )
        .await?;

        self.active_pane().await?.ok_or(Error::ObjectGone {
            kind: ObjectKind::Window,
            id: target,
        })
    }

    /// Move to the pane that was active before this one, and return it.
    ///
    /// `None` when nothing else has been active yet, which a window holding
    /// one pane never has. That is an ordinary state rather than a failure,
    /// so it is reported as absence; an error means tmux could not be reached
    /// or refused the move for some other reason.
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
    /// use libtmux::{SplitDirection, SplitOptions};
    ///
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("panes").await?;
    /// let mut window = session.active_window().await?.expect("a window");
    ///
    /// // One pane, so there is nowhere to go back to and nothing broke.
    /// assert!(window.last_pane().await?.is_none());
    ///
    /// window.split(SplitOptions::new(SplitDirection::Below).select()).await?;
    /// assert!(window.last_pane().await?.is_some());
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn last_pane(&self) -> Result<Option<Pane>, Error> {
        let target = self.id().to_string();
        let result = self
            .core
            .execute(Command::new("last-pane").arg("-t").arg(&target))
            .await?;
        if !result.success() {
            let stderr = result.stderr_lossy();
            if crate::error::NO_SUCH_NEIGHBOUR.contains(&stderr.trim_end()) {
                return Ok(None);
            }
            return Err(Error::refused(
                "last-pane",
                result.exit_code(),
                stderr.into_owned(),
                Some(OsStr::new(&target)),
            ));
        }

        self.active_pane().await
    }

    /// The sessions this window is linked into, in the order tmux lists them.
    ///
    /// A window can be linked into several sessions at once, and every one of
    /// them holds the same window rather than a copy. This reports the
    /// sessions reaching it, including the one this handle was found through.
    ///
    /// The sessions are read from tmux's winlink rows rather than from
    /// `#{window_linked_sessions_list}`, which is a comma-separated list of
    /// *names* and so cannot be taken apart: a session named `has,comma`
    /// makes the list `a,has,comma`, which reads exactly like three sessions.
    ///
    /// Empty when the listing fails, which suits a status line. Use
    /// [`Self::linked_sessions`] when the difference matters.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let server = guard.server();
    /// let first = server.new_session("first").await?;
    /// let second = server.new_session("second").await?;
    /// let window = first.active_window().await?.expect("a window");
    ///
    /// assert_eq!(window.linked_sessions_or_empty().await.len(), 1);
    ///
    /// window.link_to(&second, None).await?;
    /// let linked = window.linked_sessions_or_empty().await;
    /// assert_eq!(linked.len(), 2, "the same window, reached two ways");
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn linked_sessions_or_empty(&self) -> Vec<Session> {
        self.linked_sessions().await.unwrap_or_default()
    }

    /// The sessions this window is linked into, reporting why if it cannot.
    ///
    /// The loud form of [`Self::linked_sessions_or_empty`].
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses a listing.
    pub async fn linked_sessions(&self) -> Result<Vec<Session>, Error> {
        // The window id is a sigil and digits, so tmux matches it server-side
        // and returns only the winlink rows that reach this window.
        let links = listing::windows(
            &self.core,
            listing::Scope::Server,
            Some(&self.id().predicate("window_id")),
        )
        .await?;

        let mut sessions = Vec::with_capacity(links.len());
        let mut seen = HashSet::with_capacity(links.len());
        for link in &links {
            let session = link.link().identity().session_id();
            if !seen.insert(session.number()) {
                continue;
            }
            let infos =
                listing::sessions(&self.core, Some(&session.predicate("session_id"))).await?;
            // A session that goes away between the two listings is dropped
            // rather than reported half-formed.
            sessions.extend(
                infos
                    .into_iter()
                    .next()
                    .map(|info| Session::new(Arc::clone(&self.core), info)),
            );
        }

        Ok(sessions)
    }
    /// Return the session this window was reached through.
    ///
    /// This re-reads tmux rather than the snapshot, so a session renamed or
    /// removed since discovery is reported as it is now. `Ok(None)` means the
    /// session no longer exists.
    ///
    /// # Errors
    ///
    /// Returns an error when the session listing fails.
    pub async fn session(&self) -> Result<Option<Session>, Error> {
        let infos = listing::sessions(&self.core, None).await?;

        Ok(infos
            .into_iter()
            .find(|info| info.session_id() == self.session_id())
            .map(|info| Session::new(Arc::clone(&self.core), info)))
    }

    /// Replace this handle's snapshot with the window's current state.
    ///
    /// The handle keeps following the same link, so a window that moved to a
    /// different index in the same session is found at its new index.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ObjectGone`] when this session no longer links the
    /// window, or a listing error when tmux could not be read.
    pub async fn refresh(&mut self) -> Result<&mut Self, Error> {
        let session = self.session_id().to_string();
        let projection = listing::windows(&self.core, listing::Scope::Target(&session), None)
            .await?
            .into_iter()
            .find(|projection| projection.window().window_id() == self.id())
            .ok_or_else(|| Error::ObjectGone {
                kind: ObjectKind::Window,
                id: self.id().to_string(),
            })?;

        self.projection = projection;
        Ok(self)
    }

    /// Return a new handle holding the window's current state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ObjectGone`] when this session no longer links the
    /// window, or a listing error when tmux could not be read.
    pub async fn refreshed(&self) -> Result<Self, Error> {
        let mut refreshed = self.clone();
        refreshed.refresh().await?;
        Ok(refreshed)
    }

    /// Split this window and return the new pane.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the split, which includes a target
    /// pane that is too small to divide.
    pub async fn split(&self, options: impl Into<SplitOptions>) -> Result<Pane, Error> {
        let options = options.into();
        let window = self.id().to_string();
        let projection =
            listing::create_pane(&self.core, |format| options.into_command(&window, format))
                .await?;

        Ok(Pane::new(Arc::clone(&self.core), projection))
    }

    /// Rename the window and update this handle.
    ///
    /// The name belongs to the window, so every session linking it sees the
    /// change.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the name.
    pub async fn rename(&mut self, name: impl Into<OsString>) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "rename-window",
            Command::new("rename-window")
                .arg("-t")
                .arg(self.id().to_string())
                .arg(name.into()),
        )
        .await?;

        self.refresh().await?;
        Ok(self)
    }

    /// Make this window active in the session it was reached through.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn select(&mut self) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "select-window",
            Command::new("select-window").arg("-t").arg(format!(
                "{}:{}",
                self.session_id(),
                self.index()
            )),
        )
        .await?;

        self.refresh().await?;
        Ok(self)
    }

    /// Set the window's pane layout.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the layout name or specification.
    pub async fn select_layout(&mut self, layout: impl Into<OsString>) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "select-layout",
            Command::new("select-layout")
                .arg("-t")
                .arg(self.id().to_string())
                .arg(layout.into()),
        )
        .await?;

        self.refresh().await?;
        Ok(self)
    }

    /// Move the window's panes round one position, keeping the layout.
    ///
    /// The panes swap places within the arrangement rather than the
    /// arrangement changing, so a rotation of a three-pane window three times
    /// returns it to where it started.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the rotation.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::{Rotation, SplitDirection, SplitOptions};
    ///
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("turning").await?;
    /// let mut window = session.active_window().await?.expect("a window");
    /// window.split(SplitOptions::new(SplitDirection::Below)).await?;
    ///
    /// let before: Vec<_> = window.panes().await?.iter().map(|p| p.id().clone()).collect();
    /// window.rotate(Rotation::Up).await?;
    /// let after: Vec<_> = window.panes().await?.iter().map(|p| p.id().clone()).collect();
    ///
    /// assert_ne!(before, after, "the panes moved");
    /// window.rotate(Rotation::Down).await?;
    /// let back: Vec<_> = window.panes().await?.iter().map(|p| p.id().clone()).collect();
    /// assert_eq!(before, back, "and the other way undoes it");
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn rotate(&self, rotation: Rotation) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "rotate-window",
            Command::new("rotate-window")
                .arg(rotation.flag())
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await
    }

    /// Kill the window, closing it in every session that links it.
    ///
    /// This consumes the handle. Use [`Window::unlink`] to remove only this
    /// session's link while leaving the window alive elsewhere.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn kill(self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "kill-window",
            Command::new("kill-window")
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await
    }

    /// Remove this session's link to the window, leaving other links intact.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command, which includes
    /// unlinking a window that only one session holds.
    pub async fn unlink(self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "unlink-window",
            Command::new("unlink-window").arg("-t").arg(format!(
                "{}:{}",
                self.session_id(),
                self.index()
            )),
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
        options::get(&self.core, options::Scope::Window(&target), name).await
    }

    /// List the option names set at this window's scope.
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
        options::names(&self.core, options::Scope::Window(&target)).await
    }

    /// Read every option set at this window, decoded by its declared kind.
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
    /// let window = session.new_window("w").await?;
    ///
    /// window.set_option("main-pane-width", "99").await?;
    /// let options = window.options().await?;
    /// assert!(options.contains_key("main-pane-width"));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn options(&self) -> Result<BTreeMap<String, OptionValue>, Error> {
        let target = self.id().to_string();
        options::typed_all(&self.core, options::Scope::Window(&target)).await
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
            options::Scope::Window(&target),
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
            options::Scope::Window(&target),
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
        options::unset(&self.core, options::Scope::Window(&target), name).await
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
        options::set_hook(&self.core, options::Scope::Window(&target), name, command).await
    }

    /// Remove one hook.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the hook name.
    pub async fn unset_hook(&self, name: &str) -> Result<(), Error> {
        let target = self.id().to_string();
        options::unset_hook(&self.core, options::Scope::Window(&target), name).await
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
    /// let window = session.active_window().await?.expect("a window");
    ///
    /// let mut entries = BTreeMap::new();
    /// entries.insert(0, TmuxText::from(b"display-message first".to_vec()));
    /// entries.insert(3, TmuxText::from(b"display-message fourth".to_vec()));
    ///
    /// window
    ///     .set_hooks("alert-bell", &IndexedHooks::from(entries), ReplaceMode::Replace)
    ///     .await?;
    ///
    /// let written = window.hook("alert-bell").await?.expect("the hook is set");
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
            options::Scope::Window(&target),
            name,
            hooks,
            replace,
        )
        .await
    }

    /// Run a raw tmux command against this window.
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
    /// let window = session.active_window().await?.expect("a window");
    ///
    /// let result = window
    ///     .cmd(libtmux::Command::new("display-message").arg("-p").arg("#{window_id}"))
    ///     .await?;
    /// assert_eq!(result.stdout_lossy().trim(), window.id().to_string());
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

    /// Expand a tmux format string in this window's context.
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
    /// let window = session.active_window().await?.expect("a window");
    ///
    /// let expanded = window.format("#{window_id}").await?;
    /// assert_eq!(expanded.to_string_lossy(), window.id().to_string());
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

    /// Show a message on the clients viewing this window.
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

    /// Read one hook's commands, or `None` when it holds nothing.
    ///
    /// There is deliberately no listing counterpart at this scope. tmux does
    /// not enumerate hooks set on a window or a pane: `show-hooks` reports
    /// nothing for them, and `show-options` omits them while still listing
    /// ordinary options. A listing here could only ever answer empty, which
    /// would read as "no hooks" rather than "tmux will not say". Reading one
    /// by name works, so that is what is offered; [`crate::Server::hooks`] and
    /// [`crate::Session::hooks`] list the scopes tmux does enumerate.
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
    /// let window = session.new_window("w").await?;
    ///
    /// assert!(window.hook("alert-bell").await?.is_none());
    /// window.set_hook("alert-bell", "display-message rang").await?;
    /// assert!(window.hook("alert-bell").await?.is_some());
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn hook(&self, name: &str) -> Result<Option<IndexedHooks>, Error> {
        let target = self.id().to_string();
        options::hook(&self.core, options::Scope::Window(&target), name).await
    }

    /// Create a pane, run an operation with it, then kill it.
    ///
    /// The pane is killed whether the operation succeeded or failed, so a
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
    /// pane could not be created, or could not be killed after the
    /// operation succeeded.
    pub async fn with_pane<T, E>(
        &self,
        options: impl Into<SplitOptions>,
        operation: impl AsyncFnOnce(&Pane) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<Error>,
    {
        let created = self.split(options).await?;
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

    /// Swap this window's position with another.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the swap.
    pub async fn swap_with(&mut self, other: &Self) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "swap-window",
            Command::new("swap-window")
                .arg("-s")
                .arg(self.id().to_string())
                .arg("-t")
                .arg(format!("{}:{}", other.session_id(), other.index())),
        )
        .await?;

        self.refresh().await?;
        Ok(self)
    }

    /// Move this window to an index, possibly in another session.
    ///
    /// The destination is a session and an index rather than a target string,
    /// because those are the two things tmux needs and a string could express
    /// neither or both.
    ///
    /// # Errors
    ///
    /// Returns an error when the destination is occupied or does not exist.
    pub async fn move_to(&mut self, session: &Session, index: i32) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "move-window",
            Command::new("move-window")
                .arg("-s")
                .arg(self.id().to_string())
                .arg("-t")
                .arg(format!("{}:{index}", session.id())),
        )
        .await?;

        self.refresh().await?;
        Ok(self)
    }

    /// Resize the window to an exact size in cells.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the size.
    /// Move one edge of the window by a number of cells.
    ///
    /// This is the form a keybinding uses: "make it two columns wider" rather
    /// than a size computed from the current one.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the resize.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
    /// use libtmux::ResizeDirection;
    ///
    /// let session = server.new_session("resized-window").await?;
    /// let mut window = session.windows().await?.remove(0);
    ///
    /// window.resize(80, 24).await?;
    /// window.resize_by(ResizeDirection::Down, 4).await?;
    /// assert_eq!(window.height(), 28);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn resize_by(
        &mut self,
        direction: ResizeDirection,
        cells: u32,
    ) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "resize-window",
            Command::new("resize-window")
                .arg("-t")
                .arg(self.id().to_string())
                .arg(direction.flag())
                .arg(cells.to_string()),
        )
        .await?;

        self.refresh().await?;
        Ok(self)
    }

    /// Link this window into another session, so both hold the same window.
    ///
    /// A linked window is one window with two winlinks, not a copy: renaming
    /// it or splitting it shows up in both sessions. [`Window::unlink`] takes
    /// one link away again.
    ///
    /// `index` places the link at a window index, or appends when `None`.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the link, which includes an index
    /// that is already taken.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
    /// let source = server.new_session("linking-from").await?;
    /// let target = server.new_session("linking-to").await?;
    /// let window = source.windows().await?.remove(0);
    ///
    /// window.link_to(&target, None).await?;
    ///
    /// let linked = target.windows().await?;
    /// assert!(linked.iter().any(|other| other.id() == window.id()));
    /// # Ok(())
    /// # }
    /// ```
    pub async fn link_to(&self, session: &Session, index: Option<i32>) -> Result<(), Error> {
        let target = index.map_or_else(
            || session.id().to_string(),
            |index| format!("{}:{index}", session.id()),
        );

        listing::mutate(
            &self.core,
            "link-window",
            Command::new("link-window")
                .arg("-s")
                .arg(self.id().to_string())
                .arg("-t")
                .arg(target),
        )
        .await?;

        Ok(())
    }

    /// Resize the window to an exact size in cells.
    ///
    /// Use [`Window::resize_by`] to move one edge instead.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the size.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
    /// let session = server.new_session("sized").await?;
    /// let mut window = session.windows().await?.remove(0);
    ///
    /// window.resize(100, 40).await?;
    /// assert_eq!((window.width(), window.height()), (100, 40));
    /// # Ok(())
    /// # }
    /// ```
    pub async fn resize(&mut self, width: u32, height: u32) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "resize-window",
            Command::new("resize-window")
                .arg("-t")
                .arg(self.id().to_string())
                .arg("-x")
                .arg(width.to_string())
                .arg("-y")
                .arg(height.to_string()),
        )
        .await?;

        self.refresh().await?;
        Ok(self)
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
            options::get(&self.core, options::Scope::Window(&target), name)
                .await?
                .map(|value| OptionValue::decode(name, value)),
        )
    }

    /// Find this window's pane at the given index.
    ///
    /// # Errors
    ///
    /// Returns an error when the pane listing fails.
    pub async fn pane_at(&self, index: u32) -> Result<Option<Pane>, Error> {
        let target = self.id().to_string();
        let projections = listing::panes(
            &self.core,
            listing::Scope::Target(&target),
            Some(&index.predicate("pane_index")),
        )
        .await?;

        Ok(projections
            .into_iter()
            .next()
            .map(|projection| Pane::new(Arc::clone(&self.core), projection)))
    }
}

/// Windows compare by server endpoint, session, index, and window id.
///
/// Equality follows the link, not the window, because a linked window occupies
/// a genuinely different position in each session that holds it. Compare
/// [`Window::id`] directly to ask whether two handles name the same underlying
/// window.
impl PartialEq for Window {
    fn eq(&self, other: &Self) -> bool {
        self.server_identity() == other.server_identity()
            && self.projection.link().identity() == other.projection.link().identity()
    }
}

impl Eq for Window {}

impl Hash for Window {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.server_identity().hash(state);
        self.projection.link().identity().hash(state);
    }
}

/// Renders identity only, never snapshot text.
impl fmt::Debug for Window {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Window")
            .field("id", &self.id())
            .field("session_id", &self.session_id())
            .field("index", &self.index())
            .finish_non_exhaustive()
    }
}

/// Filtering a window uses the same handles as the snapshot beneath it.
///
/// Matching and validation delegate to that snapshot, so an expression can
/// only name fields the catalog knows. The companion is re-parameterized to
/// [`Window`] so the type a listing returns is the type an expression
/// filters.
#[cfg(feature = "query")]
impl Filterable for Window {
    type Fields = WindowFields<Self>;

    const FILTER_TARGET: &'static str = <WindowInfo as Filterable>::FILTER_TARGET;

    fn filter_fields() -> Self::Fields {
        Self::Fields::for_target(Self::FILTER_TARGET)
    }

    fn __filter_matches(&self, predicate: &crate::query::__private::Predicate) -> bool {
        self.projection.window().__filter_matches(predicate)
    }

    fn __filter_validate(
        predicate: &crate::query::__private::Predicate,
    ) -> Result<(), crate::query::FilterExpressionError> {
        <WindowInfo as Filterable>::__filter_validate(predicate)
    }
}

/// Where a split puts the new pane, relative to the one being divided.
///
/// tmux spells these `-v`, `-v -b`, `-h`, and `-h -b`, where "horizontal"
/// means side by side. Naming the resulting position instead removes a
/// question every tmux user has asked at least once.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Rotation {
    /// Move each pane to the position before it, so the first becomes last.
    Up,
    /// Move each pane to the position after it, which is tmux's default.
    Down,
}

impl Rotation {
    /// The tmux flag that asks for this direction.
    const fn flag(self) -> &'static str {
        match self {
            Self::Up => "-U",
            Self::Down => "-D",
        }
    }
}

/// Where a split puts the pane it makes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplitDirection {
    /// Above the pane being divided.
    Above,
    /// Below it, which is tmux's default.
    Below,
    /// To its left.
    Left,
    /// To its right.
    Right,
}

impl SplitDirection {
    /// Return the tmux flags that produce this position.
    const fn flags(self) -> (&'static str, bool) {
        match self {
            Self::Above => ("-v", true),
            Self::Below => ("-v", false),
            Self::Left => ("-h", true),
            Self::Right => ("-h", false),
        }
    }
}

/// How much space a new pane gets.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PaneSize {
    /// A number of rows or columns, depending on the split direction.
    Cells(u32),
    /// A share of the space being divided, as a percentage.
    Percent(u32),
}

impl fmt::Display for PaneSize {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cells(cells) => write!(formatter, "{cells}"),
            Self::Percent(percent) => write!(formatter, "{percent}%"),
        }
    }
}

/// Which edge a resize moves.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResizeDirection {
    /// Move the top edge up, making it taller.
    Up,
    /// Move the bottom edge down.
    Down,
    /// Move the left edge left.
    Left,
    /// Move the right edge right.
    Right,
}

impl ResizeDirection {
    /// Return the tmux flag for this direction.
    pub(crate) const fn flag(self) -> &'static str {
        match self {
            Self::Up => "-U",
            Self::Down => "-D",
            Self::Left => "-L",
            Self::Right => "-R",
        }
    }
}

/// Options for splitting a window or pane into a new pane.
///
/// A bare [`SplitDirection`] is accepted wherever this is:
/// `window.split(SplitDirection::Below)`.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::{SplitDirection, SplitOptions};
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("split").await?;
/// let window = session.active_window().await?.expect("a session has a window");
///
/// // Splitting leaves focus where it was unless asked otherwise, so a caller
/// // that wants the new pane selected says so.
/// let pane = window
///     .split(SplitOptions::new(SplitDirection::Right).select())
///     .await?;
/// assert_eq!(window.active_pane().await?.map(|active| active.id().to_string()),
///            Some(pane.id().to_string()));
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[must_use = "options describe a split but do not perform one"]
#[derive(Clone, Debug)]
pub struct SplitOptions {
    direction: SplitDirection,
    start_directory: Option<std::path::PathBuf>,
    command: Option<OsString>,
    size: Option<PaneSize>,
    environment: Vec<(OsString, OsString)>,
    full: bool,
    zoom: bool,
    select: bool,
}

impl SplitOptions {
    /// Describe a split placing the new pane in one direction.
    pub const fn new(direction: SplitDirection) -> Self {
        Self {
            direction,
            start_directory: None,
            command: None,
            size: None,
            environment: Vec::new(),
            full: false,
            zoom: false,
            select: false,
        }
    }

    /// Set the new pane's working directory.
    pub fn start_directory(mut self, directory: impl Into<std::path::PathBuf>) -> Self {
        self.start_directory = Some(directory.into());
        self
    }

    /// Run a command instead of the default shell.
    pub fn command(mut self, command: impl Into<OsString>) -> Self {
        self.command = Some(command.into());
        self
    }

    /// Give the new pane a size, in cells or as a share of the space.
    pub const fn size(mut self, size: PaneSize) -> Self {
        self.size = Some(size);
        self
    }

    /// Set an environment variable for the process the new pane starts.
    ///
    /// Call this more than once for more than one variable. tmux applies
    /// these to the new process only, not to the session.
    pub fn environment(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((name.into(), value.into()));
        self
    }

    /// Span the full width or height of the window rather than of the pane.
    pub const fn full(mut self) -> Self {
        self.full = true;
        self
    }

    /// Zoom the new pane, filling the window until it is unzoomed.
    pub const fn zoom(mut self) -> Self {
        self.zoom = true;
        self
    }

    /// Make the new pane active.
    pub const fn select(mut self) -> Self {
        self.select = true;
        self
    }

    /// Lower these options into a `split-window` command for one target.
    ///
    /// `print_format` is placed with the other flags because tmux stops
    /// parsing flags at the first positional, and the shell command is one.
    pub(crate) fn into_command(self, target: &str, print_format: &str) -> Command {
        let (axis, before) = self.direction.flags();
        let mut command = Command::new("split-window")
            .arg("-P")
            .arg("-F")
            .arg(print_format)
            .arg("-t")
            .arg(target)
            .arg(axis);
        if before {
            command = command.arg("-b");
        }
        if !self.select {
            command = command.arg("-d");
        }
        if self.full {
            command = command.arg("-f");
        }
        if self.zoom {
            command = command.arg("-Z");
        }
        if let Some(size) = self.size {
            command = command.arg("-l").arg(size.to_string());
        }
        if let Some(directory) = self.start_directory {
            command = command.arg("-c").arg(directory.into_os_string());
        }
        for (name, value) in self.environment {
            command = command.arg("-e").arg(assignment(&name, &value));
        }
        if let Some(shell_command) = self.command {
            command = command.arg(shell_command);
        }
        command
    }
}

impl From<SplitDirection> for SplitOptions {
    fn from(direction: SplitDirection) -> Self {
        Self::new(direction)
    }
}

/// Render one `NAME=VALUE` pair for tmux's `-e` flag.
///
/// Built from bytes because tmux accepts environment values that are not
/// valid UTF-8, and rejecting them here would be stricter than tmux is.
pub(crate) fn assignment(name: &OsStr, value: &OsStr) -> OsString {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

    let mut bytes = name.as_bytes().to_vec();
    bytes.push(b'=');
    bytes.extend_from_slice(value.as_bytes());

    OsString::from_vec(bytes)
}

/// Renders `session:index`, the form tmux targets a window by.
///
/// The id alone would be ambiguous about which link is meant, and this is
/// the spelling that can be pasted into a tmux command.
impl fmt::Display for Window {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.session_id(), self.index())
    }
}
