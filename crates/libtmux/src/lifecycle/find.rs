use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Owned, Target};
use crate::internal::scoped;
use crate::{
    Command, Error, NewSessionOptions, NewWindowOptions, Pane, Server, Session, SplitOptions,
    Window,
};

static SERIAL: AtomicU64 = AtomicU64::new(0);

/// A new owned resource or an existing borrowed handle.
///
/// Find-or-create calls on clones of one `Server` share serialization. Other
/// `Server` constructors, control-mode handles and external tmux clients do
/// not share that lock. tmux rejects duplicate session names; window names
/// and pane identities can become ambiguous after an external write.
#[derive(Clone, Debug)]
pub enum FindOrCreate<T> {
    /// This call created the resource and accepted destruction responsibility.
    Created(Owned<T>),
    /// The match existed already and carries no destruction responsibility.
    Reused(T),
}

impl<T> FindOrCreate<T> {
    /// Borrow either result's resource without changing its ownership.
    pub const fn resource(&self) -> &T {
        match self {
            Self::Created(owner) => owner.resource(),
            Self::Reused(resource) => resource,
        }
    }

    /// Report whether this call created the resource.
    pub const fn is_created(&self) -> bool {
        matches!(self, Self::Created(_))
    }
}

/// A pane user-option key and exact byte value set by the creation operation.
///
/// Matching is local to one window. A key starts with `@` and contains only
/// ASCII letters, digits, `_` or `-`; values must be nonempty and NUL-free.
/// Pane names, shell commands and titles are not unique application identities.
#[derive(Clone, Debug)]
pub struct PaneIdentity {
    key: String,
    value: OsString,
}

impl PaneIdentity {
    /// Describe an exact pane user-option match.
    ///
    /// # Errors
    /// Returns `LifecycleInput` for an invalid key or value.
    pub fn new(key: impl Into<String>, value: impl Into<OsString>) -> Result<Self, Error> {
        let key = key.into();
        let value = value.into();
        if !key.starts_with('@')
            || key.len() < 2
            || !key.as_bytes()[1..]
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(b))
            || value.is_empty()
            || value.as_bytes().contains(&0)
        {
            return Err(Error::LifecycleInput {
                reason: "pane identity needs an @key and a nonempty NUL-free value",
            });
        }
        Ok(Self { key, value })
    }
}

fn one<T>(kind: &'static str, mut matches: Vec<T>) -> Result<Option<T>, Error> {
    if matches.len() > 1 {
        return Err(Error::LifecycleAmbiguous {
            kind,
            matches: matches.len(),
        });
    }
    Ok(matches.pop())
}

fn exact_name(name: &OsStr) -> Result<(), Error> {
    if name.is_empty() || name.as_bytes().contains(&0) {
        return Err(Error::LifecycleInput {
            reason: "matching names must be nonempty and NUL-free",
        });
    }
    Ok(())
}

impl Server {
    /// Find an exact session name or create a session with that literal name.
    ///
    /// # Errors
    /// Returns a listing, creation or ambiguity error. Competing calls sharing
    /// this core are serialized. tmux enforces session name uniqueness across
    /// other clients; a duplicate-creation refusal triggers an exact relookup.
    pub async fn find_or_create_session(
        &self,
        name: impl Into<OsString>,
    ) -> Result<FindOrCreate<Session>, Error> {
        let name = name.into();
        exact_name(&name)?;
        if name.as_bytes().iter().any(|byte| b":.".contains(byte)) {
            return Err(Error::LifecycleInput {
                reason: "exact session names cannot contain tmux target separators",
            });
        }
        let server = self.clone();
        find_handoff(self.core.lifecycle.clone(), async move {
            let _lock = server
                .core
                .lifecycle_lock
                .acquire()
                .await
                .map_err(|error| Error::LifecycleTaskLost {
                    detail: error.to_string(),
                })?;
            if let Some(session) = one(
                "session",
                sessions_or_absent(&server)
                    .await?
                    .into_iter()
                    .filter(|s| s.name().as_bytes() == name.as_bytes())
                    .collect(),
            )? {
                return Ok(FindOrCreate::Reused(session));
            }
            match server
                .new_session(NewSessionOptions::new(name.clone()))
                .await
            {
                Ok(session) => Ok(FindOrCreate::Created(session.created_owner()?)),
                Err(Error::SessionExists { .. }) => {
                    let session = one(
                        "session",
                        server
                            .sessions()
                            .await?
                            .into_iter()
                            .filter(|s| s.name().as_bytes() == name.as_bytes())
                            .collect(),
                    )?
                    .ok_or(Error::LifecycleInput {
                        reason: "competing session disappeared before relookup",
                    })?;
                    Ok(FindOrCreate::Reused(session))
                }
                Err(error) => Err(error),
            }
        })
        .await
    }

