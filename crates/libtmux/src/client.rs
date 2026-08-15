//! Client handles and their snapshot getters.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::formats::TmuxText;
use crate::internal::core::Core;
use crate::internal::listing;
#[cfg(feature = "query")]
use crate::query::Filterable;
#[cfg(feature = "query")]
use crate::snapshot::ClientFields;
use crate::snapshot::ClientInfo;
use crate::target::ServerIdentity;
use crate::{Command, Error, ObjectKind};

/// One client attached to the tmux server.
///
/// Clients have no `$`-style id. tmux identifies them by the terminal they
/// occupy, so [`Client::name`] is the identity and is always present.
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
    /// Returns [`Error::ObjectGone`] when the client has detached, or a
    /// listing error when tmux could not be read.
    pub async fn refresh(&mut self) -> Result<&mut Self, Error> {
        let info = listing::clients(&self.core, None)
            .await?
            .into_iter()
            .find(|info| info.client_name() == self.name())
            .ok_or_else(|| Error::ObjectGone {
                kind: ObjectKind::Client,
                id: self.name().to_string_lossy().into_owned(),
            })?;

        self.info = info;
        Ok(self)
    }

    /// Return a new handle holding the client's current state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ObjectGone`] when the client has detached, or a
    /// listing error when tmux could not be read.
    pub async fn refreshed(&self) -> Result<Self, Error> {
        let mut refreshed = self.clone();
        refreshed.refresh().await?;
        Ok(refreshed)
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
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
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
    /// Returns an error when the session does not exist or tmux refuses.
    pub async fn switch_to(&self, session: &crate::Session) -> Result<(), Error> {
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

/// Renders the client name, which is its terminal path.
impl fmt::Display for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name().to_string_lossy())
    }
}
