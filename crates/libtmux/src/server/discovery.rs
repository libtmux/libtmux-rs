use std::collections::{HashMap, HashSet};
#[cfg(feature = "query")]
use std::fmt;
use std::sync::Arc;

use super::Server;
use crate::client::Client;
use crate::internal::listing::{self, Pushdown as _};
use crate::pane::Pane;
#[cfg(feature = "query")]
use crate::query::{FilterSchema, Filterable, ManyRelation};
use crate::session::Session;
#[cfg(feature = "query")]
use crate::snapshot::{SessionFields, WindowFields};
use crate::window::Window;
use crate::{Error, PaneId, SessionId, WindowId};

impl Server {
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

#[cfg(feature = "query")]
impl FilterSchema for SessionTree {
    fn __filter_schema() -> crate::query::__private::FilterSchemaDescriptor {
        <Session as FilterSchema>::__filter_schema()
            .retarget(Self::FILTER_TARGET)
            .with_field(crate::query::__private::FilterFieldSchema::new(
                WINDOWS_RELATION,
                crate::query::__private::FilterValueSchema::Many(
                    crate::query::__private::filter_schema::<WindowTree>,
                ),
            ))
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

#[cfg(feature = "query")]
impl FilterSchema for WindowTree {
    fn __filter_schema() -> crate::query::__private::FilterSchemaDescriptor {
        <Window as FilterSchema>::__filter_schema()
            .retarget(Self::FILTER_TARGET)
            .with_field(crate::query::__private::FilterFieldSchema::new(
                PANES_RELATION,
                crate::query::__private::FilterValueSchema::Many(
                    crate::query::__private::filter_schema::<Pane>,
                ),
            ))
    }
}