    /// Find the daemon at this exact endpoint or start and own one.
    ///
    /// Creation uses a per-launch environment nonce to prove this call started
    /// the daemon, before changing `exit-empty` or accepting server ownership.
    /// A competing daemon is reused; only this call's bootstrap session is
    /// rolled back. One endpoint selects at most one daemon, so server
    /// ambiguity has no representation.
    ///
    /// # Errors
    /// Returns a probe/startup error or preserves bootstrap rollback failure.
    /// An unreadable initial create reply retains the unknown-result boundary.
    pub async fn find_or_create_server(&self) -> Result<FindOrCreate<Self>, Error> {
        let server = self.clone();
        find_handoff(self.core.lifecycle.clone(), async move {
            let _lock = server
                .core
                .lifecycle_lock
                .acquire()
                .await
                .map_err(|error| Error::LifecycleTaskLost {
                    detail: error.to_string(),
                })?;
            match server.generation_no_start().await {
                Ok(_) => return Ok(FindOrCreate::Reused(server.clone())),
                Err(Error::ServerGone {
                    kind: crate::ServerGoneKind::NotRunning | crate::ServerGoneKind::Unreachable,
                    ..
                }) => {}
                Err(error) => return Err(error),
            }
            let nonce = format!(
                "{}-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let startup = Server::from_core(Arc::new(server.core.startup_child(&nonce)));
            let session = match startup
                .new_session(
                    NewSessionOptions::new(format!("libtmux-start-{nonce}")).command("sleep 300"),
                )
                .await
            {
                Ok(session) => session,
                Err(operation) => return Err(startup_failure(&startup, &nonce, operation).await),
            };
            let owner = session.created_owner()?;
            let identity = owner.lease.identity;
            let generation = identity.generation;
            let result = startup
                .core
                .execute_no_start(Command::new("display-message").arg("-p").arg(format!(
                    "{} #{{LIBTMUX_LIFECYCLE_NONCE}}",
                    super::identity::FORMAT
                )))
                .await;
            let accepted = match result {
                Ok(ref result) if result.success() => {
                    result.stdout_lossy().trim_end()
                        == format!(
                            "{} {} {} {nonce}",
                            generation.pid,
                            generation.start_time,
                            identity.token()
                        )
                }
                _ => false,
            };
            if !accepted {
                return match result {
                    Ok(result) if result.success() => {
                        owner.close().await?;
                        Ok(FindOrCreate::Reused(server.clone()))
                    }
                    Ok(result) => Err(rollback_error(
                        Error::from_refused_result("display-message", &result, None),
                        &owner,
                    )
                    .await),
                    Err(error) => Err(rollback_error(error, &owner).await),
                };
            }
            let server_owner = Owned::new(
                server.clone(),
                Arc::clone(&server.core),
                identity,
                Target::Server,
            );
            if let Err(operation) = super::guarded_action(
                &startup.core,
                identity,
                "set-option -s exit-empty off",
                "set-option",
                None,
            )
            .await
            {
                return Err(rollback_error(operation, &server_owner).await);
            }
            if let Err(operation) = owner.close().await {
                return Err(rollback_error(operation, &server_owner).await);
            }
            Ok(FindOrCreate::Created(server_owner))
        })
        .await
    }
}

impl Session {
    /// Find an exact window name within this session or create that name.
    ///
    /// # Errors
    /// Returns `LifecycleAmbiguous` for duplicate names, or the listing/create
    /// error. Calls sharing this core serialize; external writers can create
    /// duplicate names. The returned created window keeps the requested name.
    pub async fn find_or_create_window(
        &self,
        name: impl Into<OsString>,
    ) -> Result<FindOrCreate<Window>, Error> {
        let name = name.into();
        exact_name(&name)?;
        let session = self.clone();
        find_handoff(self.core.lifecycle.clone(), async move {
            let _lock = session
                .core
                .lifecycle_lock
                .acquire()
                .await
                .map_err(|error| Error::LifecycleTaskLost {
                    detail: error.to_string(),
                })?;
            if let Some(window) = one(
                "window",
                session
                    .windows()
                    .await?
                    .into_iter()
                    .filter(|w| w.name().as_bytes() == name.as_bytes())
                    .collect(),
            )? {
                return Ok(FindOrCreate::Reused(window));
            }
            let window = session
                .new_window(NewWindowOptions::new(name.clone()))
                .await?;
            let owner = window.created_owner()?;
            if let Err(operation) = owner.resource().set_option("automatic-rename", "off").await {
                return Err(rollback_error(operation, &owner).await);
            }
            let mut current = owner.resource().clone();
            if let Err(operation) = current.rename(name).await {
                return Err(rollback_error(operation, &owner).await);
            }
            Ok(FindOrCreate::Created(owner))
        })
        .await
    }
}

impl Window {
    /// Find a pane by its exact user option or split and assign that identity.
    ///
    /// # Errors
    /// Returns `LifecycleAmbiguous` for duplicate identities, or a listing,
    /// split or assignment error. Assignment failures roll back the known ID.
    /// Calls sharing this core serialize. External clients can change identity
    /// options or move panes; a later call observes those changes.
    pub async fn find_or_create_pane(
        &self,
        identity: PaneIdentity,
        options: impl Into<SplitOptions>,
    ) -> Result<FindOrCreate<Pane>, Error> {
        let window = self.clone();
        let options = options.into();
        find_handoff(self.core.lifecycle.clone(), async move {
            let _lock = window
                .core
                .lifecycle_lock
                .acquire()
                .await
                .map_err(|error| Error::LifecycleTaskLost {
                    detail: error.to_string(),
                })?;
            let mut matches = Vec::new();
            for pane in window.panes().await? {
                let result = pane
                    .cmd(
                        Command::new("show-options")
                            .arg("-p")
                            .arg("-qv")
                            .arg(&identity.key),
                    )
                    .await?;
                if !result.success() {
                    return Err(Error::from_refused_result("show-options", &result, None));
                }
                let bytes = result
                    .stdout()
                    .strip_suffix(b"\n")
                    .unwrap_or(result.stdout());
                if bytes == identity.value.as_bytes() {
                    matches.push(pane);
                }
            }
            if let Some(pane) = one("pane", matches)? {
                return Ok(FindOrCreate::Reused(pane));
            }
            let owner = window.split(options).await?.created_owner()?;
            if let Err(operation) = owner
                .resource()
                .set_option(&identity.key, identity.value)
                .await
            {
                return Err(rollback_error(operation, &owner).await);
            }
            Ok(FindOrCreate::Created(owner))
        })
        .await
    }
}

async fn startup_failure(startup: &Server, nonce: &str, operation: Error) -> Error {
    // A startup nonce can establish ownership even when the initial create
    // reply did not carry a usable ID. An unrelated daemon has no such nonce.
    let answer = startup
        .core
        .execute_chain_no_start(
            crate::CommandChain::new(match super::identity::initialize() {
                Ok(command) => command,
                Err(cleanup) => {
                    return Error::AcquisitionRollback {
                        operation: Box::new(operation),
                        cleanup: Box::new(cleanup),
                    };
                }
            })
            .then(Command::new("display-message").arg("-p").arg(format!(
                "{} #{{LIBTMUX_LIFECYCLE_NONCE}}",
                super::identity::FORMAT
            ))),
        )
        .await;
    if let Ok(answer) = answer {
        if answer.success() {
            if let Some(identity) = answer
                .stdout_lossy()
                .trim_end()
                .strip_suffix(&format!(" {nonce}"))
            {
                if let Ok(identity) = super::identity::parse(identity.as_bytes()) {
                    return match super::guarded_kill(&startup.core, identity, &Target::Server).await
                    {
                        Ok(()) => operation.after_effect("find-or-create-server"),
                        Err(cleanup) => Error::AcquisitionRollback {
                            operation: Box::new(operation),
                            cleanup: Box::new(cleanup),
                        },
                    };
                }
            }
        }
    }
    Error::UnknownCreation {
        command: "find-or-create-server",
        source: Box::new(operation),
    }
}

async fn sessions_or_absent(server: &Server) -> Result<Vec<Session>, Error> {
    match server.sessions().await {
        Ok(sessions) => Ok(sessions),
        Err(Error::ServerGone {
            kind: crate::ServerGoneKind::NotRunning | crate::ServerGoneKind::Unreachable,
            ..
        }) => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

async fn rollback_error<T>(operation: Error, owner: &Owned<T>) -> Error {
    match owner.close().await {
        Ok(()) => operation.after_effect("find-or-create"),
        Err(cleanup) => Error::AcquisitionRollback {
            operation: Box::new(operation),
            cleanup: Box::new(cleanup),
        },
    }
}

async fn find_handoff<T: Clone + Send + Sync + 'static>(
    journal: super::jobs::Journal,
    create: impl Future<Output = Result<FindOrCreate<T>, Error>> + Send + 'static,
) -> Result<FindOrCreate<T>, Error> {
    // The scope handoff owns the result until the awaiting caller accepts it.
    // Reused handles have no remote cleanup action.
    scoped::handoff(journal, create, |result| async move {
        if let FindOrCreate::Created(owner) = result {
            owner.close().await?;
        }
        Ok(())
    })
    .await
}
