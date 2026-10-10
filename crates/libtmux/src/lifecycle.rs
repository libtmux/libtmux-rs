//! Owned scopes, bounded discovery and explicit find-or-create outcomes.
//!
//! Borrowed handles and [`Server::shutdown`] leave remote objects alive.
//! [`Owned::close`] and [`Owned::scope`] destroy the accepted object by ID on
//! its accepted daemon. A killed window destroys all its links and panes;
//! use [`Window::unlink`] when only one link should disappear.
//!
//! Await [`Server::drain_cleanup`] after joining cancelled scopes and before
//! shutting down the Tokio runtime. Cancellation releases cleanup to an
//! existing supervisor; Rust `Drop` performs no asynchronous destruction.
//! The drain returns errors that no scope caller received. Do not drain while
//! another caller is still awaiting its scope or owned acquisition.

mod discovery;
mod find;
pub(crate) mod identity;
pub(crate) mod jobs;

pub use discovery::{
    DiscoveredServer, Discovery, DiscoveryDiagnostic, DiscoveryReport, DiscoverySkip,
    DiscoveryTruncation,
};
pub use find::{FindOrCreate, PaneIdentity};

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::internal::{core::Core, scoped};
use crate::{
    Command, Error, NewSessionOptions, NewWindowOptions, Pane, ScopeError, Server,
    ServerGeneration, Session, SplitOptions, Window,
};
use identity::DaemonIdentity;

#[derive(Clone, Debug)]
pub(crate) enum Target {
    Server,
    Session(String),
    Window(String),
    Pane(String),
}

impl Target {
    pub(crate) fn command(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::Server => ("kill-server", None),
            Self::Session(id) => ("kill-session", Some(id)),
            Self::Window(id) => ("kill-window", Some(id)),
            Self::Pane(id) => ("kill-pane", Some(id)),
        }
    }
}

struct Lease {
    server: Server,
    identity: DaemonIdentity,
    target: Target,
    closed: AtomicBool,
    closing: tokio::sync::Semaphore,
}

/// Shared responsibility for killing one accepted remote object.
///
/// Clone an owner to retain access to its cleanup result after cancelling a
/// scope. Clones share the closed state: successful close is harmless to
/// repeat; a failed close retains the target and can be retried. Inspect
/// partial-effect and unknown-result errors before retrying a creation.
/// Dropping an owner alone does not kill anything. Use `scope` or await `close`.
#[derive(Clone)]
#[must_use = "use scope or await close to release the remote resource"]
pub struct Owned<T> {
    resource: T,
    lease: Arc<Lease>,
}

impl<T: std::fmt::Debug> std::fmt::Debug for Owned<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Owned")
            .field("resource", &self.resource)
            .field("generation", &self.lease.identity.generation)
            .field("target", &self.lease.target)
            .finish_non_exhaustive()
    }
}

impl<T> Owned<T> {
    fn new(resource: T, core: Arc<Core>, identity: DaemonIdentity, target: Target) -> Self {
        Self {
            resource,
            lease: Arc::new(Lease {
                server: Server::from_core(core),
                identity,
                target,
                closed: AtomicBool::new(false),
                closing: tokio::sync::Semaphore::new(1),
            }),
        }
    }

    /// Borrow the handle without transferring destruction responsibility.
    pub const fn resource(&self) -> &T {
        &self.resource
    }

    /// Return the daemon accepted when this owner was acquired.
    pub fn generation(&self) -> ServerGeneration {
        self.lease.identity.generation
    }

    /// Report whether a close attempt succeeded.
    pub fn is_closed(&self) -> bool {
        self.lease.closed.load(Ordering::Acquire)
    }

    /// Kill the accepted object and retain failures for another attempt.
    ///
    /// # Errors
    /// Returns a generation mismatch without touching a replacement daemon,
    /// or the original tmux/transport error. A cancelled wait still lets the
    /// attempt finish; `drain_cleanup` reports an unobserved failure.
    pub async fn close(&self) -> Result<(), Error> {
        if self.is_closed() {
            return Ok(());
        }
        let lease = Arc::clone(&self.lease);
        let job = lease.server.core.lifecycle.begin();
        let completion = Arc::clone(&job);
        tokio::spawn(async move {
            let result = match tokio::spawn(async move { lease.close().await }).await {
                Ok(result) => result,
                Err(error) => Err(Error::LifecycleTaskLost {
                    detail: error.to_string(),
                }),
            };
            completion.complete(result);
        });
        job.wait().await;
        job.take().ok_or_else(|| Error::LifecycleTaskLost {
            detail: "cleanup was drained while its caller was still awaiting it".to_owned(),
        })?
    }
}

