//! Client handles and their snapshot getters.

use std::ffi::OsString;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::os::unix::ffi::OsStringExt as _;
use std::sync::Arc;

use crate::formats::TmuxText;
use crate::internal::core::Core;
use crate::internal::listing;
#[cfg(feature = "query")]
use crate::query::{FilterSchema, Filterable};
#[cfg(feature = "query")]
use crate::snapshot::ClientFields;
use crate::snapshot::ClientInfo;
use crate::target::ServerIdentity;
use crate::{Command, Error, ObjectKind};

/// One client attached to the tmux server.
///
/// Clients have no `$`-style id. tmux identifies them by the terminal they
/// occupy, so [`Client::name`] is the identity and is always present.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// let guard = libtmux::test::TestServer::new().await?;
/// guard.server().new_session("work").await?;
///
/// // A session created without attaching has no client, which is the usual
/// // shape under test and under automation.
/// assert!(guard.server().clients().await?.is_empty());
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Client {
    core: Arc<Core>,
    info: ClientInfo,
}

impl Client {
    /// Build a handle from a hydrated snapshot.
    pub(crate) const fn new(core: Arc<Core>, info: ClientInfo) -> Self {
        Self { core, info }
    }

    /// Return the client name, which is its terminal path.
    #[must_use]
    pub const fn name(&self) -> &TmuxText {
        self.info.client_name()
    }

    /// Return the client's controlling terminal.
    #[must_use]
    pub fn tty(&self) -> &TmuxText {
        self.info.client_tty()
    }

    /// Return the terminal type the client reports.
    #[must_use]
    pub fn term_name(&self) -> &TmuxText {
        self.info.client_termname()
    }

