use std::sync::Arc;

use super::{Window, WindowLink};
use crate::internal::listing::{self, Pushdown as _, Scope};
use crate::{Command, CommandChain, Error, ObjectKind, Session, SessionId, WindowLinkSnapshot};

impl WindowLink {
    /// The observed window shared by this and any other links to it.
    #[must_use]
    pub const fn window(&self) -> &Window {
        &self.window
    }

    /// Take the observed window, keeping its handle and cached fields.
    #[must_use]
    pub fn into_window(self) -> Window {
        self.window
    }

    /// The immutable placement this handle addresses.
    #[must_use]
    pub const fn snapshot(&self) -> &WindowLinkSnapshot {
        &self.snapshot
    }

    /// Make this exact observed placement active in its session.
    ///
    /// # Errors
    ///
    /// A reindexed or withdrawn placement returns [`Error::LinkGone`]. A
    /// window absent from every session returns [`Error::ObjectGone`]. A
    /// failed refresh after selection retains [`Error::AfterEffect`].
    pub async fn select(&mut self) -> Result<&mut Self, Error> {
        self.dispatch(
            "select-window",
            CommandChain::new(self.snapshot.guard_command()).then(
                Command::new("select-window")
                    .arg("-t")
                    .arg(self.snapshot.target()),
            ),
            &[&self.snapshot],
        )
        .await?;
        let session = self.snapshot.session().clone();
        self.refresh_at(&session, self.snapshot.index())
            .await
            .map_err(|error| error.after_effect("select-window"))?;
        Ok(self)
    }

    /// Move this observed placement to a free index in the destination session.
    ///
    /// The returned handle contains a fresh snapshot of the destination link.
    /// Other links to the same window remain in place.
    ///
    /// # Errors
    ///
    /// Refuses another server or daemon generation before dispatch, a stale
    /// source placement, and an absent or occupied destination. A failure to
    /// observe the resulting placement retains [`Error::AfterEffect`].
    pub async fn move_to(&mut self, session: &Session, index: i32) -> Result<&mut Self, Error> {
        self.window
            .core
            .require_same_instance(&session.core, "move-window")?;
        let destination = format!("{}:{index}", session.id());
        let session_guard = Command::new("if-shell")
            .arg("-F")
            .arg("-t")
            .arg(format!("{}:", session.id()))
            .arg(session.id().predicate("session_id"))
            .arg("")
            .arg("}");
        let queue = CommandChain::new(self.snapshot.guard_command())
            .then(session_guard)
            .then(
                Command::new("move-window")
                    .arg("-s")
                    .arg(self.snapshot.target())
                    .arg("-t")
                    .arg(destination),
            );
        self.dispatch("move-window", queue, &[&self.snapshot])
            .await?;
        self.refresh_at(session.id(), index)
            .await
            .map_err(|error| error.after_effect("move-window"))?;
        Ok(self)
    }

    /// Exchange two exact placements and refresh this handle at the peer's slot.
    ///
    /// The borrowed peer remains its old observation. Rediscover its window
    /// before using its new placement.
    ///
    /// # Errors
    ///
    /// Refuses another server or generation before dispatch and either stale
    /// placement in the queue. A failed destination observation retains
    /// [`Error::AfterEffect`].
    pub async fn swap_with(&mut self, other: &Self) -> Result<&mut Self, Error> {
        self.window
            .core
            .require_same_instance(&other.window.core, "swap-window")?;
        let queue = CommandChain::new(self.snapshot.guard_command())
            .then(other.snapshot.guard_command())
            .then(
                Command::new("swap-window")
                    .arg("-s")
                    .arg(self.snapshot.target())
                    .arg("-t")
                    .arg(other.snapshot.target()),
            );
        self.dispatch("swap-window", queue, &[&self.snapshot, &other.snapshot])
            .await?;
        self.refresh_at(other.snapshot.session(), other.snapshot.index())
            .await
            .map_err(|error| error.after_effect("swap-window"))?;
        Ok(self)
    }

    /// Remove this placement if it still names the observed window.
    ///
    /// # Errors
    ///
    /// Refuses a replaced daemon, a withdrawn or reindexed link, and removal
    /// of a window's only link. Other links to the window remain available.
    pub async fn unlink(self) -> Result<(), Error> {
        self.dispatch(
            "unlink-window",
            self.snapshot.unlink_command(),
            &[&self.snapshot],
        )
        .await
    }

    async fn dispatch(
        &self,
        operation: &'static str,
        queue: CommandChain,
        links: &[&WindowLinkSnapshot],
    ) -> Result<(), Error> {
        let result = self.window.core.execute_chain(queue).await?;
        if result.success() {
            return Ok(());
        }
        let target = self.snapshot.target();
        let primary = Error::from_refused_result(operation, &result, Some(target.as_ref()));
        for link in links {
            if let Some(error) = self.placement_failure(link).await {
                return Err(error);
            }
        }
        Err(primary)
    }

    async fn placement_failure(&self, link: &WindowLinkSnapshot) -> Option<Error> {
        let server = crate::Server::from_core(Arc::clone(&self.window.core));
        match server.window_by_id(&link.window().id()).await {
            Ok(None) => Some(Error::ObjectGone {
                kind: ObjectKind::Window,
                id: link.window().id().to_string(),
            }),
            Ok(Some(_)) => {
                let target = link.target();
                let filter = link.window().id().predicate("window_id");
                match listing::windows(
                    &self.window.core,
                    Scope::Target(link.session().as_ref()),
                    Some(&filter),
                )
                .await
                {
                    Ok(rows)
                        if !rows.iter().any(|row| {
                            row.link().identity().session_id() == link.session()
                                && row.link().identity().window_index() == link.index()
                                && row.window().window_id() == &link.window().id()
                        }) =>
                    {
                        Some(Error::LinkGone {
                            kind: ObjectKind::Window,
                            target,
                        })
                    }
                    Err(error) if error.is_object_gone() => Some(Error::LinkGone {
                        kind: ObjectKind::Window,
                        target,
                    }),
                    _ => None,
                }
            }
            Err(_) => None,
        }
    }

    async fn refresh_at(&mut self, session: &SessionId, index: i32) -> Result<(), Error> {
        let filter = self.window.id().predicate("window_id");
        let projection = listing::windows(
            &self.window.core,
            Scope::Target(session.as_ref()),
            Some(&filter),
        )
        .await?
        .into_iter()
        .find(|row| {
            row.link().identity().session_id() == session
                && row.link().identity().window_index() == index
                && row.window().window_id() == self.window.id()
        })
        .ok_or_else(|| Error::LinkGone {
            kind: ObjectKind::Window,
            target: format!("{session}:{index}"),
        })?;
        self.window.projection = projection;
        self.snapshot = WindowLinkSnapshot::new(session.clone(), index, self.window.key().clone());
        Ok(())
    }
}
