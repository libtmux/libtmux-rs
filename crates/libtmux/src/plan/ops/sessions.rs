//! Operations that make or address a session.

use std::ffi::OsString;

use super::SESSION_FORMAT;
use super::{Effects, Op, Operation, Safety, Scope, SessionSlot, Slot};
use crate::Command;

/// Create a session.
///
/// # Examples
///
/// ```
/// use libtmux::plan::{NewSession, Plan};
///
/// let mut plan = Plan::new();
/// let session = plan.add(NewSession::new("work"));
/// // The session, its first window, and that window's pane all come from one
/// // command, so none of them costs a second round trip.
/// let _ = (session, session.window(), session.pane());
/// ```
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NewSession {
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::argument",
            deserialize_with = "crate::plan::wire::parse_argument"
        )
    )]
    #[cfg_attr(feature = "schema", schemars(with = "crate::plan::wire::Argument"))]
    name: OsString,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::optional_argument",
            deserialize_with = "crate::plan::wire::parse_optional_argument"
        )
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<crate::plan::wire::Argument>")
    )]
    start_directory: Option<OsString>,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::optional_argument",
            deserialize_with = "crate::plan::wire::parse_optional_argument"
        )
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<crate::plan::wire::Argument>")
    )]
    window_name: Option<OsString>,
}

impl NewSession {
    /// Create a detached session with this name.
    #[must_use]
    pub fn new(name: impl Into<OsString>) -> Self {
        Self {
            name: name.into(),
            start_directory: None,
            window_name: None,
        }
    }

    /// Start the session's first window in this directory.
    ///
    /// tmux expands this as a format, so [`crate::escape_format`] belongs
    /// around text a program did not write.
    #[must_use]
    pub fn start_directory(mut self, directory: impl Into<OsString>) -> Self {
        self.start_directory = Some(directory.into());
        self
    }

    /// Name the session's first window.
    #[must_use]
    pub fn window_name(mut self, name: impl Into<OsString>) -> Self {
        self.window_name = Some(name.into());
        self
    }

    pub(crate) fn render(&self) -> Command {
        let mut command = Command::new("new-session")
            .arg("-d")
            .arg("-P")
            .arg("-F")
            .arg(SESSION_FORMAT)
            .arg("-s")
            .arg(self.name.clone());
        if let Some(directory) = &self.start_directory {
            command = command.arg("-c").arg(directory.clone());
        }
        if let Some(name) = &self.window_name {
            command = command.arg("-n").arg(name.clone());
        }
        command
    }
}

operation!(
    NewSession,
    creates = Slot<SessionSlot>,
    effects = Effects {
        creates: Some(Scope::Session),
        ..Effects::MUTATING
    },
    safety = Safety::Mutating
);