    /// Return the client process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        *self.info.client_pid()
    }

    /// Return the client width in cells.
    #[must_use]
    pub fn width(&self) -> u32 {
        *self.info.client_width()
    }

    /// Return the client height in cells.
    #[must_use]
    pub fn height(&self) -> Option<u32> {
        self.info.client_height().copied().available()
    }

    /// Return when the client connected, as a Unix timestamp.
    #[must_use]
    pub fn created(&self) -> i64 {
        *self.info.client_created()
    }

    /// Report whether the client is attached read-only.
    #[must_use]
    pub fn is_readonly(&self) -> bool {
        *self.info.client_readonly()
    }

    /// Report whether the client is a control-mode client.
    ///
    /// Control-mode clients speak tmux's machine protocol rather than drawing
    /// a terminal, so they are usually other programs rather than people.
    #[must_use]
    pub fn is_control_mode(&self) -> bool {
        *self.info.client_control_mode()
    }

    /// Return the identity of the server this client is attached to.
    pub(crate) fn server_identity(&self) -> &ServerIdentity {
        self.core.configuration().identity()
    }

    /// Replace this handle's snapshot with the client's current state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ObjectGone`] when the client has detached, and
    /// [`Error::ClientSuspended`] when it is stopped rather than gone --
    /// suspended or locked, which tmux leaves out of the same listing. Keep
    /// the handle for the second: the client is listed again once it resumes.
    /// Returns a listing error when tmux could not be read.
    pub async fn refresh(&mut self) -> Result<&mut Self, Error> {
        let listed = listing::clients(&self.core, None)
            .await?
            .into_iter()
            .find(|info| info.client_name() == self.name());

        let Some(info) = listed else {
            return Err(self.why_unlisted().await);
        };

        self.info = info;
        Ok(self)
    }

    /// Say why this client is not in the listing.
    ///
    /// `list-clients` leaves out the dead, the exiting and the suspended
    /// alike, so absence from it does not say which happened. A suspended
    /// client is still resolvable as a command target and reports itself,
    /// which is what tells the temporary case from the permanent one.
    ///
    /// Only the miss path pays for this, and only tmux's own answer is
    /// trusted: anything that does not resolve to this client by name, or
    /// resolves without saying it is suspended, is reported gone exactly as
    /// before. The probe cannot turn a gone client into a live one, only a
    /// suspended one into something other than gone.
    async fn why_unlisted(&self) -> Error {
        let gone = || Error::ObjectGone {
            kind: ObjectKind::Client,
            id: self.name().to_string_lossy().into_owned(),
        };

        // Flags first: they are a comma-separated list of known words and so
        // never hold the separator, while a client name is a terminal path
        // and is not this crate's to constrain. Splitting once puts every
        // ambiguous byte on the far side.
        let Ok(result) = self
            .core
            .execute(
                Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(OsString::from_vec(self.name().as_bytes().to_vec()))
                    .arg(OsString::from("#{client_flags}|#{client_name}")),
            )
            .await
        else {
            return gone();
        };
        if !result.success() {
            return gone();
        }

        let stdout = result.stdout();
        let answer = stdout.strip_suffix(b"\n").unwrap_or(stdout);
        let Some(separator) = answer.iter().position(|byte| *byte == b'|') else {
            return gone();
        };
        let (flags, separated) = answer.split_at(separator);
        // Past the separator itself. `split_at` puts it at the head of the
        // second half, so this is never out of range; asking rather than
        // indexing keeps that true if the split ever moves.
        let Some(name) = separated.get(1..) else {
            return gone();
        };
        if name != self.name().as_bytes() {
            return gone();
        }

        // `display-message` is allowed to fail its target, so an absent client
        // expands every format empty rather than erroring. That makes the flag
        // the whole signal: no flag, no claim.
        if flags
            .split(|byte| *byte == b',')
            .any(|flag| flag == b"suspended")
        {
            return Error::ClientSuspended {
                name: self.name().to_string_lossy().into_owned(),
            };
        }
        gone()
    }

    /// Return a new handle holding the client's current state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ObjectGone`] when the client has detached, and
    /// [`Error::ClientSuspended`] when it is stopped rather than gone. The
    /// two are worth telling apart: [`Error::is_object_gone`] answers false
    /// for the second, because the same handle works again once the client
    /// resumes. Returns a listing error when tmux could not be read.
    pub async fn refreshed(&self) -> Result<Self, Error> {
        let mut refreshed = self.clone();
        refreshed.refresh().await?;
        Ok(refreshed)
    }

    /// Read one ID out of this client's format tree.
    ///
    /// tmux resolves a client target to the session it is attached to, that
    /// session's current window, and that window's active pane, so one
    /// `display-message` answers any of the three. `None` means the client is
    /// attached to nothing.
    ///
    /// Targeted with `-t` rather than `-c`, and that is not a preference.
    /// `display-message`'s option string is `acd:INpt:F:v` on 3.2a, where `c`
    /// carries no colon and so takes no argument: a client name after it
    /// becomes a positional argument, the command exceeds its one-argument
    /// maximum, and tmux answers with a usage error while every format
    /// expands empty. `-c` gained its argument in 3.5a. The usage line on
    /// 3.2a advertises `[-c target-client]` anyway, on the line after the
    /// option string that refuses it, so the error quotes the flag it just
    /// rejected and reads as a typo rather than as a disagreement between
    /// tmux's help and its parser.
    ///
    /// `-t` resolves a client on every supported release. Verified to report
    /// the target rather than the caller, which the obvious probe cannot
    /// show: the asking process and the client usually share a `TERM`, so a
    /// wrong answer is byte-identical to a right one. With the client
    /// attached as `screen-256color` and the caller at `vt100`, 3.2a and
    /// 3.7c both answer `screen-256color`.
    async fn attached_id(&self, format: &str) -> Result<Option<TmuxText>, Error> {
        let result = self
            .core
            .execute(
                Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(OsString::from_vec(self.name().as_bytes().to_vec()))
                    .arg(OsString::from(format)),
            )
            .await?;
        if !result.success() {
            let stderr = result.stderr_lossy();
            if stderr.trim_end() == crate::error::NO_CURRENT_CLIENT {
                return Ok(None);
            }
            return Err(Error::from_refused_result("display-message", &result, None));
        }

        let stdout = result.stdout();
        let value = stdout.strip_suffix(b"\n").unwrap_or(stdout);
        if value.is_empty() {
            return Ok(None);
        }

        Ok(Some(TmuxText::from(value.to_vec())))
    }

    /// The session this client is attached to.
    ///
    /// `None` when the client is attached to nothing, which is an ordinary
    /// state rather than a failure.
    ///
    /// Resolved through `#{session_id}` rather than `#{client_session}`. The
    /// latter is what tmux calls the attachment, but it is a *name*, and a
    /// name is not a handle: tmux will create a session called `a:b` and then
    /// refuse to address it, because `:` separates a session from a window in
    /// a target. The ID is unambiguous by construction.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached, or answers with an ID
    /// this crate cannot parse.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let server = guard.server();
    /// let session = server.new_session("work").await?;
    ///
    /// // A control-mode connection is a client, so the server has one to find.
    /// # #[cfg(feature = "control-mode")]
    /// # {
    /// let control = libtmux::control::ControlMode::attach(server, session.id()).await?;
    /// let client = server.clients().await?.into_iter().next().expect("one client");
    ///
    /// let attached = client.attached_session().await?.expect("it is attached");
    /// assert_eq!(attached.id(), session.id());
    /// control.shutdown().await?;
    /// # }
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn attached_session(&self) -> Result<Option<crate::Session>, Error> {
        let Some(id) = self.attached_id("#{session_id}").await? else {
            return Ok(None);
        };
        let id: crate::SessionId =
            id.to_string_lossy()
                .parse()
                .map_err(|detail| Error::UnreadableFormatValue {
                    format: "#{session_id}",
                    detail,
                })?;
        crate::Server::from_core(Arc::clone(&self.core))
            .session_by_id(&id)
            .await
    }

    /// The current window of the session this client is attached to.
    ///
    /// Not this client's own view. tmux keeps the current window on the
    /// session -- `curw` is a member of `struct session`, not of
    /// `struct client` -- so every client attached to one session reports the
    /// same window, and one client changing it changes it for all of them.
    ///
    /// `None` when the client is attached to nothing.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached, or answers with an ID
    /// this crate cannot parse.
    pub async fn attached_window(&self) -> Result<Option<crate::Window>, Error> {
        let Some(id) = self.attached_id("#{window_id}").await? else {
            return Ok(None);
        };
        let id: crate::WindowId =
            id.to_string_lossy()
                .parse()
                .map_err(|detail| Error::UnreadableFormatValue {
                    format: "#{window_id}",
                    detail,
                })?;
        crate::Server::from_core(Arc::clone(&self.core))
            .window_by_id(&id)
            .await
    }

    /// The active pane of the current window of this client's session.
    ///
    /// Shares the caveat on [`Self::attached_window`], and adds one: this is
    /// the window's active pane, not a per-client focus, because tmux does
    /// not keep one. Two clients on the same session always report the same
    /// pane.
    ///
    /// `None` when the client is attached to nothing.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached, or answers with an ID
    /// this crate cannot parse.
    pub async fn attached_pane(&self) -> Result<Option<crate::Pane>, Error> {
        let Some(id) = self.attached_id("#{pane_id}").await? else {
            return Ok(None);
        };
        let id: crate::PaneId =
            id.to_string_lossy()
                .parse()
                .map_err(|detail| Error::UnreadableFormatValue {
                    format: "#{pane_id}",
                    detail,
                })?;
        crate::Server::from_core(Arc::clone(&self.core))
            .pane_by_id(&id)
            .await
    }

    /// Detach this client from its server.
    ///
    /// This consumes the handle: the client is gone once it detaches.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn detach(self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "detach-client",
            Command::new("detach-client")
                .arg("-t")
                .arg(self.name().to_string_lossy().into_owned()),
        )
        .await
    }

    /// Suspend this client, as if its user pressed the suspend key.
    ///
    /// The handle survives, and deliberately: unlike [`Self::detach`], this
    /// stops the client rather than ending it. tmux stops listing a suspended
    /// client, so [`Server::clients`] will not show it and the session stops
    /// counting it as attached, but it is still there. It returns when its
    /// process continues, which for a suspended client is `SIGCONT`.
    ///
    /// [`Server::clients`]: crate::Server::clients
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command. Reading through the
    /// handle afterwards gives [`Error::ClientSuspended`] rather than
    /// [`Error::ObjectGone`], so a caller can tell this from a client that
    /// left.
    pub async fn suspend(&self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "suspend-client",
            Command::new("suspend-client")
                .arg("-t")
                .arg(self.name().to_string_lossy().into_owned()),
        )
        .await
    }

    /// Lock this client's terminal.
    ///
    /// Runs the `lock-command` option, which is `lock -np` unless the server
    /// was told otherwise. [`crate::Session::lock`] locks every client
    /// attached to one session and [`crate::Server::lock_all`] locks every
    /// client on the server; this locks exactly one.
    ///
    /// tmux marks a locked client with the flag it uses for a suspended one,
    /// so a locked client leaves [`crate::Server::clients`] the same way and
    /// comes back when its `lock-command` exits. A server with
    /// `lock-after-time` set reaches that state with nobody asking.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command. Reading through the
    /// handle while the client is locked gives [`Error::ClientSuspended`],
    /// not [`Error::ObjectGone`].
    pub async fn lock(&self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "lock-client",
            Command::new("lock-client")
                .arg("-t")
                .arg(self.name().to_string_lossy().into_owned()),
        )
        .await
    }

    /// Redraw this client's terminal.
    ///
    /// This is tmux's `refresh-client`. It is named `redraw` because
    /// `refresh` means "re-read the snapshot" on every handle in this crate,
    /// and these do different things.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn redraw(&self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "refresh-client",
            Command::new("refresh-client")
                .arg("-t")
                .arg(self.name().to_string_lossy().into_owned()),
        )
        .await
    }

    /// Point this client at another session.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the session belongs to another
    /// server, or an error when the session does not exist or tmux refuses.
    pub async fn switch_to(&self, session: &crate::Session) -> Result<(), Error> {
        self.core
            .require_same_server(session.server_identity(), "switch-client")?;
        listing::mutate(
            &self.core,
            "switch-client",
            Command::new("switch-client")
                .arg("-c")
                .arg(self.name().to_string_lossy().into_owned())
                .arg("-t")
                .arg(session.id().to_string()),
        )
        .await
    }
}