impl<T: Clone + Send + Sync + 'static> Owned<T> {
    /// Run a body and await destruction on success or failure.
    ///
    /// # Errors
    /// `ScopeError` preserves the body's original error and any cleanup error.
    /// Cancellation and panics release cleanup to the running Tokio runtime.
    pub async fn scope<V, E>(
        &self,
        operation: impl AsyncFnOnce(&T) -> Result<V, E>,
    ) -> Result<V, ScopeError<V, E>> {
        let owner = self.clone();
        scoped::run_tracked(
            self.lease.server.core.lifecycle.clone(),
            "owned-scope",
            async move { Ok(owner) },
            |owner| async move { owner.lease.close().await },
            async |owner: &Self| operation(&owner.resource).await,
        )
        .await
    }
}

impl Lease {
    async fn close(&self) -> Result<(), Error> {
        let _permit = self
            .closing
            .acquire()
            .await
            .map_err(|error| Error::LifecycleTaskLost {
                detail: error.to_string(),
            })?;
        if !self.closed.load(Ordering::Acquire) {
            guarded_kill(&self.server.core, self.identity, &self.target).await?;
            self.closed.store(true, Ordering::Release);
        }
        Ok(())
    }
}

pub(crate) async fn guarded_kill(
    core: &Core,
    expected: DaemonIdentity,
    target: &Target,
) -> Result<(), Error> {
    let (name, id) = target.command();
    let action = id.map_or_else(|| name.to_owned(), |id| format!("{name} -t {id}"));
    guarded_action(core, expected, &action, name, id).await
}

pub(crate) async fn guarded_action(
    core: &Core,
    expected: DaemonIdentity,
    action: &str,
    name: &'static str,
    id: Option<&str>,
) -> Result<(), Error> {
    let condition = format!(
        "#{{&&:#{{&&:#{{==:#{{pid}},{}}},#{{==:#{{start_time}},{}}}}},#{{==:#{{@libtmux_owner_generation}},{}}}}}",
        expected.generation.pid,
        expected.generation.start_time,
        expected.token()
    );
    let result = core
        .execute_no_start(
            Command::new("if-shell")
                .arg("-F")
                .arg(condition)
                .sensitive_arg(action)
                .arg("display-message -p '__libtmux_generation__ #{pid} #{start_time}'"),
        )
        .await?;
    if !result.success() {
        return Err(Error::from_refused_result(
            name,
            &result,
            id.map(std::ffi::OsStr::new),
        ));
    }
    if let Some(answer) = result.stdout().strip_prefix(b"__libtmux_generation__ ") {
        let found = parse_generation(answer)?;
        return Err(if found == expected.generation {
            Error::OwnershipTokenChanged
        } else {
            Error::ServerGenerationChanged {
                expected: expected.generation,
                found,
            }
        });
    }
    Ok(())
}

pub(crate) fn parse_generation(bytes: &[u8]) -> Result<ServerGeneration, Error> {
    let text = std::str::from_utf8(bytes).unwrap_or_default();
    let mut parts = text.split_whitespace();
    let parsed = (
        parts
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|pid| *pid > 0),
        parts.next().and_then(|s| s.parse::<i64>().ok()),
    );
    match parsed {
        (Some(pid), Some(start_time)) if parts.next().is_none() => {
            Ok(ServerGeneration { pid, start_time })
        }
        _ => Err(Error::UnreadableFormatValue {
            format: "#{pid} #{start_time}",
            detail: crate::IdParseError::new('#'),
        }),
    }
}

impl Server {
    /// Wait for registered acquisitions/cleanup and return unobserved errors.
    ///
    /// Call after cancelled tasks have been joined, before `shutdown` and
    /// before runtime teardown. Errors are consumed once by this drain.
    pub async fn drain_cleanup(&self) -> Vec<Error> {
        self.core.lifecycle.drain().await
    }

