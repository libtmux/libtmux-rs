//! Operations that set tmux options.

use std::ffi::OsString;
use std::fmt;

use super::Resolver;
use super::{Chainable, Effects, Op, Operation, Safety, SessionTarget, WindowTarget};
use crate::Command;
use crate::plan::SlotUse;

/// Set a tmux option.
#[derive(Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SetOption {
    scope: OptionScope,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::argument",
            deserialize_with = "crate::plan::wire::parse_argument"
        )
    )]
    name: OsString,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::argument",
            deserialize_with = "crate::plan::wire::parse_argument"
        )
    )]
    value: OsString,
}

/// Where a [`SetOption`] applies.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
enum OptionScope {
    Global,
    Session(SessionTarget),
    Window(WindowTarget),
}

impl SetOption {
    /// Set a server-global option.
    #[must_use]
    pub fn global(name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        Self {
            scope: OptionScope::Global,
            name: name.into(),
            value: value.into(),
        }
    }

    /// Set an option on one session.
    #[must_use]
    pub fn session(
        target: impl Into<SessionTarget>,
        name: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        Self {
            scope: OptionScope::Session(target.into()),
            name: name.into(),
            value: value.into(),
        }
    }

    /// Set an option on one window.
    #[must_use]
    pub fn window(
        target: impl Into<WindowTarget>,
        name: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        Self {
            scope: OptionScope::Window(target.into()),
            name: name.into(),
            value: value.into(),
        }
    }

    pub(in crate::plan) fn target(&self) -> Option<SlotUse> {
        match &self.scope {
            OptionScope::Global => None,
            OptionScope::Session(target) => target.slot(),
            OptionScope::Window(target) => target.slot(),
        }
    }

    pub(crate) fn render(&self, resolve: Resolver<'_>) -> Option<Command> {
        let command = match &self.scope {
            OptionScope::Global => Command::new("set-option").arg("-g"),
            OptionScope::Session(target) => Command::new("set-option")
                .arg("-t")
                .arg(target.token(resolve)?),
            OptionScope::Window(target) => Command::new("set-option")
                .arg("-w")
                .arg("-t")
                .arg(target.token(resolve)?),
        };
        Some(
            command
                .arg("--")
                .arg(self.name.clone())
                .sensitive_arg(self.value.clone()),
        )
    }
}

impl fmt::Debug for SetOption {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SetOption")
            .field("scope", &self.scope)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

operation!(
    SetOption,
    creates = (),
    effects = Effects {
        idempotent: true,
        ..Effects::MUTATING
    },
    safety = Safety::Mutating,
    chainable
);

/// Set a variable in a session's environment.
///
/// Applied before anything runs in the session, so a command started later
/// sees the environment the caller described rather than the one tmux
/// happened to inherit.
#[derive(Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SetEnvironment {
    pub(crate) target: SessionTarget,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::argument",
            deserialize_with = "crate::plan::wire::parse_argument"
        )
    )]
    name: OsString,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::argument",
            deserialize_with = "crate::plan::wire::parse_argument"
        )
    )]
    value: OsString,
}

impl SetEnvironment {
    /// Set `name` in this session's environment.
    #[must_use]
    pub fn new(
        target: impl Into<SessionTarget>,
        name: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        Self {
            target: target.into(),
            name: name.into(),
            value: value.into(),
        }
    }

    pub(crate) fn render(&self, resolve: Resolver<'_>) -> Option<Command> {
        Some(
            Command::new("set-environment")
                .arg("-t")
                .arg(self.target.token(resolve)?)
                .arg("--")
                .arg(self.name.clone())
                .sensitive_arg(self.value.clone()),
        )
    }
}

impl fmt::Debug for SetEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SetEnvironment")
            .field("target", &self.target)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

operation!(
    SetEnvironment,
    creates = (),
    effects = Effects {
        idempotent: true,
        ..Effects::MUTATING
    },
    safety = Safety::Mutating,
    chainable
);