/// Clients compare by server endpoint and client name.
impl PartialEq for Client {
    fn eq(&self, other: &Self) -> bool {
        self.server_identity() == other.server_identity() && self.name() == other.name()
    }
}

impl Eq for Client {}

impl Hash for Client {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.server_identity().hash(state);
        self.name().hash(state);
    }
}

/// Renders nothing but the type.
///
/// A client name is a terminal path, which identifies the user's machine, so
/// it stays out of diagnostics like every other snapshot value.
impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Client").finish_non_exhaustive()
    }
}

/// Filtering a client uses the same handles as the snapshot beneath it.
///
/// Matching and validation delegate to that snapshot, so an expression can
/// only name fields the catalog knows. The companion is re-parameterized to
/// [`Client`] so the type a listing returns is the type an expression
/// filters.
#[cfg(feature = "query")]
impl Filterable for Client {
    type Fields = ClientFields<Self>;

    const FILTER_TARGET: &'static str = <ClientInfo as Filterable>::FILTER_TARGET;

    fn filter_fields() -> Self::Fields {
        Self::Fields::for_target(Self::FILTER_TARGET)
    }

    fn __filter_matches(&self, predicate: &crate::query::__private::Predicate) -> bool {
        self.info.__filter_matches(predicate)
    }

    fn __filter_validate(
        predicate: &crate::query::__private::Predicate,
    ) -> Result<(), crate::query::FilterExpressionError> {
        <ClientInfo as Filterable>::__filter_validate(predicate)
    }
}

#[cfg(feature = "query")]
impl FilterSchema for Client {
    fn __filter_schema() -> crate::query::__private::FilterSchemaDescriptor {
        <ClientInfo as FilterSchema>::__filter_schema()
    }
}

/// Renders the client name, which is its terminal path.
impl fmt::Display for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name().to_string_lossy())
    }
}