    /// Accept responsibility for killing the daemon currently at this endpoint.
    ///
    /// # Errors
    /// Returns a probe or reserved ownership-metadata error. This never starts
    /// a daemon. Adoption initializes `@libtmux_owner_generation` if absent.
    pub async fn adopt(&self) -> Result<Owned<Self>, Error> {
        let identity = identity::accept(&self.core).await?;
        Ok(Owned::new(
            self.clone(),
            Arc::clone(&self.core),
            identity,
            Target::Server,
        ))
    }

    pub(crate) async fn generation_no_start(&self) -> Result<ServerGeneration, Error> {
        let result = self
            .core
            .execute_no_start(
                Command::new("display-message")
                    .arg("-p")
                    .arg("#{pid} #{start_time}"),
            )
            .await?;
        if !result.success() {
            return Err(Error::from_refused_result("display-message", &result, None));
        }
        parse_generation(result.stdout())
    }

    /// Create a session and hand its destruction responsibility to the caller.
    ///
    /// # Errors
    /// Creation errors retain known-ID rollback failures or an explicit
    /// unknown-result boundary. Cancellation before handoff kills a known ID.
    pub async fn owned_session(
        &self,
        options: impl Into<NewSessionOptions>,
    ) -> Result<Owned<Session>, Error> {
        let server = self.clone();
        let options = options.into();
        scoped::acquire_owned(self.core.lifecycle.clone(), async move {
            let session = server.new_session(options).await?;
            session.created_owner()
        })
        .await
    }
}

macro_rules! adoption {
    ($type:ty, $target:ident, $lookup:ident) => {
        impl $type {
            /// Accept responsibility for killing this ID on its current daemon.
            ///
            /// # Errors
            /// Returns an absent-object, generation or transport error if the
            /// object cannot be accepted. Lookup handles remain borrowed. Adoption
            /// initializes reserved `@libtmux_owner_generation` if absent.
            pub async fn adopt(&self) -> Result<Owned<Self>, Error> {
                let server = Server::from_core(Arc::clone(&self.core));
                let identity = identity::accept(&self.core).await?;
                let current =
                    server
                        .$lookup(self.id())
                        .await?
                        .ok_or_else(|| Error::ObjectGone {
                            kind: crate::ObjectKind::$target,
                            id: self.id().to_string(),
                        })?;
                guarded_action(
                    &self.core,
                    identity,
                    "display-message -p ''",
                    "display-message",
                    None,
                )
                .await?;
                Ok(Owned::new(
                    current,
                    Arc::clone(&self.core),
                    identity,
                    Target::$target(self.id().to_string()),
                ))
            }

            pub(crate) fn created_owner(self) -> Result<Owned<Self>, Error> {
                let identity = *self
                    .created_identity
                    .as_deref()
                    .ok_or(Error::LifecycleInput {
                        reason: "owned acquisition requires a creation receipt",
                    })?;
                let core = Arc::clone(&self.core);
                let target = Target::$target(self.id().to_string());
                Ok(Owned::new(self, core, identity, target))
            }
        }
    };
}
adoption!(Session, Session, session_by_id);
adoption!(Window, Window, window_by_id);
adoption!(Pane, Pane, pane_by_id);

impl Session {
    /// Create a window and hand its destruction responsibility to the caller.
    ///
    /// # Errors
    /// Returns a creation/rollback error. A killed owned window destroys all
    /// links and panes, even after a move or rename.
    pub async fn owned_window(
        &self,
        options: impl Into<NewWindowOptions>,
    ) -> Result<Owned<Window>, Error> {
        let session = self.clone();
        let options = options.into();
        scoped::acquire_owned(self.core.lifecycle.clone(), async move {
            session.new_window(options).await?.created_owner()
        })
        .await
    }
}
impl Window {
    /// Split a pane and hand its destruction responsibility to the caller.
    ///
    /// # Errors
    /// Returns a creation/rollback error; cancellation before handoff kills a
    /// known pane ID on the creating daemon.
    pub async fn owned_pane(&self, options: impl Into<SplitOptions>) -> Result<Owned<Pane>, Error> {
        let window = self.clone();
        let options = options.into();
        scoped::acquire_owned(self.core.lifecycle.clone(), async move {
            window.split(options).await?.created_owner()
        })
        .await
    }
}
