use std::collections::HashSet;
use std::ffi::OsStr;
use std::sync::Arc;

use super::{PaneDirection, Window};
use crate::internal::listing::{self, Pushdown as _};
use crate::pane::Pane;
use crate::session::Session;
use crate::{Command, Error, ObjectKind};

impl Window {
    /// List this window's panes, in tmux's own order.
    ///
    /// This is the lenient form; use [`Window::panes`] when the reason for
    /// an empty result matters.
    pub async fn panes_or_empty(&self) -> Vec<Pane> {
        self.panes().await.unwrap_or_else(|error| {
            listing::trace_discarded("list-panes", &error);
            Vec::new()
        })
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
        self.search_panes(matcher).await.unwrap_or_else(|error| {
            listing::trace_discarded("list-panes", &error);
            Vec::new()
        })
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

        self.active_pane()
            .await
            .map_err(|error| error.after_effect("select-pane"))?
            .ok_or_else(|| {
                Error::ObjectGone {
                    kind: ObjectKind::Window,
                    id: target,
                }
                .after_effect("select-pane")
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
            return Err(Error::from_refused_result(
                "last-pane",
                &result,
                Some(OsStr::new(&target)),
            ));
        }

        let active = self
            .active_pane()
            .await
            .map_err(|error| error.after_effect("last-pane"))?;
        active
            .ok_or_else(|| {
                Error::ObjectGone {
                    kind: ObjectKind::Window,
                    id: target,
                }
                .after_effect("last-pane")
            })
            .map(Some)
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
        self.linked_sessions().await.unwrap_or_else(|error| {
            listing::trace_discarded("list-sessions", &error);
            Vec::new()
        })
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
